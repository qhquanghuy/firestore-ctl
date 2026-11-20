//! Dynamic shell completion (`COMPLETE=<shell> fsq`): collection ids and field names are
//! looked up in the database being addressed while the user presses TAB.
//!
//! Completion must never get in the way, so every completer here is best effort:
//! - it returns no candidates on any failure (no project, no credentials, timeout, ...);
//! - it prints nothing itself (stdout is the completion protocol);
//! - it only reads Firestore, and its only side effect is writing the cache files below;
//! - it gives up after [`DEADLINE`], connecting included.
//!
//! The database and collection come from the words already typed (`--project`,
//! `--database`, the collection argument), then from `FSQ_PROJECT` / `FSQ_DATABASE`.
//! Answers are cached for [`CACHE_TTL`] under
//! `$XDG_CACHE_HOME/fsq/<project>/<database>/` (`~/.cache` when unset).

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fs;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{ArgMatches, CommandFactory};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use itertools::Itertools;
use serde_json::json;

use crate::cli::{Cli, CompletionShell, ConnArgs};
use crate::domain::{
    AnyPath, CollectionId, DbTarget, Document, FieldPath, Fields, Limit, Op, Query, Scope, Value,
};
use crate::grpc;
use crate::parse::unquoted;
use crate::port::Firestore;

/// Everything a completer does, connecting included, must finish within this.
const DEADLINE: Duration = Duration::from_secs(2);
/// How long a cached answer is trusted.
const CACHE_TTL: Duration = Duration::from_mins(10);
/// Documents sampled to discover field names.
const SAMPLE_SIZE: NonZeroU32 = NonZeroU32::new(20).unwrap();
/// Deepest nesting offered: `a.b.c` has depth 3.
const MAX_FIELD_DEPTH: usize = 3;

/// The line that enables completion in `shell`; what `fsq completions <shell>` prints.
#[must_use]
pub fn registration_line(shell: CompletionShell) -> String {
    format!("source <(COMPLETE={} fsq)", shell.name())
}

/// Completer for collection ids (the collection argument, `--group`).
#[must_use]
pub fn collection_completer() -> ArgValueCompleter {
    ArgValueCompleter::new(|current: &OsStr| {
        silently(|| {
            let current = current.to_str()?;
            Some(collection_candidates(
                &Invocation::from_process(),
                current,
                SystemTime::now(),
            ))
        })
    })
}

/// Completer for the field part of `--where`, `--order-by`, `--select`, `--sum`, `--avg`
/// and `--set`: field names sampled from the collection already typed on the command line.
#[must_use]
pub fn field_completer() -> ArgValueCompleter {
    ArgValueCompleter::new(|current: &OsStr| {
        silently(|| {
            let current = current.to_str()?;
            Some(field_candidates(
                &Invocation::from_process(),
                current,
                SystemTime::now(),
            ))
        })
    })
}

/// The candidates `complete` finds, or none if it finds nothing or panics. A panic
/// (say, inside a dependency) must not print its message: stderr is the user's terminal
/// in the middle of typing. The process exits right after completing, so the silenced
/// panic hook is not restored.
fn silently(
    complete: impl FnOnce() -> Option<Vec<CompletionCandidate>>,
) -> Vec<CompletionCandidate> {
    std::panic::set_hook(Box::new(|_| {}));
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(complete))
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// The names of the fields in `docs`: every key, and the keys of nested maps (as
/// dotted paths) down to depth [`MAX_FIELD_DEPTH`]. Arrays are not descended into.
#[must_use]
pub fn field_names(docs: &[Document]) -> BTreeSet<FieldPath> {
    docs.iter()
        .flat_map(|doc| nested_keys(&[], &doc.data))
        .filter_map(|segments| FieldPath::from_segments(segments).ok())
        .collect()
}

/// The path of every key of `fields` (below `prefix`) and of every nested map's keys.
fn nested_keys(prefix: &[String], fields: &Fields) -> Vec<Vec<String>> {
    fields
        .iter()
        .flat_map(|(key, value)| {
            let path = prefix.iter().cloned().chain([key.clone()]).collect_vec();
            let below = match value {
                Value::Map(inner) if path.len() < MAX_FIELD_DEPTH => nested_keys(&path, inner),
                _ => Vec::new(),
            };
            std::iter::once(path).chain(below)
        })
        .collect()
}

