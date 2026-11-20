//! Dynamic shell completion (`FSQ_COMPLETE=<shell> fsq`): collection ids and field names are
//! looked up in the addressed database while the user presses TAB.
//!
//! Completion must never get in the way, so every completer here is best effort:
//! - it returns no candidates on any failure (no project, no credentials, timeout, ...);
//! - it prints nothing itself (stdout is the completion protocol);
//! - it only reads Firestore; its only side effect is writing the cache files;
//! - it gives up after [`DEADLINE`], connecting included, and after a failure does not try
//!   again for [`FAILURE_TTL`], so an offline user is slowed down once a minute at most.
//!
//! The database and collection come from the words already typed, then from `FSQ_PROJECT` /
//! `FSQ_DATABASE`. Answers are cached for [`CACHE_TTL`] under
//! `$XDG_CACHE_HOME/fsq/<project>/<database>/` (`~/.cache` when unset).
//!
//! # Reliance on `clap_complete` internals
//! This is built on `clap_complete`'s `unstable-dynamic` feature, which is why the crate
//! pins `clap_complete = "~4.6"`. Besides its public API we depend on the layout its shell
//! scripts use to call the binary: the words after a `--` argument, the position of the
//! word being completed in `_CLAP_COMPLETE_INDEX`, the separator in `_CLAP_IFS`, and the
//! text of the bash registration script (patched in [`bash`]). Re-check all of them, and
//! the tests that cover them, before moving the pin.

mod bash;

use std::collections::BTreeSet;
use std::ffi::OsStr;
use std::fmt::Write as _;
use std::fs;
use std::io;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clap::{ArgMatches, CommandFactory};
use clap_complete::engine::{ArgValueCompleter, CompletionCandidate};
use clap_complete::env::{Shells, Zsh};
use itertools::Itertools;
use serde_json::json;

use crate::cli::{Cli, CompletionShell, ConnArgs};
use crate::domain::{
    AnyPath, CollectionId, DbTarget, Document, FieldPath, Fields, Limit, Op, Query, Scope, Value,
};
use crate::grpc;
use crate::parse::unquoted;
use crate::port::Firestore;

/// The environment variable that turns `fsq` into a completion helper (`FSQ_COMPLETE=zsh
/// fsq` prints the registration script). Not the `clap_complete` default, `COMPLETE`: a
/// stray `COMPLETE` in someone's environment must not hijack the program.
pub const COMPLETE_VAR: &str = "FSQ_COMPLETE";

const DEADLINE: Duration = Duration::from_secs(2);
const CACHE_TTL: Duration = Duration::from_mins(10);
const FAILURE_TTL: Duration = Duration::from_mins(1);
const SAMPLE_SIZE: NonZeroU32 = NonZeroU32::new(20).unwrap();
const MAX_FIELD_DEPTH: usize = 3;

/// bash (made safe for field names that are not plain words, see [`bash`]) and zsh: the
/// shells `fsq completions` documents; the others `clap_complete` knows are not registered.
#[must_use]
pub fn shells() -> Shells<'static> {
    Shells(&[&bash::FsqBash, &Zsh])
}

#[must_use]
pub fn registration_line(shell: CompletionShell) -> String {
    format!("source <({COMPLETE_VAR}={} fsq)", shell.name())
}

