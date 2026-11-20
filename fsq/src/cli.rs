//! The command-line surface: [`Cli`] is what `clap` parses, [`Cli::into_request`] turns
//! it into an [`app::Request`](crate::app::Request). Flag values are parsed into domain
//! types by their `FromStr` impls, so a malformed path, filter or field op is a usage
//! error (exit 2) before anything runs.

use std::convert::Infallible;
use std::fs;
use std::io::Read;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::str::FromStr;

use clap::{Args, Parser, Subcommand, ValueEnum};
use clap_complete::Shell;
use itertools::Itertools;
use serde_json::Value as Json;

use crate::app::{AppError, Request};
use crate::codec::typed_json;
use crate::domain::{
    AggregateQuery, Aggregation, AnyPath, CollectionId, DatabaseId, DbTarget, DeleteDoc, DocPath,
    FieldOp, FieldPath, Filter, Guard, Limit, NonEmptyVec, OrderBy, ProjectId, Query, Scope,
    SetDoc, SetMode, UpdateDoc, UpdateTime, Write,
};
use crate::parse::{parse_field, unquoted};
use crate::render::{Format, Layout, RenderOpts, Typing};

/// `query` returns at most this many documents unless `--limit` or `--all` says otherwise.
const DEFAULT_LIMIT: NonZeroU32 = NonZeroU32::new(100).unwrap();

const LONG_HELP: &str = r#"VALUES (--where, --set, lists)
  null  true  false     literals
  18  -3  1.5           numbers (integer or float)
  "text"                quoted string; escapes are \" and \\ only
  ts:2026-01-05T10:00:00Z   timestamp (RFC 3339, nanoseconds kept)
  ref:users/u1          document reference
  [a, "b c", 3]         list, for `in`, `not-in`, `array-contains-any`, `+=`, `-=`
  anything else         a string. Numbers with leading zeros (007, 0123) stay strings;
                        quote a value to force a string ("18", "true", "null").
  A whole filter is one shell argument:  --where 'name == "John Smith"'
  Operators: == != < <= > >= in not-in array-contains array-contains-any

field names:
  Dots separate nested fields: address.city. Wrap a name containing spaces or
  punctuation (- + = : < > ! . whitespace) in backticks: `my-field`, `a b`.c
  A literal backtick or backslash inside backticks is escaped with a backslash.

updating fields (update --set):
  field=value    set the field
  field+=v       ARRAY-UNION: add v (or each element of [a, b]) if not already present
  field-=v       ARRAY-REMOVE: remove every occurrence of v (or of each element)
  Both += and -= work on arrays only. They are not numeric increment or decrement,
  and applied to a field that is not an array they replace it with an array.

typed json (what `get`/`query` print and `set --data` reads):
  Documents are {"_path": ..., "_updateTime": ..., "data": {...}}. Values that JSON
  cannot express carry a tag:
  {"$ts": "2026-01-05T10:00:00.123456789Z"}   timestamp
  {"$ref": "users/u1"}                         document reference
  {"$double": 1.5}                             double ("NaN", "Infinity" as strings);
                                               a bare JSON integer is an integer
  {"$bytes": "aGk="}                           bytes (standard base64 with padding)
  {"$geo": {"lat": 1.5, "lng": -2.0}}          geo point
  {"$map": {...}}                              escape: a map whose only key starts
                                               with $ is written inside $map
  --plain prints untagged JSON for humans and jq (not readable back losslessly).
  `set --data` takes a bare object of fields or a whole document as printed by
  `get` (its _path must match the target); use --data @file.json or --data - (stdin).

concurrency:
  --if-unchanged-since takes the _updateTime printed by `get`, verbatim. The write
  fails (exit 5) if the document changed or is missing.

environment:
  FSQ_PROJECT    default for --project
  FSQ_DATABASE   default for --database (otherwise "(default)")
  Flags win over environment variables.