// ---- what the command line typed so far tells us ----

/// What is known about the command being completed.
#[derive(Debug)]
struct Invocation {
    /// The database to ask; `None` when no project is given anywhere.
    target: Option<DbTarget>,
    /// The collection(s) the command reads or writes, once typed.
    scope: Option<Scope>,
    /// Where the cache lives; `None` when there is no usable home.
    cache_base: Option<PathBuf>,
    /// The cursor word is the tail of one the shell cut at `:` or `=`, see [`Typed::detached`].
    detached: bool,
}

impl Invocation {
    /// From the process's own arguments and environment, as set up by the shell script.
    fn from_process() -> Self {
        let index = std::env::var("_CLAP_COMPLETE_INDEX")
            .ok()
            .and_then(|i| i.parse().ok());
        let typed = typed_words(
            std::env::args_os().map(|a| a.to_string_lossy().into_owned()),
            index,
        );
        Self {
            detached: typed.detached,
            ..Self::new(&typed.words, &|name| std::env::var(name).ok())
        }
    }

    /// From the typed `words` (program name first; see [`Typed::words`]) and an
    /// environment lookup.
    fn new(words: &[String], env: &impl Fn(&str) -> Option<String>) -> Self {
        let matches = lenient_command().try_get_matches_from(words).ok();
        let conn = ConnArgs {
            project: matches.as_ref().and_then(|m| parsed(m, "project")),
            database: matches.as_ref().and_then(|m| parsed(m, "database")),
        };
        Self {
            target: conn.target(env).ok().flatten(),
            scope: matches.as_ref().and_then(scope_of),
            cache_base: cache_base(env),
            detached: false,
        }
    }

    fn cache(&self) -> Option<Cache> {
        let (base, target) = self.cache_base.as_ref().zip(self.target.as_ref())?;
        Some(Cache::new(base, target, CACHE_TTL))
    }
}

/// The words of the line being completed, as the command parser should see them.
#[derive(Debug, PartialEq, Eq)]
struct Typed {
    /// The words after the program name's `--` marker, with the word at the cursor
    /// replaced by an empty one (so that a flag before it still gets its value and the
    /// words after the cursor are still read).
    words: Vec<String>,
    /// Whether the shell cut the cursor word off the word before it at a `:` or `=`
    /// (bash breaks words at those characters: `--order-by age:d` arrives as `age`, `:`,
    /// `d`). The command parser then takes the tail for an argument of its own, so it
    /// is no collection or field name.
    detached: bool,
}

/// The words the shell passed after `--` (the whole line, words after the cursor
/// included); `index` is the position of the word being completed.
fn typed_words(args: impl Iterator<Item = String>, index: Option<usize>) -> Typed {
    let raw = args.skip_while(|a| a != "--").skip(1).collect_vec();
    let detached = index
        .and_then(|i| raw.get(i.checked_sub(1)?))
        .is_some_and(|word| word == ":" || word == "=");
    let words = raw
        .into_iter()
        .enumerate()
        .map(|(i, word)| {
            if Some(i) == index {
                String::new()
            } else {
                word
            }
        })
        .collect_vec();
    Typed {
        words: rejoin_split_words(words),
        detached,
    }
}

/// Undoes bash's splitting of `--project=p` into `--project`, `=`, `p` (and likewise
/// at `:`) by gluing a lone `=` or `:` and the word after it to the word before.
fn rejoin_split_words(words: Vec<String>) -> Vec<String> {
    words
        .into_iter()
        .fold(
            (Vec::<String>::new(), false),
            |(mut joined, glue_next), word| {
                let is_break = word == "=" || word == ":";
                match joined.last_mut() {
                    Some(last) if is_break || glue_next => {
                        last.push_str(&word);
                        (joined, is_break)
                    }
                    _ => {
                        joined.push(word);
                        (joined, false)
                    }
                }
            },
        )
        .0
}

/// The command line grammar with every value left as text and no complaints about
/// missing or malformed arguments: the line being completed is unfinished, and one
/// bad word must not hide the rest.
fn lenient_command() -> clap::Command {
    fn as_text(cmd: clap::Command) -> clap::Command {
        cmd.mut_args(|arg| {
            if arg.get_action().takes_values() {
                arg.value_parser(clap::value_parser!(String))
            } else {
                arg
            }
        })
        .mut_subcommands(as_text)
    }
    as_text(Cli::command()).ignore_errors(true)
}