#[must_use]
pub fn collection_completer() -> ArgValueCompleter {
    ArgValueCompleter::new(|current: &OsStr| {
        silently(|| {
            let current = current.to_str()?;
            Some(collection_candidates(
                &Invocation::from_process(),
                current,
                SystemTime::now(),
                fetch_collections,
            ))
        })
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Where,
    OrderBy,
    Select,
    /// `--sum`, `--avg`, `--set`: the field name alone (`--set`'s `=value` is not completed).
    Single,
}

#[must_use]
pub fn field_completer(kind: FieldKind) -> ArgValueCompleter {
    ArgValueCompleter::new(move |current: &OsStr| {
        silently(|| {
            let current = current.to_str()?;
            Some(field_candidates(
                &Invocation::from_process(),
                kind,
                current,
                SystemTime::now(),
                fetch_fields,
            ))
        })
    })
}

/// The candidates `complete` finds, or none if it finds nothing or panics. A panic must not
/// print its message in a release build: stderr is the user's terminal mid-typing. Debug
/// builds (and so the test suite) keep it. The process exits right after completing, so the
/// silenced hook is not restored.
fn silently(
    complete: impl FnOnce() -> Option<Vec<CompletionCandidate>>,
) -> Vec<CompletionCandidate> {
    #[cfg(not(debug_assertions))]
    std::panic::set_hook(Box::new(|_| {}));
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(complete))
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// Every key, and the keys of nested maps (as dotted paths) down to depth
/// [`MAX_FIELD_DEPTH`]. Arrays are not descended into.
#[must_use]
pub fn field_names(docs: &[Document]) -> BTreeSet<FieldPath> {
    docs.iter()
        .flat_map(|doc| nested_keys(&[], &doc.data))
        .filter_map(|segments| FieldPath::from_segments(segments).ok())
        .collect()
}

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

#[derive(Debug)]
struct Invocation {
    target: Option<DbTarget>,
    scope: Option<Scope>,
    cache_base: Option<PathBuf>,
    /// The Firestore emulator in use (`FIRESTORE_EMULATOR_HOST`): its data is not the
    /// database's, so it is cached apart.
    emulator: Option<String>,
    /// The cursor word is the tail of one the shell cut at `:` or `=`, see [`Typed::detached`].
    detached: bool,
}

impl Invocation {
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
            emulator: env("FIRESTORE_EMULATOR_HOST").filter(|host| !host.is_empty()),
            detached: false,
        }
    }

    fn cache(&self) -> Option<Cache> {
        let (base, target) = self.cache_base.as_ref().zip(self.target.as_ref())?;
        Some(Cache::new(base, target, self.emulator.as_deref()))
    }
}

#[derive(Debug, PartialEq, Eq)]
struct Typed {
    /// The words after the program name's `--` marker, with the word at the cursor replaced by
    /// an empty one (so a flag before it still gets its value and later words are still read).
    words: Vec<String>,
    /// Whether the shell cut the cursor word off the word before it at a `:` or `=`
    /// (bash breaks words at those characters: `--order-by age:d` arrives as `age`, `:`,
    /// `d`). The command parser then takes the tail for an argument of its own, so it
    /// is no collection or field name.
    detached: bool,
}

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

fn parsed<T: FromStr>(matches: &ArgMatches, id: &str) -> Option<T> {
    let text = |m: &ArgMatches| m.try_get_one::<String>(id).ok().flatten().cloned();
    text(matches)
        .or_else(|| matches.subcommand().and_then(|(_, sub)| text(sub)))
        .and_then(|text| text.parse().ok())
}

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

fn collection_candidates(
    inv: &Invocation,
    current: &str,
    now: SystemTime,
    fetch: impl FnOnce(&DbTarget) -> Option<Vec<String>>,
) -> Vec<CompletionCandidate> {
    let (false, Some(target)) = (inv.detached, &inv.target) else {
        return Vec::new();
    };
    cached(inv.cache().as_ref(), "collections", now, || fetch(target))
        .into_iter()
        .filter(|id| id.starts_with(current))
        .map(CompletionCandidate::new)
        .collect()
}

/// Candidates for the word `current` of the flag value `kind`. zsh hands over the word
/// with the quote it was opened with (`'age >`); the quote is ignored, and left out of the
/// candidates: the shell matches them against the text inside the quote and keeps it open.
fn field_candidates(
    inv: &Invocation,
    kind: FieldKind,
    current: &str,
    now: SystemTime,
    fetch: impl FnOnce(&DbTarget, &Scope) -> Option<Vec<String>>,
) -> Vec<CompletionCandidate> {
    if inv.detached {
        return Vec::new();
    }
    let word = current.trim_start_matches(['\'', '"']);
    let candidates = match field_position(kind, word) {
        FieldPosition::Literal(candidates) => candidates,
        FieldPosition::Field { kept, typed } => inv
            .target
            .as_ref()
            .zip(inv.scope.as_ref())
            .map(|(target, scope)| {
                cached(inv.cache().as_ref(), &fields_cache_name(scope), now, || {
                    fetch(target, scope)
                })
            })
            .unwrap_or_default()
            .into_iter()
            .filter(|field| names_field(field, typed))
            .map(|field| format!("{kept}{field}"))
            .collect(),
        FieldPosition::Nothing => Vec::new(),
    };
    candidates
        .into_iter()
        .map(CompletionCandidate::new)
        .collect()
}