output:
  A table on a terminal, JSON otherwise (indented on a terminal, compact in a pipe);
  jsonl is always compact. --format, --pretty, --compact and --plain override.

exit codes:
  0  ok
  1  other error
  2  usage error (bad flag, path, filter, field op or --data)
  3  not found
  4  already exists
  5  precondition failed (changed since --if-unchanged-since)
  6  writes disabled for this database
  7  missing index (the error lists the index to create)
  8  authentication failed (run: gcloud auth application-default login)
"#;

/// Query Firestore from the command line.
///
/// Reads work everywhere; writes (`set`, `update`, `delete`) work only on databases
/// the operator has enabled for writing, and fail with exit code 6 otherwise.
#[derive(Debug, Parser)]
#[command(name = "fsq", version, after_long_help = LONG_HELP)]
pub struct Cli {
    #[command(flatten)]
    pub conn: ConnArgs,
    #[command(flatten)]
    pub out: OutArgs,
    #[command(subcommand)]
    pub cmd: Cmd,
}

/// Which database to talk to.
#[derive(Debug, Clone, Args)]
pub struct ConnArgs {
    /// Google Cloud project id (required, except for `completions`).
    #[arg(long, global = true, env = "FSQ_PROJECT", value_name = "PROJECT")]
    pub project: Option<ProjectId>,
    /// Firestore database id.
    #[arg(
        long,
        global = true,
        env = "FSQ_DATABASE",
        value_name = "DATABASE",
        default_value_t = DatabaseId::default()
    )]
    pub database: DatabaseId,
}

impl ConnArgs {
    /// The database addressed, or `None` when `--project` (and `FSQ_PROJECT`) is unset.
    #[must_use]
    pub fn target(&self) -> Option<DbTarget> {
        self.project.clone().map(|project| DbTarget {
            project,
            database: self.database.clone(),
        })
    }
}

/// `--format` values.
#[derive(Debug, Clone, Copy, ValueEnum)]
enum FormatArg {
    /// Human-readable table.
    Table,
    /// One JSON value.
    Json,
    /// One compact JSON document per line.
    Jsonl,
}

/// How results are printed.
#[derive(Debug, Clone, Args)]
pub struct OutArgs {
    /// Output format. Default: table on a terminal, json otherwise.
    #[arg(long, global = true, value_enum)]
    format: Option<FormatArg>,
    /// Indent JSON (default on a terminal).
    #[arg(long, global = true, conflicts_with = "compact")]
    pretty: bool,
    /// Print JSON on one line (default when piped).
    #[arg(long, global = true)]
    compact: bool,
    /// Untagged JSON for humans and jq: no `$ts`/`$double`/... wrappers.
    #[arg(long, global = true)]
    plain: bool,
}

impl OutArgs {
    /// The output options for a terminal (`is_tty`) or a pipe.
    #[must_use]
    pub fn render_opts(&self, is_tty: bool) -> RenderOpts {
        let format = self.format.map(|f| match f {
            FormatArg::Table => Format::Table,
            FormatArg::Json => Format::Json,
            FormatArg::Jsonl => Format::Jsonl,
        });
        let layout = match (self.pretty, self.compact) {
            (true, _) => Some(Layout::Pretty),
            (_, true) => Some(Layout::Compact),
            _ => None,
        };
        let typing = if self.plain {
            Typing::Plain
        } else {
            Typing::Typed
        };
        RenderOpts::resolve(is_tty, format, layout, typing)
    }
}

/// Where `set --data` reads its JSON from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataArg {
    /// The argument itself.
    Inline(String),
    /// `@path`: a file.
    File(PathBuf),
    /// `-`: standard input.
    Stdin,
}

impl FromStr for DataArg {
    type Err = Infallible;

    fn from_str(s: &str) -> Result<Self, Infallible> {
        Ok(match (s, s.strip_prefix('@')) {
            ("-", _) => Self::Stdin,
            (_, Some(path)) => Self::File(PathBuf::from(path)),
            _ => Self::Inline(s.to_owned()),
        })
    }
}