/// The value of `id` (a flag of the command or of its subcommand) if it parses as a `T`.
fn parsed<T: FromStr>(matches: &ArgMatches, id: &str) -> Option<T> {
    let text = |m: &ArgMatches| m.try_get_one::<String>(id).ok().flatten().cloned();
    text(matches)
        .or_else(|| matches.subcommand().and_then(|(_, sub)| text(sub)))
        .and_then(|text| text.parse().ok())
}

/// The collection(s) a typed command line addresses: the collection or `--group` of
/// `query` / `count`, the collection holding the document of `get` / `set` / ...
fn scope_of(matches: &ArgMatches) -> Option<Scope> {
    let (_, sub) = matches.subcommand()?;
    let group = parsed::<CollectionId>(sub, "group");
    match (group, parsed::<AnyPath>(sub, "path")) {
        (None, Some(AnyPath::Collection(c))) => Some(Scope::Collection(c)),
        (None, Some(AnyPath::Doc(doc))) => Some(Scope::Collection(doc.collection().clone())),
        (Some(id), None) => Some(Scope::Group { parent: None, id }),
        (Some(id), Some(AnyPath::Doc(parent))) => Some(Scope::Group {
            parent: Some(parent),
            id,
        }),
        (Some(_), Some(AnyPath::Collection(_))) | (None, None) => None,
    }
}

// ---- candidates ----

fn collection_candidates(
    inv: &Invocation,
    current: &str,
    now: SystemTime,
) -> Vec<CompletionCandidate> {
    let (false, Some(target)) = (inv.detached, &inv.target) else {
        return Vec::new();
    };
    cached(inv.cache().as_ref(), "collections", now, || {
        fetch_collections(target)
    })
    .into_iter()
    .filter(|id| id.starts_with(current))
    .map(CompletionCandidate::new)
    .collect()
}

/// Candidates for the word `current` of a field-taking flag. zsh hands over the word with
/// the quote it was opened with (`'age >`); the quote is ignored, and left out of the
/// candidates: the shell matches them against the text inside the quote and keeps it open.
fn field_candidates(inv: &Invocation, current: &str, now: SystemTime) -> Vec<CompletionCandidate> {
    if inv.detached {
        return Vec::new();
    }
    let word = current.trim_start_matches(['\'', '"']);
    let candidates = match field_position(word) {
        FieldPosition::Literal(candidates) => candidates,
        FieldPosition::Field { kept, typed } => inv
            .target
            .as_ref()
            .zip(inv.scope.as_ref())
            .map(|(target, scope)| {
                cached(inv.cache().as_ref(), &fields_cache_name(scope), now, || {
                    fetch_fields(target, scope)
                })
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|field| field.starts_with(typed))
            .map(|field| format!("{kept}{field}"))
            .collect(),
        FieldPosition::Nothing => Vec::new(),
    };
    candidates
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}

/// The sort directions starting with `typed`.
fn directions(typed: &str) -> impl Iterator<Item = String> + '_ {
    ["asc", "desc"]
        .into_iter()
        .filter(move |direction| direction.starts_with(typed))
        .map(str::to_owned)
}

/// What the word being completed asks for.
#[derive(Debug, PartialEq, Eq)]
enum FieldPosition<'a> {
    /// A field name starting with `typed`, to be preceded by `kept` (the earlier items of
    /// a `--select a,b,` list).
    Field { kept: &'a str, typed: &'a str },
    /// Complete candidates that need no lookup: operators or sort directions.
    Literal(Vec<String>),
    /// Past the field part (a value, say), or not a field at all.
    Nothing,
}

/// Characters that end an unquoted field name (besides whitespace).
const FIELD_BREAKS: &str = "=<>!+-:,";

fn ends_field(c: char) -> bool {
    c.is_whitespace() || FIELD_BREAKS.contains(c)
}