/// Matches as written or without the quoting, so that `my` finds `` `my-field` ``.
fn names_field(field: &str, typed: &str) -> bool {
    field.starts_with(typed)
        || FieldPath::from_str(field)
            .is_ok_and(|path| path.segments().iter().join(".").starts_with(typed))
}

fn directions(typed: &str) -> impl Iterator<Item = String> + '_ {
    ["asc", "desc"]
        .into_iter()
        .filter(move |direction| direction.starts_with(typed))
        .map(str::to_owned)
}

#[derive(Debug, PartialEq, Eq)]
enum FieldPosition<'a> {
    /// A field name starting with `typed`, after `kept` (the earlier items of a `--select a,b,`).
    Field {
        kept: &'a str,
        typed: &'a str,
    },
    Literal(Vec<String>),
    Nothing,
}

const FIELD_BREAKS: &str = "=<>!+-:,";

fn ends_field(c: char) -> bool {
    c.is_whitespace() || FIELD_BREAKS.contains(c)
}

/// Where in the flag value the cursor is. Only the field part is completed; after it,
/// `--where` gets operators, `--order-by` gets `asc`/`desc`, anything else gets nothing.
fn field_position(kind: FieldKind, current: &str) -> FieldPosition<'_> {
    let last_comma = unquoted(current, '`')
        .filter(|&(_, c)| c == ',')
        .map(|(i, _)| i)
        .last()
        .filter(|_| kind == FieldKind::Select);
    let (kept, tail) = last_comma.map_or(("", current), |i| current.split_at(i + 1));
    if !unquoted(kept, '`').all(|(_, c)| c == ',' || !ends_field(c)) {
        return FieldPosition::Nothing;
    }
    match (kind, unquoted(tail, '`').find(|&(_, c)| ends_field(c))) {
        (_, None) => FieldPosition::Field { kept, typed: tail },
        (FieldKind::Where, Some((i @ 1.., c))) if c.is_whitespace() => {
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
        (FieldKind::OrderBy, Some((i @ 1.., ':'))) => {
            let (field, rest) = (&tail[..i], &tail[i + 1..]);
            if unquoted(rest, '`').any(|(_, c)| ends_field(c)) {
                return FieldPosition::Nothing;
            }
            FieldPosition::Literal(
                directions(rest)
                    .map(|direction| format!("{field}:{direction}"))
                    .collect(),
            )
        }
        _ => FieldPosition::Nothing,
    }
}