/// A field path from a flag, as strict as in `--where`: an unquoted name may not contain
/// whitespace or `< > ! = + - :`, so a typo like `name age` is an error, not one field.
fn strict_field(text: &str) -> Result<FieldPath, String> {
    parse_field(
        text,
        "backtick-quote field names with spaces or punctuation, e.g. `my-field`",
    )
}

/// A comma-separated list of field paths (`--select a,b.c`); commas inside backticks
/// do not split.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldList(Vec<FieldPath>);

impl FromStr for FieldList {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        let commas = unquoted(s, '`')
            .filter(|&(_, c)| c == ',')
            .map(|(i, _)| i)
            .collect_vec();
        let bounds = std::iter::once(0)
            .chain(commas.iter().map(|i| i + 1))
            .zip(commas.iter().copied().chain(std::iter::once(s.len())));
        bounds
            .map(|(from, to)| {
                let name = s[from..to].trim();
                strict_field(name)
            })
            .try_collect()
            .map(Self)
    }
}

/// Where to look and what to keep: shared by `query` and `count`.
#[derive(Debug, Clone, Args)]
pub struct Selection {
    /// Collection to read, e.g. `users` or `users/u1/orders`. With --group: optionally
    /// a document, to search only below it.
    #[arg(value_name = "PATH", required_unless_present = "group")]
    path: Option<AnyPath>,
    /// Read every collection with this id at any depth (a bare id like `orders`).
    #[arg(long, value_name = "COLLECTION_ID")]
    group: Option<CollectionId>,
    /// Condition `<field> <op> <value>`; repeat for AND. Quote the whole filter.
    #[arg(long = "where", value_name = "FILTER", action = clap::ArgAction::Append)]
    where_: Vec<Filter>,
}

impl Selection {
    /// The scope the path and --group describe.
    fn scope(self) -> Result<(Scope, Vec<Filter>), AppError> {
        let scope = match (self.group, self.path) {
            (None, Some(AnyPath::Collection(c))) => Ok(Scope::Collection(c)),
            (None, Some(AnyPath::Doc(d))) => Err(format!(
                "{d} is a document path; use `get {d}`, or give a collection path"
            )),
            (None, None) => Err("a collection path is required".to_owned()),
            (Some(id), None) => Ok(Scope::Group { parent: None, id }),
            (Some(id), Some(AnyPath::Doc(parent))) => Ok(Scope::Group {
                parent: Some(parent),
                id,
            }),
            (Some(_), Some(AnyPath::Collection(c))) => Err(format!(
                "with --group the path must be a document to search below; {c} is a collection"
            )),
        }
        .map_err(AppError::Input)?;
        Ok((scope, self.where_))
    }
}