/// Where in a field-taking flag value (`age`, `a,b`, `age >`, `name:`, `tags+=x`) the
/// cursor is. Only the field part is completed; after it, `--where` gets operators and
/// `--order-by` gets `asc`/`desc`, and values get nothing.
fn field_position(current: &str) -> FieldPosition<'_> {
    let last_comma = unquoted(current, '`')
        .filter(|&(_, c)| c == ',')
        .map(|(i, _)| i)
        .last();
    let (kept, tail) = last_comma.map_or(("", current), |i| current.split_at(i + 1));
    // `x in [1,2`: the part before the comma is not a list of fields.
    if !unquoted(kept, '`').all(|(_, c)| c == ',' || !ends_field(c)) {
        return FieldPosition::Nothing;
    }
    match unquoted(tail, '`').find(|&(_, c)| ends_field(c)) {
        None => FieldPosition::Field { kept, typed: tail },
        Some((i @ 1.., c)) if c.is_whitespace() && kept.is_empty() => {
            let (field, rest) = (&tail[..i], tail[i..].trim_start());
            if rest.contains(char::is_whitespace) {
                return FieldPosition::Nothing;
            }
            FieldPosition::Literal(
                Op::ALL
                    .iter()
                    .map(|op| op.symbol())
                    .filter(|symbol| symbol.starts_with(rest))
                    .map(|symbol| format!("{field} {symbol}"))
                    .collect(),
            )
        }
        Some((i @ 1.., ':')) => {
            let (field, rest) = (&tail[..i], &tail[i + 1..]);
            if unquoted(rest, '`').any(|(_, c)| ends_field(c)) {
                return FieldPosition::Nothing;
            }
            FieldPosition::Literal(
                directions(rest)
                    .map(|direction| format!("{kept}{field}:{direction}"))
                    .collect(),
            )
        }
        Some(_) => FieldPosition::Nothing,
    }
}

// ---- Firestore lookups ----

/// The root collection ids of `target`; `None` on any failure or timeout.
fn fetch_collections(target: &DbTarget) -> Option<Vec<String>> {
    within_deadline(async {
        let fs = grpc::connect(target).await.ok()?;
        let ids = fs.list_collections(None).await.ok()?;
        Some(ids.iter().map(ToString::to_string).collect())
    })
}

/// The field names (as Firestore renders them) of a sample of the documents in `scope`;
/// `None` on any failure or timeout.
fn fetch_fields(target: &DbTarget, scope: &Scope) -> Option<Vec<String>> {
    let query = Query {
        scope: scope.clone(),
        filters: Vec::new(),
        order: Vec::new(),
        limit: Some(Limit::from(SAMPLE_SIZE)),
        offset: 0,
        select: None,
    };
    within_deadline(async {
        let fs = grpc::connect(target).await.ok()?;
        let docs = fs.query(&query).await.ok()?;
        Some(
            field_names(&docs)
                .iter()
                .map(FieldPath::to_firestore)
                .collect(),
        )
    })
}

/// Runs `lookup` on a private single-threaded runtime (completion runs before the
/// program's own runtime exists), giving up after [`DEADLINE`].
fn within_deadline<T>(lookup: impl Future<Output = Option<T>>) -> Option<T> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .ok()?;
    let found = runtime
        .block_on(async { tokio::time::timeout(DEADLINE, lookup).await })
        .ok()
        .flatten();
    // Do not wait for stragglers (a DNS lookup, a `gcloud` child) past the deadline.
    runtime.shutdown_background();
    found
}

// ---- cache ----

/// Answers of earlier lookups for one database, one JSON file per question. Entries
/// carry the time they were written, so staleness is a pure function of the clock.
#[derive(Debug)]
struct Cache {
    dir: PathBuf,
    ttl: Duration,
}