fn fetch_collections(target: &DbTarget) -> Option<Vec<String>> {
    within_deadline(async {
        let fs = grpc::connect(target).await.ok()?;
        let ids = fs.list_collections(None).await.ok()?;
        Some(ids.iter().map(ToString::to_string).collect())
    })
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
enum Entry {
    Found(Vec<String>),
    Failed,
}

impl Entry {
    fn into_items(self) -> Vec<String> {
        match self {
            Self::Found(items) => items,
            Self::Failed => Vec::new(),
        }
    }
}

/// Answers of earlier lookups for one database, one JSON file per question. Entries
/// carry the time they were written, so staleness is a pure function of the clock.
#[derive(Debug)]
struct Cache {
    dir: PathBuf,
    ttl: Duration,
    failure_ttl: Duration,
}

impl Cache {
    /// `<base>/fsq/<project>/<database>`, below `emulator-<host>` for an emulator.
    fn new(base: &Path, target: &DbTarget, emulator: Option<&str>) -> Self {
        let database = base
            .join("fsq")
            .join(target.project.as_str())
            .join(target.database.as_str());
        Self {
            dir: emulator.map_or(database.clone(), |host| {
                database.join(format!("emulator-{}", escape_name(host)))
            }),
            ttl: CACHE_TTL,
            failure_ttl: FAILURE_TTL,
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.dir.join(format!("{name}.json"))
    }

    /// What is stored under `name`, unless it is older than its time to live or stamped
    /// in the future (a clock that went backwards, or a damaged file: either way not to
    /// be trusted).
    fn read(&self, name: &str, now: SystemTime) -> Option<Entry> {
        let entry: serde_json::Value =
            serde_json::from_slice(&fs::read(self.file(name)).ok()?).ok()?;
        let age = unix_seconds(now).checked_sub(entry.get("written")?.as_u64()?)?;
        if entry.get("failed").and_then(serde_json::Value::as_bool) == Some(true) {
            return (age <= self.failure_ttl.as_secs()).then_some(Entry::Failed);
        }
        (age <= self.ttl.as_secs()).then(|| {
            entry
                .get("items")?
                .as_array()?
                .iter()
                .map(|item| item.as_str().map(str::to_owned))
                .collect::<Option<Vec<_>>>()
                .map(Entry::Found)
        })?
    }

    /// Stores `entry` under `name`, stamped `now`. Failing to write is not an error: the
    /// cache only saves time. The directories are private to the user.
    fn write(&self, name: &str, now: SystemTime, entry: &Entry) {
        let written = unix_seconds(now);
        let body = match entry {
            Entry::Found(items) => json!({ "written": written, "items": items }),
            Entry::Failed => json!({ "written": written, "failed": true }),
        };
        let _ = create_private_dir(&self.dir)
            .and_then(|()| fs::write(self.file(name), body.to_string()));
    }
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(dir)
}

fn unix_seconds(t: SystemTime) -> u64 {
    t.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn cache_base(env: &impl Fn(&str) -> Option<String>) -> Option<PathBuf> {
    let absolute = |name: &str| env(name).map(PathBuf::from).filter(|p| p.is_absolute());
    absolute("XDG_CACHE_HOME").or_else(|| absolute("HOME").map(|home| home.join(".cache")))
}

/// `text` as one file name component: everything but ASCII letters, digits and `_` is
/// written as `%XX` (per UTF-8 byte). Injective, so different texts never share a name,
/// and free of `/`, `.`, `-` and `~`, which the names built from it use as separators.
fn escape_name(text: &str) -> String {
    text.bytes().fold(String::new(), |mut out, b| {
        if b.is_ascii_alphanumeric() || b == b'_' {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
        out
    })
}

/// The cache entry name for the fields of `scope`: `fields-c-<collection path>`, or
/// `fields-g-<id>[-<parent document>]` for a collection group; the parts are
/// [escaped](escape_name).
fn fields_cache_name(scope: &Scope) -> String {
    let part = |text: &dyn ToString| escape_name(&text.to_string());
    match scope {
        Scope::Collection(path) => format!("fields-c-{}", part(path)),
        Scope::Group { id, parent: None } => format!("fields-g-{}", part(id)),
        Scope::Group {
            id,
            parent: Some(parent),
        } => format!("fields-g-{}-{}", part(id), part(parent)),
    }
}

/// The items cached under `name`, else what `fetch` finds (which is then cached). A
/// failed lookup is cached too, briefly: there is nothing to offer, and asking again on
/// every TAB would cost a full timeout each time.
fn cached(
    cache: Option<&Cache>,
    name: &str,
    now: SystemTime,
    fetch: impl FnOnce() -> Option<Vec<String>>,
) -> Vec<String> {
    cache
        .and_then(|cache| cache.read(name, now))
        .unwrap_or_else(|| {
            let fresh = fetch().map_or(Entry::Failed, Entry::Found);
            if let Some(cache) = cache {
                cache.write(name, now, &fresh);
            }
            fresh
        })
        .into_items()
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

    fn none() -> Vec<String> {
        Vec::new()
    }

    /// The lookups of tests: none succeeds and none touches a real service.
    fn no_collections(_: &DbTarget) -> Option<Vec<String>> {
        None
    }

    fn no_fields(_: &DbTarget, _: &Scope) -> Option<Vec<String>> {
        None
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
            failure_ttl: Duration::from_secs(60),
        }
    }

    fn found(items: &[&str]) -> Entry {
        Entry::Found(items.iter().map(|i| (*i).to_owned()).collect())
    }

    fn at(secs: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(secs)
    }

    #[test]
    fn cache_expires_after_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        cache.write("collections", at(1_000), &found(&["users"]));
        let read = |t| cache.read("collections", at(t));
        assert_eq!(read(1_000), Some(found(&["users"])));
        assert_eq!(read(1_600), Some(found(&["users"])));
        assert_eq!(read(1_601), None);
        assert_eq!(cache.read("other", at(1_000)), None);
    }

    #[test]
    fn cache_entries_stamped_in_the_future_are_stale() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        cache.write("k", at(5_000), &found(&["a"]));
        assert_eq!(cache.read("k", at(5_000)), Some(found(&["a"])));
        assert_eq!(cache.read("k", at(4_999)), None);
        cache.write("f", at(5_000), &Entry::Failed);
        assert_eq!(cache.read("f", at(1)), None);
    }

    #[test]
    fn cache_survives_missing_dirs_and_garbage() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(&dir.path().join("a/b"));
        cache.write("x", at(5), &found(&["1"]));
        assert_eq!(cache.read("x", at(5)), Some(found(&["1"])));
        fs::write(cache.file("x"), "not json").unwrap();
        assert_eq!(cache.read("x", at(5)), None);
        fs::write(cache.file("x"), r#"{"written":5,"items":[1]}"#).unwrap();
        assert_eq!(cache.read("x", at(5)), None);
        let blocked = dir.path().join("file");
        fs::write(&blocked, "").unwrap();
        cache_in(&blocked.join("sub")).write("x", at(5), &found(&[]));
    }

    #[cfg(unix)]
    #[test]
    fn cache_directories_are_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let leaf = dir.path().join("fsq/p/d");
        cache_in(&leaf).write("x", at(1), &found(&[]));
        for created in [&leaf, &dir.path().join("fsq/p"), &dir.path().join("fsq")] {
            let mode = fs::metadata(created).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700, "{}", created.display());
        }
    }

    #[test]
    fn cache_layout_follows_project_database_and_emulator() {
        let target = DbTarget {
            project: "p".parse().unwrap(),
            database: "(default)".parse().unwrap(),
        };
        let base = Path::new("/c");
        assert_eq!(
            Cache::new(base, &target, None).file("collections"),
            Path::new("/c/fsq/p/(default)/collections.json")
        );
        assert_eq!(
            Cache::new(base, &target, Some("localhost:8080")).file("collections"),
            Path::new("/c/fsq/p/(default)/emulator-localhost%3A8080/collections.json")
        );
    }

    #[test]
    fn cache_names_are_injective() {
        assert_eq!(escape_name("users"), "users");
        assert_eq!(escape_name("a/b~c-d.e f"), "a%2Fb%7Ec%2Dd%2Ee%20f");
        assert_eq!(escape_name("é"), "%C3%A9");
        let scope = |path: &str| Scope::Collection(path.parse().unwrap());
        let group = |id: &str, parent: Option<&str>| Scope::Group {
            parent: parent.map(|p| p.parse().unwrap()),
            id: id.parse().unwrap(),
        };
        let all = [
            scope("users"),
            scope("users/u1/orders"),
            scope("users~u1/x/orders"),
            scope("a-b"),
            scope("a/b/c"),
            group("orders", None),
            group("orders", Some("users/u1")),
            group("orders-users", None),
            group("a", Some("b/c")),
            group("a-b", Some("c/d")),
        ];
        let names = all.iter().map(fields_cache_name).collect_vec();
        assert_eq!(names.iter().unique().count(), all.len(), "{names:?}");
        assert_eq!(names[0], "fields-c-users");
        assert_eq!(names[1], "fields-c-users%2Fu1%2Forders");
        assert_eq!(names[5], "fields-g-orders");
        assert_eq!(names[6], "fields-g-orders-users%2Fu1");
        assert!(names.iter().all(|n| !n.contains('/')));
    }

    #[test]
    fn cached_fetches_once_and_caches_what_it_found() {
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
        let some = |item: &str| Some(vec![item.to_owned()]);
        assert_eq!(cached(Some(&cache), "k", at(1), fetch(some("a"))), ["a"]);
        assert_eq!(cached(Some(&cache), "k", at(2), fetch(some("b"))), ["a"]);
        assert_eq!(calls.get(), 1);
        assert_eq!(
            cached(Some(&cache), "k", at(5_000), fetch(some("c"))),
            ["c"]
        );
        assert_eq!(cached(None, "k", at(1), fetch(some("d"))), ["d"]);
        assert_eq!(cached(None, "k", at(1), fetch(some("e"))), ["e"]);
        assert_eq!(calls.get(), 4);
    }

    #[test]
    fn failed_lookups_are_not_repeated_for_a_minute() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_in(dir.path());
        let calls = Cell::new(0);
        let fail = || {
            calls.set(calls.get() + 1);
            None
        };
        assert_eq!(cached(Some(&cache), "k", at(1_000), fail), none());
        assert_eq!(calls.get(), 1);
        assert_eq!(cached(Some(&cache), "k", at(1_059), fail), none());
        assert_eq!(cached(Some(&cache), "k", at(1_060), fail), none());
        assert_eq!(calls.get(), 1);
        assert_eq!(cached(Some(&cache), "k", at(1_061), fail), none());
        assert_eq!(calls.get(), 2);
        let ok = || Some(vec!["a".to_owned()]);
        assert_eq!(cached(Some(&cache), "k", at(1_200), ok), ["a"]);
        assert_eq!(cached(Some(&cache), "other", at(1_200), ok), ["a"]);
        assert_eq!(cached(None, "k", at(1_000), fail), none());
        assert_eq!(cached(None, "k", at(1_000), fail), none());
        assert_eq!(calls.get(), 4);
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
            values(&collection_candidates(&inv, "", at(0), no_collections)),
            none()
        );
        for kind in [FieldKind::Where, FieldKind::Select] {
            for typed in ["", "na"] {
                assert_eq!(
                    values(&field_candidates(&inv, kind, typed, at(0), no_fields)),
                    none()
                );
            }
        }
        let empty = Invocation::new(&[], &no_env);
        assert_eq!(
            values(&collection_candidates(&empty, "", at(0), no_collections)),
            none()
        );
        let blank = Invocation::new(&words("fsq query"), &|_: &str| Some(String::new()));
        assert_eq!(blank.target, None);
        assert_eq!(blank.emulator, None);
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

    fn position(kind: FieldKind, current: &str) -> Vec<String> {
        match field_position(kind, current) {
            FieldPosition::Field { kept, typed } => vec![format!("field {kept}|{typed}")],
            FieldPosition::Literal(items) => items,
            FieldPosition::Nothing => Vec::new(),
        }
    }

    #[test]
    fn every_kind_completes_a_field_name() {
        for kind in [
            FieldKind::Where,
            FieldKind::OrderBy,
            FieldKind::Select,
            FieldKind::Single,
        ] {
            assert_eq!(position(kind, ""), ["field |"]);
            assert_eq!(position(kind, "na"), ["field |na"]);
            assert_eq!(position(kind, "address.ci"), ["field |address.ci"]);
            assert_eq!(position(kind, "`my fi"), ["field |`my fi"]);
            for past in ["name=", "name=Jo", "tags+=x", "tags-=[a", "age>", " x"] {
                assert_eq!(position(kind, past), none(), "{kind:?} {past}");
            }
        }
    }

    #[test]
    fn only_select_completes_a_comma_list() {
        let select = |s| position(FieldKind::Select, s);
        assert_eq!(select("name,ad"), ["field name,|ad"]);
        assert_eq!(select("`a,b`.c,ad"), ["field `a,b`.c,|ad"]);
        assert_eq!(select("name,"), ["field name,|"]);
        assert_eq!(select("name ,x"), none());
        assert_eq!(select("name:desc,x"), none());
        for kind in [FieldKind::Where, FieldKind::OrderBy, FieldKind::Single] {
            assert_eq!(position(kind, "name,ad"), none(), "{kind:?}");
            assert_eq!(position(kind, "name,"), none(), "{kind:?}");
        }
    }

    #[test]
    fn only_where_offers_operators() {
        let wh = |s| position(FieldKind::Where, s);
        assert_eq!(wh("age ").len(), 10);
        assert_eq!(wh("age >"), ["age >", "age >="]);
        assert_eq!(
            wh("`my field` ar"),
            ["`my field` array-contains", "`my field` array-contains-any"]
        );
        assert_eq!(wh("age not"), ["age not-in"]);
        assert_eq!(wh("age zz"), none());
        for past in [
            "age >= 1",
            r#"n == "a b""#,
            "x in [1,2",
            "age >= [1,2",
            "name ==  ",
            " x",
        ] {
            assert_eq!(wh(past), none(), "{past}");
        }
        for kind in [FieldKind::OrderBy, FieldKind::Select, FieldKind::Single] {
            assert_eq!(position(kind, "age "), none(), "{kind:?}");
            assert_eq!(position(kind, "age >"), none(), "{kind:?}");
        }
    }

    #[test]
    fn only_order_by_offers_directions() {
        let ob = |s| position(FieldKind::OrderBy, s);
        assert_eq!(ob("age:"), ["age:asc", "age:desc"]);
        assert_eq!(ob("age:d"), ["age:desc"]);
        assert_eq!(ob("age:desc"), ["age:desc"]);
        assert_eq!(ob("age:desc:"), none());
        assert_eq!(ob(":d"), none());
        assert_eq!(ob("age "), none());
        for kind in [FieldKind::Where, FieldKind::Select, FieldKind::Single] {
            assert_eq!(position(kind, "age:"), none(), "{kind:?}");
            assert_eq!(position(kind, "age:d"), none(), "{kind:?}");
        }
    }

    #[test]
    fn registration_line_names_the_variable_and_the_shell() {
        assert_eq!(
            registration_line(CompletionShell::Zsh),
            "source <(FSQ_COMPLETE=zsh fsq)"
        );
        assert_eq!(
            registration_line(CompletionShell::Bash),
            "source <(FSQ_COMPLETE=bash fsq)"
        );
    }

    #[test]
    fn a_leading_shell_quote_is_ignored() {
        let inv = Invocation::new(&words("fsq query users"), &no_env);
        assert_eq!(
            values(&field_candidates(
                &inv,
                FieldKind::Where,
                "'age >",
                at(0),
                no_fields
            )),
            ["age >", "age >="]
        );
        assert_eq!(
            values(&field_candidates(
                &inv,
                FieldKind::OrderBy,
                "\"age:d",
                at(0),
                no_fields
            )),
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
            values(&collection_candidates(&inv, "x", at(0), no_collections)),
            none()
        );
        assert_eq!(
            values(&field_candidates(
                &inv,
                FieldKind::OrderBy,
                "d",
                at(0),
                no_fields
            )),
            none()
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
            values(&field_candidates(
                &inv,
                FieldKind::OrderBy,
                "age:",
                at(0),
                no_fields
            )),
            ["age:asc", "age:desc"]
        );
    }

    fn seeded(dir: &Path, fields: &[&str], emulator: Option<&str>) -> Invocation {
        let env = |name: &str| match name {
            "XDG_CACHE_HOME" => Some(dir.display().to_string()),
            "FIRESTORE_EMULATOR_HOST" => emulator.map(str::to_owned),
            _ => None,
        };
        let inv = Invocation::new(&words("fsq --project p query users"), &env);
        let scope = inv.scope.clone().unwrap();
        inv.cache()
            .unwrap()
            .write(&fields_cache_name(&scope), at(100), &found(fields));
        inv
    }

    #[test]
    fn cached_fields_are_filtered_by_what_was_typed() {
        let dir = tempfile::tempdir().unwrap();
        let inv = seeded(
            dir.path(),
            &["address", "address.city", "name", "`my-field`"],
            None,
        );
        // Everything is in the cache: a lookup (which could reach a real service) is a bug.
        let lookups = Cell::new(0);
        let fields = |_: &DbTarget, _: &Scope| {
            lookups.set(lookups.get() + 1);
            None
        };
        let candidates =
            |kind, typed| values(&field_candidates(&inv, kind, typed, at(200), fields));
        assert_eq!(
            candidates(FieldKind::Single, ""),
            ["address", "address.city", "name", "`my-field`"]
        );
        assert_eq!(
            candidates(FieldKind::Single, "addr"),
            ["address", "address.city"]
        );
        assert_eq!(candidates(FieldKind::Single, "address.c"), ["address.city"]);
        assert_eq!(
            candidates(FieldKind::Select, "name,ad"),
            ["name,address", "name,address.city"]
        );
        assert_eq!(candidates(FieldKind::Where, "zzz"), none());
        assert_eq!(candidates(FieldKind::Single, "my"), ["`my-field`"]);
        assert_eq!(candidates(FieldKind::Single, "`my"), ["`my-field`"]);
        assert_eq!(lookups.get(), 0);
    }

    #[test]
    fn a_cache_miss_looks_up_once_with_the_typed_database_and_collection() {
        let dir = tempfile::tempdir().unwrap();
        let env = |name: &str| (name == "XDG_CACHE_HOME").then(|| dir.path().display().to_string());
        let inv = Invocation::new(&words("fsq --project p --database d query users"), &env);
        let seen = std::cell::RefCell::new(Vec::new());
        let fields = |target: &DbTarget, scope: &Scope| {
            seen.borrow_mut()
                .push(format!("{}/{} {scope:?}", target.project, target.database));
            Some(vec!["a".to_owned()])
        };
        let ask = || {
            values(&field_candidates(
                &inv,
                FieldKind::Single,
                "",
                at(5),
                fields,
            ))
        };
        assert_eq!(ask(), ["a"]);
        assert_eq!(ask(), ["a"]);
        assert_eq!(seen.borrow().len(), 1);
        assert!(seen.borrow()[0].starts_with("p/d "), "{:?}", seen.borrow());
    }

    #[test]
    fn emulator_data_is_cached_apart() {
        let dir = tempfile::tempdir().unwrap();
        let inv = seeded(dir.path(), &["only_in_emulator"], Some("localhost:8080"));
        let scope = inv.scope.clone().unwrap();
        let name = fields_cache_name(&scope);
        assert_eq!(
            inv.cache().unwrap().read(&name, at(200)),
            Some(found(&["only_in_emulator"]))
        );
        // The same database without the emulator, and another emulator, have their own
        // caches, which hold nothing of it. (Only the caches are looked at: no lookup.)
        let root = dir.path().display().to_string();
        let in_dir = |emulator: Option<&'static str>| {
            let root = root.clone();
            let env = move |var: &str| match var {
                "XDG_CACHE_HOME" => Some(root.clone()),
                "FIRESTORE_EMULATOR_HOST" => emulator.map(str::to_owned),
                _ => None,
            };
            Invocation::new(&words("fsq --project p query users"), &env)
                .cache()
                .unwrap()
        };
        for other in [None, Some("localhost:9090"), Some("localhost:80801")] {
            let cache = in_dir(other);
            assert_ne!(cache.dir, inv.cache().unwrap().dir);
            assert_eq!(cache.read(&name, at(200)), None, "{other:?}");
        }
        assert_eq!(
            in_dir(Some("localhost:8080")).read(&name, at(200)),
            Some(found(&["only_in_emulator"]))
        );
    }
}