/// The commands.
#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Print one document.
    Get {
        /// Document path, e.g. `users/u1`.
        path: DocPath,
    },
    /// List documents of a collection. At most 100 unless --limit or --all.
    Query {
        #[command(flatten)]
        selection: Selection,
        /// Sort term `field` or `field:desc`; repeat for several.
        #[arg(long, value_name = "FIELD[:asc|desc]", action = clap::ArgAction::Append)]
        order_by: Vec<OrderBy>,
        /// Maximum documents returned.
        #[arg(long, value_name = "N", conflicts_with = "all")]
        limit: Option<NonZeroU32>,
        /// No limit.
        #[arg(long)]
        all: bool,
        /// Skip this many documents.
        #[arg(long, value_name = "N", default_value_t = 0)]
        offset: u32,
        /// Return only these fields, comma separated: `name,address.city`.
        #[arg(long, value_name = "FIELDS")]
        select: Option<FieldList>,
    },
    /// Count documents, or sum/average a numeric field, on the server.
    Count {
        #[command(flatten)]
        selection: Selection,
        /// Also sum this field; repeat for several fields.
        #[arg(long, value_name = "FIELD", value_parser = strict_field, action = clap::ArgAction::Append)]
        sum: Vec<FieldPath>,
        /// Also average this field; repeat for several fields.
        #[arg(long, value_name = "FIELD", value_parser = strict_field, action = clap::ArgAction::Append)]
        avg: Vec<FieldPath>,
    },
    /// List collection ids: the root ones, or those under a document.
    Collections {
        /// Document whose subcollections to list; omit for the root collections.
        parent: Option<DocPath>,
    },
    /// List the composite indexes of a collection id.
    Indexes {
        /// Collection id, e.g. `orders`.
        collection: CollectionId,
    },
    /// Write a whole document. Creates it; fails with exit 4 if it exists, unless
    /// --overwrite or --merge.
    Set {
        /// Document path, e.g. `users/u1`.
        path: DocPath,
        /// JSON (typed, see `fsq --help`): inline, `@file.json`, or `-` for stdin.
        #[arg(long, value_name = "JSON|@FILE|-")]
        data: DataArg,
        /// Replace the document if it exists.
        #[arg(long, group = "mode")]
        overwrite: bool,
        /// Merge the given fields into the document, creating it if missing.
        #[arg(long, group = "mode")]
        merge: bool,
        /// Only write if the document's _updateTime is exactly this (needs --overwrite
        /// or --merge).
        #[arg(long, value_name = "UPDATE_TIME", requires = "mode")]
        if_unchanged_since: Option<UpdateTime>,
    },
    /// Change fields of an existing document: --set field=value, field+=v, field-=v.
    Update {
        /// Document path, e.g. `users/u1`.
        path: DocPath,
        /// `field=value` sets, `field+=v` array-unions, `field-=v` array-removes (not
        /// numeric increments); repeat for several changes.
        #[arg(
            long = "set",
            value_name = "OP",
            required = true,
            action = clap::ArgAction::Append
        )]
        set: Vec<FieldOp>,
        /// Only write if the document's _updateTime is exactly this.
        #[arg(long, value_name = "UPDATE_TIME")]
        if_unchanged_since: Option<UpdateTime>,
    },
    /// Delete a document (fails with exit 3 if it does not exist).
    Delete {
        /// Document path, e.g. `users/u1`.
        path: DocPath,
        /// Only delete if the document's _updateTime is exactly this.
        #[arg(long, value_name = "UPDATE_TIME")]
        if_unchanged_since: Option<UpdateTime>,
    },
    /// Print a shell completion script.
    Completions {
        /// The shell to generate for.
        shell: Shell,
    },
}

fn guard(since: Option<UpdateTime>) -> Guard {
    since.map_or(Guard::None, Guard::UnchangedSince)
}

/// The JSON text `--data` names.
fn read_data(data: DataArg, mut stdin: impl Read) -> Result<String, AppError> {
    let io_error = |what: String, e: std::io::Error| AppError::Input(format!("{what}: {e}"));
    match data {
        DataArg::Inline(text) => Ok(text),
        DataArg::File(path) => fs::read_to_string(&path)
            .map_err(|e| io_error(format!("--data @{}", path.display()), e)),
        DataArg::Stdin => {
            let mut text = String::new();
            stdin
                .read_to_string(&mut text)
                .map_err(|e| io_error("--data -".to_owned(), e))
                .map(|_| text)
        }
    }
}

/// The fields `set --data` writes to `target`. A full document's `_path` must name
/// the target, so data copied from another document is not written here by accident.
fn decode_data(text: &str, target: &DocPath) -> Result<crate::domain::Fields, AppError> {
    let json: Json = serde_json::from_str(text)
        .map_err(|e| AppError::Input(format!("--data is not valid JSON: {e}")))?;
    match json.get("_path") {
        None => Ok(()),
        Some(Json::String(p)) => p
            .parse::<DocPath>()
            .map_err(|e| {
                AppError::Input(format!("_path in --data ({p}) is not a document path: {e}"))
            })
            .and_then(|path| {
                if path == *target {
                    Ok(())
                } else {
                    Err(AppError::Input(format!(
                        "_path in --data ({path}) does not match target {target}"
                    )))
                }
            }),
        Some(other) => Err(AppError::Input(format!(
            "_path in --data must be a string, got {other}"
        ))),
    }?;
    typed_json::fields_from_input(&json).map_err(|e| AppError::Input(format!("--data {e}")))
}