impl Cache {
    /// `<base>/fsq/<project>/<database>`.
    fn new(base: &Path, target: &DbTarget, ttl: Duration) -> Self {
        Self {
            dir: base
                .join("fsq")
                .join(target.project.as_str())
                .join(target.database.as_str()),
            ttl,
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.json"))
    }

    /// The items stored under `name` if they were written at most `ttl` before `now`.
    fn read(&self, name: &str, now: SystemTime) -> Option<Vec<String>> {
        let entry: serde_json::Value =
            serde_json::from_slice(&fs::read(self.file(name)).ok()?).ok()?;
        let written = entry.get("written")?.as_u64()?;
        let fresh = unix_seconds(now).saturating_sub(written) <= self.ttl.as_secs();
        fresh.then(|| {
            entry
                .get("items")?
                .as_array()?
                .iter()
                .map(|item| item.as_str().map(str::to_owned))
                .collect()
        })?
    }

    /// Stores `items` under `name`, stamped `now`. Failing to write is not an error: the
    /// cache only saves time.
    fn write(&self, name: &str, now: SystemTime, items: &[String]) {
        let entry = json!({ "written": unix_seconds(now), "items": items });
        let _ = fs::create_dir_all(&self.dir)
            .and_then(|()| fs::write(self.file(name), entry.to_string()));
    }
}

fn unix_seconds(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// The cache root: `$XDG_CACHE_HOME` if it is an absolute path, else `$HOME/.cache`.
fn cache_base(env: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let absolute = |name: &str| env(name).map(PathBuf::from).filter(|p| p.is_absolute());
    absolute("XDG_CACHE_HOME").or_else(|| absolute("HOME").map(|home| home.join(".cache")))
}

/// The cache entry name for the fields of `scope`: `fields-<collection path>`, with `/`
/// (not allowed in a file name) written as `~`; a collection group is
/// `fields-group~<id>`, then `~<parent document>`. A collection path has an odd number
/// of segments and these an even number, so the two kinds never share a name.
fn fields_cache_name(scope: &Scope) -> String {
    let flat = |path: &dyn ToString| path.to_string().replace('/', "~");
    match scope {
        Scope::Collection(path) => format!("fields-{}", flat(path)),
        Scope::Group { id, parent: None } => format!("fields-group~{id}"),
        Scope::Group {
            id,
            parent: Some(parent),
        } => format!("fields-group~{id}~{}", flat(parent)),
    }
}

/// The items cached under `name`, else what `fetch` finds (which is then cached). A
/// failed lookup (`None`) is neither cached nor an error: there is just nothing to offer.
fn cached(
    cache: Option<&Cache>,
    name: &str,
    now: SystemTime,
    fetch: impl FnOnce() -> Option<Vec<String>>,
) -> Vec<String> {
    cache
        .and_then(|cache| cache.read(name, now))
        .or_else(|| {
            let fresh = fetch()?;
            if let Some(cache) = cache {
                cache.write(name, now, &fresh);
            }
            Some(fresh)
        })
        .unwrap_or_default()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::cell::Cell;

    use pretty_assertions::assert_eq;

    use super::*;

    fn doc(data: Fields) -> Document {
        Document {
            path: "users/u1".parse().unwrap(),
            update_time: "2026-01-01T00:00:00Z".parse().unwrap(),
            data,
        }
    }

    fn map<const N: usize>(entries: [(&str, Value); N]) -> Value {
        Value::Map(
            entries
                .map(|(k, v)| (k.to_owned(), v))
                .into_iter()
                .collect(),
        )
    }

    fn names(docs: &[Document]) -> Vec<String> {
        field_names(docs)
            .iter()
            .map(FieldPath::to_firestore)
            .collect()
    }

    fn words(line: &str) -> Vec<String> {
        line.split_whitespace().map(str::to_owned).collect()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn values(candidates: &[CompletionCandidate]) -> Vec<String> {
        candidates
            .iter()
            .map(|c| c.get_value().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn field_names_include_nested() {
        let data = [
            ("a".to_owned(), Value::Int(1)),
            ("b".to_owned(), map([("c", Value::Int(2))])),
        ]
        .into_iter()
        .collect();
        assert_eq!(names(&[doc(data)]), ["a", "b", "b.c"]);
    }

    #[test]
    fn field_names_stop_at_depth_3_skip_arrays_and_quote_odd_keys() {
        let deep = map([("b", map([("c", map([("d", Value::Int(1))]))]))]);
        let data = [
            ("a".to_owned(), deep),
            (
                "list".to_owned(),
                Value::Array(vec![map([("x", Value::Null)])]),
            ),
            ("my.key".to_owned(), map([("k-2", Value::Null)])),
            (String::new(), Value::Null),
        ]
        .into_iter()
        .collect();
        assert_eq!(
            names(&[doc(data)]),
            ["a", "a.b", "a.b.c", "list", "`my.key`", "`my.key`.`k-2`"]
        );
    }

    #[test]
    fn field_names_merge_across_documents() {
        let one = doc([("a".to_owned(), Value::Null)].into_iter().collect());
        let two = doc(
            [("b".to_owned(), Value::Null), ("a".to_owned(), Value::Null)]
                .into_iter()
                .collect(),
        );
        assert_eq!(names(&[one, two]), ["a", "b"]);
    }

    fn cache_in(dir: &Path) -> Cache {
        Cache {
            dir: dir.to_owned(),
            ttl: Duration::from_secs(600),
        }
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn cache_expires_after_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        cache.write("collections", at(1_000), &["users".to_owned()]);
        let read = |t| cache.read("collections", at(t));
        assert_eq!(read(1_000), Some(vec!["users".to_owned()]));
        assert_eq!(read(1_600), Some(vec!["users".to_owned()]));
        assert_eq!(read(1_601), None);
        assert_eq!(cache.read("other", at(1_000)), None);
    }

    #[test]
    fn cache_survives_missing_dirs_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(&dir.path().join("a/b"));
        cache.write("x", at(5), &["1".to_owned()]);
        assert_eq!(cache.read("x", at(5)), Some(vec!["1".to_owned()]));
        fs::write(cache.file("x"), "not json").unwrap();
        assert_eq!(cache.read("x", at(5)), None);
        // An unwritable location is ignored, not an error.
        let blocked = dir.path().join("file");
        fs::write(&blocked, "").unwrap();
        cache_in(&blocked.join("sub")).write("x", at(5), &[]);
    }

    #[test]
    fn cache_layout_follows_project_and_database() {
        let target = DbTarget {
            project: "p".parse().unwrap(),
            database: "(default)".parse().unwrap(),
        };
        let cache = Cache::new(Path::new("/c"), &target, CACHE_TTL);
        assert_eq!(
            cache.file("collections"),
            Path::new("/c/fsq/p/(default)/collections.json")
        );
        let scope = |path: &str| Scope::Collection(path.parse().unwrap());
        assert_eq!(fields_cache_name(&scope("users")), "fields-users");
        assert_eq!(
            fields_cache_name(&scope("users/u1/orders")),
            "fields-users~u1~orders"
        );
        let group = |parent: Option<&str>| Scope::Group {
            parent: parent.map(|p| p.parse().unwrap()),
            id: "orders".parse().unwrap(),
        };
        assert_eq!(fields_cache_name(&group(None)), "fields-group~orders");
        assert_eq!(
            fields_cache_name(&group(Some("users/u1"))),
            "fields-group~orders~users~u1"
        );
    }

    #[test]
    fn cached_fetches_once_and_never_caches_failures() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let calls = Cell::new(0);
        let calls = &calls;
        let fetch = |result: Option<Vec<String>>| {
            move || {
                calls.set(calls.get() + 1);
                result
            }
        };
        assert_eq!(
            cached(Some(&cache), "k", at(1), fetch(None)),
            Vec::<String>::new()
        );
        assert_eq!(
            cached(Some(&cache), "k", at(1), fetch(Some(vec!["a".into()]))),
            ["a"]
        );
        assert_eq!(
            cached(Some(&cache), "k", at(2), fetch(Some(vec!["b".into()]))),
            ["a"]
        );
        assert_eq!(calls.get(), 2);
        assert_eq!(
            cached(Some(&cache), "k", at(5_000), fetch(Some(vec!["c".into()]))),
            ["c"]
        );
        assert_eq!(
            cached(None, "k", at(1), fetch(Some(vec!["d".into()]))),
            ["d"]
        );
    }

    #[test]
    fn cache_base_prefers_absolute_xdg_then_home() {
        let env = |pairs: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        let both = env(&[("XDG_CACHE_HOME", "/x"), ("HOME", "/h")]);
        assert_eq!(cache_base(&both), Some("/x".into()));
        let home = env(&[("XDG_CACHE_HOME", ""), ("HOME", "/h")]);
        assert_eq!(cache_base(&home), Some("/h/.cache".into()));
        let relative = env(&[("XDG_CACHE_HOME", "rel"), ("HOME", "/h")]);
        assert_eq!(cache_base(&relative), Some("/h/.cache".into()));
        assert_eq!(cache_base(&no_env), None);
    }

    #[test]
    fn completer_is_silent_without_project() {
        let inv = Invocation::new(&words("fsq query users --where"), &no_env);
        assert_eq!(inv.target, None);
        assert_eq!(
            values(&collection_candidates(&inv, "", at(0))),
            Vec::<String>::new()
        );
        assert_eq!(
            values(&field_candidates(&inv, "", at(0))),
            Vec::<String>::new()
        );
        assert_eq!(
            values(&field_candidates(&inv, "na", at(0))),
            Vec::<String>::new()
        );
        let empty = Invocation::new(&[], &no_env);
        assert_eq!(
            values(&collection_candidates(&empty, "", at(0))),
            Vec::<String>::new()
        );
        // Empty variables count as unset.
        let blank = Invocation::new(&words("fsq query"), &|_: &str| Some(String::new()));
        assert_eq!(blank.target, None);
    }

    #[test]
    fn completion_words_drop_the_shell_prefix_and_blank_the_current_word() {
        let typed = |line: &str, index| typed_words(words(line).into_iter(), index);
        let line = |words: &[&str], detached| Typed {
            words: words.iter().map(|w| (*w).to_owned()).collect(),
            detached,
        };
        assert_eq!(
            typed("fsq -- fsq query us --all", Some(2)),
            line(&["fsq", "query", "", "--all"], false)
        );
        assert_eq!(
            typed("fsq -- fsq query", None),
            line(&["fsq", "query"], false)
        );
        assert_eq!(typed("fsq", Some(0)), line(&[], false));
        // The line the cursor is in the middle of keeps its later words.
        assert_eq!(
            typed("fsq -- fsq query users --where X --project p", Some(4)),
            line(
                &["fsq", "query", "users", "--where", "", "--project", "p"],
                false
            )
        );
    }

    #[test]
    fn bash_split_words_are_rejoined_and_the_split_is_reported() {
        let typed = |line: &str, index| typed_words(words(line).into_iter(), index);
        // `--project=p query users --order-by age:d`, bash style.
        let split = typed(
            "fsq -- fsq --project = p query users --order-by age : d",
            Some(9),
        );
        assert!(split.detached);
        assert_eq!(
            split.words,
            ["fsq", "--project=p", "query", "users", "--order-by", "age:"]
        );
        assert!(typed("fsq -- fsq --set a = x", Some(4)).detached);
        assert!(!typed("fsq -- fsq query x", Some(2)).detached);
        assert_eq!(
            rejoin_split_words(words("--database = d : x")),
            ["--database=d:x"]
        );
    }

    fn hints(line: &str, env: &[(&'static str, &'static str)]) -> (Option<String>, Option<String>) {
        let env = |name: &str| {
            env.iter()
                .find(|(k, _)| *k == name)
                .map(|(_, v)| (*v).to_owned())
        };
        let inv = Invocation::new(&words(line), &env);
        (
            inv.target.map(|t| format!("{}/{}", t.project, t.database)),
            inv.scope.map(|s| match s {
                Scope::Collection(c) => format!("collection {c}"),
                Scope::Group { parent: None, id } => format!("group {id}"),
                Scope::Group {
                    parent: Some(p),
                    id,
                } => format!("group {id} under {p}"),
            }),
        )
    }

    #[test]
    fn invocation_reads_target_and_scope_from_the_typed_words() {
        let some = |t: &str, s: &str| (Some(t.to_owned()), Some(s.to_owned()));
        let target_only = |t: &str| (Some(t.to_owned()), None);
        assert_eq!(
            hints("fsq --project p query users --where", &[]),
            some("p/(default)", "collection users")
        );
        assert_eq!(
            hints("fsq query users --project p --database d --where", &[]),
            some("p/d", "collection users")
        );
        assert_eq!(
            hints("fsq query --project=p --database=d users", &[]),
            some("p/d", "collection users")
        );
        assert_eq!(
            hints(
                "fsq query users --where",
                &[("FSQ_PROJECT", "ep"), ("FSQ_DATABASE", "ed")]
            ),
            some("ep/ed", "collection users")
        );
        assert_eq!(
            hints(
                "fsq --project p query users --where",
                &[("FSQ_PROJECT", "ep")]
            ),
            some("p/(default)", "collection users")
        );
        assert_eq!(
            hints("fsq --project p query --group orders --where", &[]),
            some("p/(default)", "group orders")
        );
        assert_eq!(
            hints("fsq --project p query users/u1 --group orders", &[]),
            some("p/(default)", "group orders under users/u1")
        );
        assert_eq!(
            hints("fsq --project p count users/u1/orders --sum", &[]),
            some("p/(default)", "collection users/u1/orders")
        );
        assert_eq!(
            hints("fsq --project p update users/u1 --set", &[]),
            some("p/(default)", "collection users")
        );
        assert_eq!(
            hints("fsq --project p query --where a --limit zero users", &[])
                .1
                .as_deref(),
            Some("collection users")
        );
        assert_eq!(
            hints("fsq --project p query", &[]),
            target_only("p/(default)")
        );
        assert_eq!(hints("fsq --project p", &[]), target_only("p/(default)"));
        assert_eq!(hints("fsq --project Bad query users", &[]).0, None);
    }

    fn position(current: &str) -> Vec<String> {
        match field_position(current) {
            FieldPosition::Field { kept, typed } => vec![format!("field {kept}|{typed}")],
            FieldPosition::Literal(items) => items,
            FieldPosition::Nothing => Vec::new(),
        }
    }

    #[test]
    fn field_position_completes_only_the_field_part() {
        assert_eq!(position(""), ["field |"]);
        assert_eq!(position("na"), ["field |na"]);
        assert_eq!(position("address.ci"), ["field |address.ci"]);
        assert_eq!(position("`my fi"), ["field |`my fi"]);
        assert_eq!(position("name,ad"), ["field name,|ad"]);
        assert_eq!(position("`a,b`.c,ad"), ["field `a,b`.c,|ad"]);
        assert_eq!(position("name,"), ["field name,|"]);
        // Past the field part: nothing, except operators after `--where`'s field.
        for past in [
            "name=",
            "name=Jo",
            "tags+=x",
            "tags-=[a",
            "age>",
            "age >= 1",
            r#"n == "a b""#,
            "x in [1,2",
            "age >= [1,2",
            " x",
            "name ==  ",
        ] {
            assert!(position(past).is_empty(), "{past}");
        }
    }

    #[test]
    fn field_position_offers_operators_and_directions() {
        assert_eq!(position("age ").len(), 10);
        assert_eq!(position("age >"), ["age >", "age >="]);
        assert_eq!(
            position("`my field` ar"),
            ["`my field` array-contains", "`my field` array-contains-any"]
        );
        assert_eq!(position("age not"), ["age not-in"]);
        assert_eq!(position("age zz"), Vec::<String>::new());
        assert_eq!(position("age:"), ["age:asc", "age:desc"]);
        assert_eq!(position("age:d"), ["age:desc"]);
        assert_eq!(position("age:desc"), ["age:desc"]);
        assert_eq!(position("age:desc:"), Vec::<String>::new());
        assert_eq!(position(":d"), Vec::<String>::new());
    }

    #[test]
    fn registration_line_names_the_shell() {
        assert_eq!(
            registration_line(CompletionShell::Zsh),
            "source <(COMPLETE=zsh fsq)"
        );
        assert_eq!(
            registration_line(CompletionShell::Bash),
            "source <(COMPLETE=bash fsq)"
        );
    }

    #[test]
    fn a_leading_shell_quote_is_ignored() {
        let inv = Invocation::new(&words("fsq query users"), &no_env);
        assert_eq!(
            values(&field_candidates(&inv, "'age >", at(0))),
            ["age >", "age >="]
        );
        assert_eq!(
            values(&field_candidates(&inv, "\"age:d", at(0))),
            ["age:desc"]
        );
    }

    #[test]
    fn after_a_bash_split_nothing_is_offered() {
        let inv = Invocation {
            detached: true,
            ..Invocation::new(&words("fsq --project p query users"), &no_env)
        };
        assert_eq!(
            values(&collection_candidates(&inv, "x", at(0))),
            Vec::<String>::new()
        );
        assert_eq!(
            values(&field_candidates(&inv, "age:", at(0))),
            Vec::<String>::new()
        );
    }

    #[test]
    fn the_cursor_word_does_not_hide_later_flags() {
        let typed = typed_words(
            words("fsq -- fsq query users --where X --project p").into_iter(),
            Some(4),
        );
        let inv = Invocation::new(&typed.words, &no_env);
        assert_eq!(
            inv.target.map(|t| t.project.to_string()),
            Some("p".to_owned())
        );
    }

    #[test]
    fn a_lookup_that_finishes_in_time_is_returned() {
        assert_eq!(
            within_deadline(async {
                tokio::time::sleep(Duration::from_millis(1)).await;
                Some(1)
            }),
            Some(1)
        );
        assert_eq!(within_deadline(async { None::<u8> }), None);
    }

    #[test]
    fn literal_candidates_need_no_database() {
        let inv = Invocation::new(&words("fsq query users"), &no_env);
        assert_eq!(
            values(&field_candidates(&inv, "age:", at(0))),
            ["age:asc", "age:desc"]
        );
    }
}