fn query(selection: Selection) -> Result<Query, AppError> {
    let (scope, filters) = selection.scope()?;
    Ok(Query {
        scope,
        filters,
        order: Vec::new(),
        limit: None,
        offset: 0,
        select: None,
    })
}

/// Count, then sums, then averages; a field named twice for the same aggregate is an error.
fn aggregations(
    sum: Vec<FieldPath>,
    avg: Vec<FieldPath>,
) -> Result<NonEmptyVec<Aggregation>, AppError> {
    let duplicate = |flag: &str, fields: &[FieldPath]| {
        fields
            .iter()
            .duplicates()
            .next()
            .map(|f| AppError::Input(format!("--{flag} {f} given more than once")))
    };
    if let Some(e) = duplicate("sum", &sum).or_else(|| duplicate("avg", &avg)) {
        return Err(e);
    }
    let all = std::iter::once(Aggregation::Count)
        .chain(sum.into_iter().map(Aggregation::Sum))
        .chain(avg.into_iter().map(Aggregation::Avg))
        .collect_vec();
    NonEmptyVec::try_from(all).map_err(|e| AppError::Input(e.to_string()))
}

impl Cli {
    /// The request the command line describes. `stdin` is read only by `set --data -`.
    ///
    /// # Errors
    /// [`AppError::Input`] for input that parses but is wrong: `--data` that is
    /// unreadable, not JSON, not valid typed JSON or names another document, a
    /// document path where a collection is needed (or the reverse), or a repeated
    /// `--sum`/`--avg` field. `completions` is not a request; callers handle it first
    /// and it yields an input error here.
    pub fn into_request(self, stdin: impl Read) -> Result<Request, AppError> {
        match self.cmd {
            Cmd::Get { path } => Ok(Request::Get(path)),
            Cmd::Query {
                selection,
                order_by,
                limit,
                all,
                offset,
                select,
            } => query(selection).map(|q| {
                Request::Query(Query {
                    order: order_by,
                    limit: (!all).then(|| Limit::from(limit.unwrap_or(DEFAULT_LIMIT))),
                    offset,
                    select: select.map(|FieldList(fields)| fields),
                    ..q
                })
            }),
            Cmd::Count {
                selection,
                sum,
                avg,
            } => Ok(Request::Aggregate(AggregateQuery {
                query: query(selection)?,
                aggregations: aggregations(sum, avg)?,
            })),
            Cmd::Collections { parent } => Ok(Request::Collections(parent)),
            Cmd::Indexes { collection } => Ok(Request::Indexes(collection)),
            Cmd::Set {
                path,
                data,
                overwrite,
                merge,
                if_unchanged_since,
            } => {
                let data = read_data(data, stdin).and_then(|text| decode_data(&text, &path))?;
                let guard = guard(if_unchanged_since);
                let mode = match (overwrite, merge) {
                    (true, _) => SetMode::Overwrite(guard),
                    (_, true) => SetMode::Merge(guard),
                    _ => SetMode::Create,
                };
                Ok(Request::Write(Write::Set(SetDoc { path, data, mode })))
            }
            Cmd::Update {
                path,
                set,
                if_unchanged_since,
            } => NonEmptyVec::try_from(set)
                .map_err(|e| AppError::Input(e.to_string()))
                .map(|ops| {
                    Request::Write(Write::Update(UpdateDoc {
                        path,
                        ops,
                        guard: guard(if_unchanged_since),
                    }))
                }),
            Cmd::Delete {
                path,
                if_unchanged_since,
            } => Ok(Request::Write(Write::Delete(DeleteDoc {
                path,
                guard: guard(if_unchanged_since),
            }))),
            Cmd::Completions { .. } => Err(AppError::Input(
                "`completions` prints a script and is not a Firestore request".to_owned(),
            )),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use std::io::{Cursor, empty};

    use super::*;
    use crate::domain::{Fields, Value};

    fn request(args: &[&str]) -> Result<Request, AppError> {
        request_with_stdin(args, "")
    }

    fn request_with_stdin(args: &[&str], stdin: &str) -> Result<Request, AppError> {
        Cli::try_parse_from(
            ["fsq", "--project", "p"]
                .into_iter()
                .chain(args.iter().copied()),
        )
        .unwrap()
        .into_request(Cursor::new(stdin.to_owned()))
    }

    fn query_of(args: &[&str]) -> Query {
        match request(args).unwrap() {
            Request::Query(q) => q,
            other => panic!("not a query: {other:?}"),
        }
    }

    fn set_of(args: &[&str]) -> SetDoc {
        match request(args).unwrap() {
            Request::Write(Write::Set(s)) => s,
            other => panic!("not a set: {other:?}"),
        }
    }

    fn field(s: &str) -> FieldPath {
        s.parse().unwrap()
    }

    #[test]
    fn query_defaults_to_100_documents() {
        let q = query_of(&["query", "users"]);
        assert_eq!(q.limit.map(Limit::get), Some(100));
        assert_eq!((q.offset, q.select, q.order.len()), (0, None, 0));
    }

    #[test]
    fn query_all_removes_the_limit_and_limit_overrides_it() {
        assert_eq!(query_of(&["query", "users", "--all"]).limit, None);
        assert_eq!(
            query_of(&["query", "users", "--limit", "5"])
                .limit
                .map(Limit::get),
            Some(5)
        );
        assert!(Cli::try_parse_from(["fsq", "query", "u", "--all", "--limit", "5"]).is_err());
        assert!(Cli::try_parse_from(["fsq", "query", "u", "--limit", "0"]).is_err());
    }

    #[test]
    fn query_collects_filters_order_offset_and_select() {
        let q = query_of(&[
            "query",
            "users",
            "--where",
            "age >= 18",
            "--where",
            r#"name == "Ann Lee""#,
            "--order-by",
            "age:desc",
            "--order-by",
            "name",
            "--offset",
            "3",
            "--select",
            "name,`a,b`.c",
        ]);
        assert_eq!(q.filters.len(), 2);
        assert_eq!(q.order.len(), 2);
        assert_eq!(q.offset, 3);
        assert_eq!(q.select, Some(vec![field("name"), field("`a,b`.c")]));
    }

    #[test]
    fn query_group_scopes() {
        assert_eq!(
            query_of(&["query", "--group", "orders"]).scope,
            Scope::Group {
                parent: None,
                id: "orders".parse().unwrap()
            }
        );
        assert_eq!(
            query_of(&["query", "users/u1", "--group", "orders"]).scope,
            Scope::Group {
                parent: Some("users/u1".parse().unwrap()),
                id: "orders".parse().unwrap()
            }
        );
    }

    #[test]
    fn query_rejects_the_wrong_kind_of_path() {
        assert!(
            matches!(request(&["query", "users/u1"]), Err(AppError::Input(m)) if m.contains("document path"))
        );
        assert!(matches!(
            request(&["query", "users", "--group", "orders"]),
            Err(AppError::Input(m)) if m.contains("collection")
        ));
    }

    #[test]
    fn count_lists_count_then_sums_then_averages() {
        let Request::Aggregate(a) = request(&[
            "count", "orders", "--avg", "total", "--sum", "qty", "--sum", "total",
        ])
        .unwrap() else {
            panic!("not an aggregate")
        };
        assert_eq!(
            a.aggregations.into_vec(),
            vec![
                Aggregation::Count,
                Aggregation::Sum(field("qty")),
                Aggregation::Sum(field("total")),
                Aggregation::Avg(field("total")),
            ]
        );
        assert_eq!((a.query.limit, a.query.offset), (None, 0));
    }

    #[test]
    fn count_rejects_duplicate_sum_and_avg_fields() {
        for args in [
            ["count", "o", "--sum", "t", "--sum", "t"],
            ["count", "o", "--avg", "t", "--avg", "t"],
        ] {
            assert!(matches!(request(&args), Err(AppError::Input(m)) if m.contains('t')));
        }
    }

    #[test]
    fn set_mode_follows_flags() {
        let mode =
            |extra: &[&str]| set_of(&[&["set", "users/u1", "--data", "{}"], extra].concat()).mode;
        let t: UpdateTime = "2026-01-01T00:00:00Z".parse().unwrap();
        let since = ["--if-unchanged-since", "2026-01-01T00:00:00Z"];
        assert_eq!(mode(&[]), SetMode::Create);
        assert_eq!(mode(&["--overwrite"]), SetMode::Overwrite(Guard::None));
        assert_eq!(mode(&["--merge"]), SetMode::Merge(Guard::None));
        assert_eq!(
            mode(&[&["--merge"][..], &since].concat()),
            SetMode::Merge(Guard::UnchangedSince(t))
        );
        assert!(
            Cli::try_parse_from([
                "fsq",
                "set",
                "u/1",
                "--data",
                "{}",
                "--if-unchanged-since",
                since[1]
            ])
            .is_err()
        );
    }

    #[test]
    fn set_data_sources() {
        let expected: Fields = [("a".to_owned(), Value::Int(1))].into_iter().collect();
        assert_eq!(
            set_of(&["set", "users/u1", "--data", r#"{"a":1}"#]).data,
            expected
        );
        let Request::Write(Write::Set(from_stdin)) =
            request_with_stdin(&["set", "users/u1", "--data", "-"], r#"{"a":1}"#).unwrap()
        else {
            panic!("not a set")
        };
        assert_eq!(from_stdin.data, expected);
        let dir = std::env::temp_dir().join(format!("fsq-cli-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("doc.json");
        fs::write(&file, r#"{"a":1}"#).unwrap();
        let arg = format!("@{}", file.display());
        assert_eq!(set_of(&["set", "users/u1", "--data", &arg]).data, expected);
        fs::remove_dir_all(&dir).unwrap();
        let missing = request(&["set", "users/u1", "--data", "@/nonexistent/fsq.json"]);
        assert!(matches!(missing, Err(AppError::Input(m)) if m.contains("/nonexistent/fsq.json")));
    }

    #[test]
    fn data_arg_parses_prefixes() {
        assert_eq!("-".parse(), Ok(DataArg::Stdin));
        assert_eq!("@f.json".parse(), Ok(DataArg::File("f.json".into())));
        assert_eq!("{}".parse(), Ok(DataArg::Inline("{}".into())));
    }

    #[test]
    fn full_document_data_must_name_the_target() {
        let doc = |p: &str| format!(r#"{{"_path":"{p}","data":{{"a":1}}}}"#);
        assert_eq!(
            set_of(&["set", "users/u1", "--data", &doc("users/u1")])
                .data
                .len(),
            1
        );
        let err = request(&["set", "users/u1", "--data", &doc("users/u2")]);
        assert_eq!(
            err,
            Err(AppError::Input(
                "_path in --data (users/u2) does not match target users/u1".into()
            ))
        );
        assert!(matches!(
            request(&["set", "users/u1", "--data", r#"{"_path":7,"data":{}}"#]),
            Err(AppError::Input(m)) if m.contains("must be a string")
        ));
        assert!(matches!(
            request(&["set", "users/u1", "--data", r#"{"_path":"users","data":{}}"#]),
            Err(AppError::Input(m)) if m.contains("not a document path")
        ));
    }

    #[test]
    fn data_errors_are_input_errors_with_pointer_and_reason() {
        assert!(matches!(
            request(&["set", "users/u1", "--data", "{nope"]),
            Err(AppError::Input(m)) if m.contains("not valid JSON")
        ));
        assert!(matches!(
            request(&["set", "users/u1", "--data", r#"{"a":{"$bogus":1}}"#]),
            Err(AppError::Input(m)) if m.starts_with("--data /a:")
        ));
        assert!(matches!(
            request(&["set", "users/u1", "--data", "[1]"]),
            Err(AppError::Input(m)) if m.contains("expected a JSON object")
        ));
    }

    #[test]
    fn update_collects_ops_in_order() {
        let Request::Write(Write::Update(u)) = request(&[
            "update",
            "users/u1",
            "--set",
            "a=1",
            "--set",
            "tags+=vip",
            "--set",
            "tags-=[x]",
        ])
        .unwrap() else {
            panic!("not an update")
        };
        assert_eq!(u.ops.len(), 3);
        assert_eq!(u.guard, Guard::None);
    }

    #[test]
    fn delete_carries_the_guard() {
        let Request::Write(Write::Delete(d)) = request(&[
            "delete",
            "users/u1",
            "--if-unchanged-since",
            "2026-01-01T00:00:00.5Z",
        ])
        .unwrap() else {
            panic!("not a delete")
        };
        assert!(matches!(d.guard, Guard::UnchangedSince(_)));
    }

    #[test]
    fn invalid_values_are_usage_errors() {
        for args in [
            &["get", "users"][..],
            &["get", "users/u1/x"],
            &["query", "users", "--where", "status active"],
            &["query", "users", "--order-by", "a:sideways"],
            &["query", "--group", "a/b"],
            &["update", "users/u1", "--set", "novalue"],
            &["delete", "users/u1", "--if-unchanged-since", "yesterday"],
            &["collections", "users"],
            &["query", "users", "--select", "name age"],
            &["count", "users", "--sum", "my-field"],
        ] {
            assert!(
                Cli::try_parse_from(
                    ["fsq", "--project", "p"]
                        .into_iter()
                        .chain(args.iter().copied())
                )
                .is_err(),
                "{args:?}"
            );
        }
    }

    #[test]
    fn target_uses_default_database_and_requires_project() {
        let cli = Cli::try_parse_from(["fsq", "--project", "p", "get", "u/1"]).unwrap();
        let target = cli.conn.target().unwrap();
        assert_eq!(
            (target.project.as_str(), target.database.as_str()),
            ("p", "(default)")
        );
        // Flags may follow the subcommand.
        let cli = Cli::try_parse_from(["fsq", "get", "u/1", "--project", "q", "--database", "d"])
            .unwrap();
        assert_eq!(cli.conn.target().unwrap().database.as_str(), "d");
    }

    #[test]
    fn render_opts_map_flags() {
        let opts = |args: &[&str], tty| {
            Cli::try_parse_from(
                ["fsq", "get", "u/1"]
                    .into_iter()
                    .chain(args.iter().copied()),
            )
            .unwrap()
            .out
            .render_opts(tty)
        };
        assert_eq!(opts(&[], true).format, Format::Table);
        assert_eq!(opts(&[], false).format, Format::Json);
        assert_eq!(opts(&[], false).layout, Layout::Compact);
        assert_eq!(opts(&["--pretty"], false).layout, Layout::Pretty);
        assert_eq!(opts(&["--compact"], true).layout, Layout::Compact);
        assert_eq!(
            opts(&["--format", "jsonl", "--plain"], true).format,
            Format::Jsonl
        );
        assert_eq!(opts(&["--plain"], true).typing, Typing::Plain);
        assert_eq!(opts(&[], true).typing, Typing::Typed);
    }

    #[test]
    fn completions_is_not_a_request() {
        let cli = Cli::try_parse_from(["fsq", "completions", "bash"]).unwrap();
        assert!(matches!(cli.into_request(empty()), Err(AppError::Input(_))));
    }
}
