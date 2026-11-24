use std::convert::Infallible;
use std::fs;
use std::io::Read;
use std::num::NonZeroU32;
use std::path::PathBuf;
use std::str::FromStr;

use clap::{Args, Parser, Subcommand, ValueEnum};
use itertools::Itertools as _;
use serde_json::Value as Json;

use crate::app::{AppError, Output, Request};
use crate::codec::typed_json::{self, Envelope, Input};
use crate::complete::{FieldKind, collection_completer, field_completer};
use crate::domain::{
    AggregateQuery, Aggregation, AnyPath, CollectionId, DatabaseId, DbTarget, DeleteDoc, DocPath,
    FieldOp, FieldPath, Fields, Filter, Guard, IdError, Limit, NonEmptyVec, OrderBy, ProjectId,
    Query, Scope, SetDoc, SetMode, UpdateDoc, UpdateTime, Write, check_field_ops,
};
use crate::parse::{parse_field, split_unquoted};
use crate::render::{Format, Layout, RenderOpts, Typing};

const DEFAULT_LIMIT: NonZeroU32 = NonZeroU32::new(100).unwrap();

const SEE_ROOT_HELP: &str =
    "Value syntax, field quoting, typed JSON tags and exit codes: run `firectl --help`.";

const LONG_HELP: &str = r#"Values (--where, --set, lists):
  null  true  false     literals
  18  -3  1.5           numbers (integer or float)
  "text"                quoted string; escapes are \" and \\ only
  ts:2026-01-05T10:00:00Z   timestamp (RFC 3339; Firestore keeps microseconds)
  ref:users/u1          document reference
  [a, "b c", 3]         list, for `in`, `not-in`, `array-contains-any`, `+=`, `-=`
  anything else         a string. Numbers with leading zeros (007, 0123) stay strings;
                        quote a value to force a string ("18", "true", "null").
  A --set value starting with { is rejected (--set cannot write a map): set nested
  fields one by one, or quote the whole op to store text: --set 'a="{...}"'
  (--where compares it as a string).
  A whole filter is one shell argument:  --where 'name == "John Smith"'
  Operators: == != < <= > >= in not-in array-contains array-contains-any

Field names:
  Dots separate nested fields: address.city. Wrap a name containing spaces or
  punctuation (- + = : < > ! . whitespace) in backticks: `my-field`, `a b`.c
  A literal backtick or backslash inside backticks is escaped with a backslash.

Updating fields (update --set):
  field=value    set the field
  field+=v       ARRAY-UNION: add v (or each element of [a, b]) if not already present
  field-=v       ARRAY-REMOVE: remove every occurrence of v (or of each element)
  Both += and -= work on arrays only. They are not numeric increment or decrement,
  and applied to a field that is not an array they replace it with an array.

Typed JSON (what `get`/`query` print and `set --data` reads):
  Documents are {"_path": ..., "_updateTime": ..., "data": {...}}. Values that JSON
  cannot express carry a tag:
  {"$ts": "2026-01-05T10:00:00.123456Z"}       timestamp (stored to the microsecond)
  {"$ref": "users/u1"}                         document reference; one in another
                                               database is its full name,
                                               "projects/P/databases/D/documents/users/u1"
  {"$double": 1.5}                             double ("NaN", "Infinity" as strings);
                                               a bare JSON integer is an integer
  {"$bytes": "aGk="}                           bytes (standard base64 with padding)
  {"$geo": {"lat": 1.5, "lng": -2.0}}          geo point
  {"$map": {...}}                              escape: a map whose only key starts
                                               with $ is written inside $map
  --plain prints untagged JSON for humans and jq (it carries "_plain": true and `set`
  refuses it: it cannot be read back losslessly). `query --select` documents carry
  "_select": [...]; `set --overwrite` refuses them (they hold only some fields),
  `set --merge` accepts them.
  `set --data` takes a bare object of fields or a whole document as printed by
  `get` (its _path must match the target); use --data @file.json or --data - (stdin).
  A whole document is recognised by a _path, _updateTime, _plain or _select key, and then
  its fields must sit inside "data" (any other top-level key is an error). Any other
  object is bare field data (so {"data": {...}} alone is a field named data).

Concurrency:
  --if-unchanged-since takes the _updateTime printed by `get`, verbatim (nanoseconds
  included). The write fails (exit 5) if the document changed or is missing.
  `set --overwrite|--merge` with a whole document from `get` (it carries _updateTime)
  uses that time as the guard by itself: get, edit, set fails with exit 5 if the
  document changed meanwhile. --force skips this and writes anyway.

Environment:
  FIRECTL_PROJECT    default for --project
  FIRECTL_DATABASE   default for --database (otherwise "(default)")
  Flags win over environment variables.

Completion:
  Add `source <(FIRECTL_COMPLETE=zsh firectl)` (or `FIRECTL_COMPLETE=bash`) to your rc file. TAB then
  completes collection ids and field names from the database (needs --project or
  FIRECTL_PROJECT; answers are cached for 10 minutes in $XDG_CACHE_HOME/firectl).

Output:
  A table on a terminal, JSON otherwise (indented on a terminal, compact in a pipe);
  jsonl is always compact. --format, --pretty, --compact and --plain override.
  `query` stops at 100 documents unless --limit or --all; when it returns exactly 100 by
  default, a note on stderr says there may be more (stdout stays clean).

Exit codes:
  (the process exit codes; match on these in scripts)
  0  ok
  1  other error (including a missing database or project)
  2  usage error (bad flag, path, filter, field op or --data)
  3  document not found
  4  already exists
  5  precondition failed (missing or changed since --if-unchanged-since)
  6  writes disabled for this database
  7  missing index (the error lists the index to create)
  8  authentication or permission failure (login: gcloud auth application-default login;
     permission: grant the account Firestore access)
"#;

/// Query Firestore from the command line.
///
/// Reads work everywhere; writes (`set`, `update`, `delete`) work only on databases
/// the operator has enabled for writing, and fail with exit code 6 otherwise.
#[derive(Debug, Parser)]
#[command(name = "firectl", version, after_long_help = LONG_HELP)]
pub struct Cli {
    #[command(flatten)]
    pub conn: ConnArgs,
    #[command(flatten)]
    pub out: OutArgs,
    #[command(subcommand)]
    pub cmd: Cmd,
}

#[derive(Debug, Clone, Args)]
#[command(next_help_heading = "Global options")]
pub struct ConnArgs {
    /// Google Cloud project id. Default: the `FIRECTL_PROJECT` environment variable; required
    /// unless that is set.
    #[arg(long, global = true, value_name = "PROJECT")]
    pub project: Option<ProjectId>,
    /// Firestore database id. Default: the `FIRECTL_DATABASE` environment variable, else
    /// `(default)`.
    #[arg(long, global = true, value_name = "DATABASE")]
    pub database: Option<DatabaseId>,
}

impl ConnArgs {
    pub fn target(&self, env: impl Fn(&str) -> Option<String>) -> Result<Option<DbTarget>, String> {
        fn resolve<T: FromStr<Err = IdError> + Clone>(
            flag: Option<&T>,
            name: &str,
            env: &impl Fn(&str) -> Option<String>,
        ) -> Result<Option<T>, String> {
            match flag {
                Some(value) => Ok(Some(value.clone())),
                None => env(name)
                    .filter(|v| !v.is_empty())
                    .map(|v| v.parse().map_err(|e| format!("invalid {name}: {e}")))
                    .transpose(),
            }
        }
        let project = resolve(self.project.as_ref(), "FIRECTL_PROJECT", &env)?;
        let database =
            resolve(self.database.as_ref(), "FIRECTL_DATABASE", &env)?.unwrap_or_default();
        Ok(project.map(|project| DbTarget { project, database }))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CompletionShell {
    /// Bash.
    Bash,
    /// Zsh.
    Zsh,
}

impl CompletionShell {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum FormatArg {
    /// Human-readable table.
    Table,
    /// One JSON value.
    Json,
    /// One compact JSON document per line.
    Jsonl,
}

#[derive(Debug, Clone, Args)]
#[command(next_help_heading = "Global options")]
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
    fn render_opts(&self, is_tty: bool, default: Format) -> RenderOpts {
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
        RenderOpts::resolve(is_tty, format, layout, typing, default)
    }
}

impl Cli {
    #[must_use]
    pub fn render_opts(&self, is_tty: bool) -> RenderOpts {
        self.out.render_opts(is_tty, self.cmd.default_format())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DataArg {
    Inline(String),
    File(PathBuf),
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

/// Strict like `--where`: `name age` is an error, not one field.
fn strict_field(text: &str) -> Result<FieldPath, String> {
    parse_field(
        text,
        "backtick-quote field names with spaces or punctuation, e.g. `my-field`",
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldList(Vec<FieldPath>);

impl FromStr for FieldList {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, String> {
        split_unquoted(s, '`', ',')
            .into_iter()
            .map(|name| strict_field(name.trim()))
            .try_collect()
            .map(Self)
    }
}

#[derive(Debug, Clone, Args)]
pub struct Selection {
    /// Collection to read, e.g. `users` or `users/u1/orders`. With --group: optionally
    /// a document, to search only below it.
    #[arg(value_name = "PATH", required_unless_present = "group", add = collection_completer())]
    path: Option<AnyPath>,
    /// Read every collection with this id at any depth (a bare id like `orders`).
    #[arg(long, value_name = "COLLECTION_ID", add = collection_completer())]
    group: Option<CollectionId>,
    /// Condition `<field> <op> <value>`, e.g. --where 'age >= 18'; repeat for AND. Quote the
    /// whole filter. Operators: == != < <= > >= in not-in array-contains
    /// array-contains-any; `in`, `not-in` and `array-contains-any` take a list `[a, b]`.
    /// Values: 18, 1.5, true, null, ts:2026-01-05T10:00:00Z, ref:users/u1, "quoted text".
    #[arg(long = "where", value_name = "FILTER", action = clap::ArgAction::Append, add = field_completer(FieldKind::Where))]
    where_: Vec<Filter>,
}

impl Selection {
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

#[derive(Debug, Subcommand)]
pub enum Cmd {
    /// Print one document.
    #[command(after_long_help = SEE_ROOT_HELP)]
    Get {
        /// Document path, e.g. `users/u1`.
        path: DocPath,
    },
    /// List documents of a collection. At most 100 unless --limit or --all.
    #[command(after_long_help = SEE_ROOT_HELP)]
    Query {
        #[command(flatten)]
        selection: Selection,
        /// Sort term `field` or `field:desc`; repeat for several.
        #[arg(long, value_name = "FIELD[:asc|desc]", action = clap::ArgAction::Append, add = field_completer(FieldKind::OrderBy))]
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
        #[arg(long, value_name = "FIELDS", add = field_completer(FieldKind::Select))]
        select: Option<FieldList>,
    },
    /// Count documents, or sum/average a numeric field, on the server.
    #[command(after_long_help = SEE_ROOT_HELP)]
    Count {
        #[command(flatten)]
        selection: Selection,
        /// Also sum this field; repeat for several fields.
        #[arg(long, value_name = "FIELD", value_parser = strict_field, action = clap::ArgAction::Append, add = field_completer(FieldKind::Single))]
        sum: Vec<FieldPath>,
        /// Also average this field; repeat for several fields.
        #[arg(long, value_name = "FIELD", value_parser = strict_field, action = clap::ArgAction::Append, add = field_completer(FieldKind::Single))]
        avg: Vec<FieldPath>,
    },
    /// List collection ids: the root ones, or those under a document.
    #[command(after_long_help = SEE_ROOT_HELP)]
    Collections {
        /// Document whose subcollections to list; omit for the root collections.
        parent: Option<DocPath>,
    },
    /// List the composite indexes of a collection id.
    #[command(after_long_help = SEE_ROOT_HELP)]
    Indexes {
        /// Collection id, e.g. `orders`.
        #[arg(add = collection_completer())]
        collection: CollectionId,
    },
    /// Write a whole document. Creates it; fails with exit 4 if it exists, unless
    /// --overwrite or --merge.
    #[command(after_long_help = SEE_ROOT_HELP)]
    Set {
        /// Document path, e.g. `users/u1`.
        path: DocPath,
        /// JSON (typed, see `firectl --help`): inline, `@file.json`, or `-` for stdin.
        #[arg(long, value_name = "JSON|@FILE|-")]
        data: DataArg,
        /// Replace the document if it exists.
        #[arg(long, group = "mode")]
        overwrite: bool,
        /// Merge the given fields into the document, creating it if missing.
        #[arg(long, group = "mode")]
        merge: bool,
        /// Only write if the document's _updateTime is exactly this (needs --overwrite
        /// or --merge). Without it, a --data document printed by `get` guards the write
        /// with its own _updateTime.
        #[arg(long, value_name = "UPDATE_TIME", requires = "mode")]
        if_unchanged_since: Option<UpdateTime>,
        /// Skip the automatic guard taken from the _updateTime in --data: overwrite or
        /// merge even if the document changed since it was read.
        #[arg(long, requires = "mode", conflicts_with = "if_unchanged_since")]
        force: bool,
    },
    /// Change fields of an existing document: --set field=value, field+=v, field-=v.
    #[command(after_long_help = SEE_ROOT_HELP)]
    Update {
        /// Document path, e.g. `users/u1`.
        path: DocPath,
        /// `f=v` sets a field; `f+=v` ARRAY-UNION (adds v to an array); `f-=v` ARRAY-REMOVE
        /// (removes v from an array); not numeric increments. Repeat for several changes.
        /// Values: 18, 1.5, true, null, ts:..., ref:..., "text", [a, b]. Backtick field
        /// names with spaces or punctuation: `my-field`=1. A value starting with `{` is
        /// rejected: `--set` cannot write a map.
        #[arg(
            long = "set",
            value_name = "OP",
            required = true,
            action = clap::ArgAction::Append,
            add = field_completer(FieldKind::Single)
        )]
        set: Vec<FieldOp>,
        /// Only write if the document's _updateTime is exactly this.
        #[arg(long, value_name = "UPDATE_TIME")]
        if_unchanged_since: Option<UpdateTime>,
    },
    /// Delete a document (fails with exit 3 if it does not exist).
    #[command(after_long_help = SEE_ROOT_HELP)]
    Delete {
        /// Document path, e.g. `users/u1`.
        path: DocPath,
        /// Only delete if the document's _updateTime is exactly this.
        #[arg(long, value_name = "UPDATE_TIME")]
        if_unchanged_since: Option<UpdateTime>,
    },
    /// Print the line that enables shell completion (add it to your shell's rc file).
    #[command(after_long_help = SEE_ROOT_HELP)]
    Completions {
        /// The shell to enable completion in.
        shell: CompletionShell,
    },
}

impl Cmd {
    #[must_use]
    pub fn is_write(&self) -> bool {
        match self {
            Self::Set { .. } | Self::Update { .. } | Self::Delete { .. } => true,
            Self::Get { .. }
            | Self::Query { .. }
            | Self::Count { .. }
            | Self::Collections { .. }
            | Self::Indexes { .. }
            | Self::Completions { .. } => false,
        }
    }
}

impl Cmd {
    // Every command names its default: a new command must choose one here.
    #[must_use]
    pub(crate) fn default_format(&self) -> Format {
        match self {
            Self::Get { .. } => Format::Json,
            Self::Query { .. }
            | Self::Count { .. }
            | Self::Collections { .. }
            | Self::Indexes { .. }
            | Self::Set { .. }
            | Self::Update { .. }
            | Self::Delete { .. }
            | Self::Completions { .. } => Format::Jsonl,
        }
    }
}

impl Cmd {
    #[must_use]
    pub fn uses_default_limit(&self) -> bool {
        matches!(
            self,
            Self::Query {
                limit: None,
                all: false,
                ..
            }
        )
    }
}

#[must_use]
pub fn truncation_note(default_limit_used: bool, output: &Output) -> Option<String> {
    if let Output::Documents { docs, .. } = output
        && default_limit_used
        && docs.len() == DEFAULT_LIMIT.get() as usize
    {
        Some(format!(
            "note: showing the first {DEFAULT_LIMIT} documents; use --limit N or --all\n"
        ))
    } else {
        None
    }
}

fn guard(since: Option<UpdateTime>) -> Guard {
    since.map_or(Guard::None, Guard::UnchangedSince)
}

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

struct SetData {
    fields: Fields,
    read_at: Option<UpdateTime>,
}

const PLAIN_REFUSED: &str = "plain output cannot be written back; use typed output (omit --plain)";

fn check_markers(envelope: &Envelope, merge: bool) -> Result<(), AppError> {
    if envelope.plain {
        Err(AppError::Input(PLAIN_REFUSED.to_owned()))
    } else if envelope.select.is_some() && !merge {
        Err(AppError::Input(
            "--data is a `query --select` projection holding only the selected fields; \
             writing it without --merge would drop every other field. Use --merge to change \
             only these fields"
                .to_owned(),
        ))
    } else {
        Ok(())
    }
}

/// A full document's `_path` must name the target, so data copied from another document is not written here by accident.
fn decode_data(text: &str, target: &DocPath, merge: bool) -> Result<SetData, AppError> {
    let json: Json = serde_json::from_str(text)
        .map_err(|e| AppError::Input(format!("--data is not valid JSON: {e}")))?;
    // The marker is the real reason, so it outranks a shape error the same input also has.
    let Input { fields, envelope } = typed_json::parse_input(&json).map_err(|e| {
        if json.get("_plain").is_some() {
            AppError::Input(PLAIN_REFUSED.to_owned())
        } else {
            AppError::Input(format!("--data {e}"))
        }
    })?;
    let Some(envelope) = envelope else {
        return Ok(SetData {
            fields,
            read_at: None,
        });
    };
    check_markers(&envelope, merge)?;
    match envelope.path {
        Some(path) if path != *target => Err(AppError::Input(format!(
            "_path in --data ({path}) does not match target {target}"
        ))),
        _ => Ok(SetData {
            fields,
            read_at: envelope.update_time,
        }),
    }
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
    /// `stdin` is read only by `set --data -`.
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
                force,
            } => {
                let SetData { fields, read_at } =
                    read_data(data, stdin).and_then(|text| decode_data(&text, &path, merge))?;
                // Lost-update guard: a document read with `get` guards its own write-back unless a guard or --force is given.
                let guard = guard(if_unchanged_since.or(read_at.filter(|_| !force)));
                let mode = match (overwrite, merge) {
                    (true, _) => SetMode::Overwrite(guard),
                    (_, true) => SetMode::Merge(guard),
                    _ => SetMode::Create,
                };
                Ok(Request::Write(Write::Set(SetDoc {
                    path,
                    data: fields,
                    mode,
                })))
            }
            Cmd::Update {
                path,
                set,
                if_unchanged_since,
            } => NonEmptyVec::try_from(set)
                .map_err(|e| AppError::Input(e.to_string()))
                .and_then(|ops| {
                    check_field_ops(&ops).map_err(|e| AppError::Input(e.to_string()))?;
                    Ok(ops)
                })
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
mod tests {
    use std::io::{Cursor, empty};

    use super::*;
    use crate::domain::{Fields, Value};

    fn request(args: &[&str]) -> Result<Request, AppError> {
        request_with_stdin(args, "")
    }

    fn parsed(args: &[&str]) -> Cli {
        Cli::try_parse_from(
            ["firectl", "--project", "p"]
                .into_iter()
                .chain(args.iter().copied()),
        )
        .unwrap()
    }

    fn request_with_stdin(args: &[&str], stdin: &str) -> Result<Request, AppError> {
        parsed(args).into_request(Cursor::new(stdin.to_owned()))
    }

    fn query_of(args: &[&str]) -> Query {
        let request = request(args).unwrap();
        let Request::Query(q) = request else {
            panic!("not a query: {request:?}")
        };
        q
    }

    fn set_of(args: &[&str]) -> SetDoc {
        let request = request(args).unwrap();
        let Request::Write(Write::Set(s)) = request else {
            panic!("not a set: {request:?}")
        };
        s
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
        assert!(Cli::try_parse_from(["firectl", "query", "u", "--all", "--limit", "5"]).is_err());
        assert!(Cli::try_parse_from(["firectl", "query", "u", "--limit", "0"]).is_err());
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
                "firectl",
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
        let dir = std::env::temp_dir().join(format!("firectl-cli-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let file = dir.join("doc.json");
        fs::write(&file, r#"{"a":1}"#).unwrap();
        let arg = format!("@{}", file.display());
        assert_eq!(set_of(&["set", "users/u1", "--data", &arg]).data, expected);
        fs::remove_dir_all(&dir).unwrap();
        let missing = request(&["set", "users/u1", "--data", "@/nonexistent/firectl.json"]);
        assert!(
            matches!(missing, Err(AppError::Input(m)) if m.contains("/nonexistent/firectl.json"))
        );
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
    fn merge_data_writes_a_map_field() {
        let s = set_of(&["set", "users/u1", "--merge", "--data", r#"{"a":{"x":1}}"#]);
        let map = Fields::from([("x".to_owned(), Value::Int(1))]);
        assert_eq!(s.data, Fields::from([("a".to_owned(), Value::Map(map))]));
        assert_eq!(s.mode, SetMode::Merge(Guard::None));
    }

    #[test]
    fn update_rejects_an_unquoted_brace_value_as_a_usage_error() {
        let e = Cli::try_parse_from([
            "firectl",
            "--project",
            "p",
            "update",
            "users/u1",
            "--set",
            r#"a={"x":1}"#,
        ])
        .unwrap_err();
        assert_eq!(e.kind(), clap::error::ErrorKind::ValueValidation);
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
            "old-=[x]",
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
                    ["firectl", "--project", "p"]
                        .into_iter()
                        .chain(args.iter().copied())
                )
                .is_err(),
                "{args:?}"
            );
        }
    }

    #[test]
    fn target_resolves_flags_then_env_then_default() {
        let no_env = |_: &str| None;
        let parse = |args: &[&str]| {
            Cli::try_parse_from(["firectl"].into_iter().chain(args.iter().copied()))
                .unwrap()
                .conn
        };
        let show = |t: Result<Option<DbTarget>, String>| {
            t.map(|t| t.map(|t| format!("{}/{}", t.project, t.database)))
        };
        let env = |name: &str| match name {
            "FIRECTL_PROJECT" => Some("ep".to_owned()),
            "FIRECTL_DATABASE" => Some("ed".to_owned()),
            _ => None,
        };
        let empty = |_: &str| Some(String::new());
        let conn = parse(&["get", "u/1", "--project", "q", "--database", "d"]);
        assert_eq!(show(conn.target(env)), Ok(Some("q/d".into())));
        let conn = parse(&["get", "u/1"]);
        assert_eq!(show(conn.target(env)), Ok(Some("ep/ed".into())));
        assert_eq!(show(conn.target(no_env)), Ok(None));
        assert_eq!(show(conn.target(empty)), Ok(None));
        let conn = parse(&["get", "u/1", "--project", "p"]);
        assert_eq!(show(conn.target(no_env)), Ok(Some("p/(default)".into())));
        assert_eq!(show(conn.target(empty)), Ok(Some("p/(default)".into())));
        let bad = |_: &str| Some("Bad Name".to_owned());
        assert!(matches!(conn.target(bad), Err(m) if m.contains("FIRECTL_DATABASE")));
    }

    #[test]
    fn render_opts_map_flags() {
        let opts = |args: &[&str], tty| {
            Cli::try_parse_from(["firectl"].into_iter().chain(args.iter().copied()))
                .unwrap()
                .render_opts(tty)
        };
        for tty in [true, false] {
            let query = opts(&["query", "users"], tty);
            assert_eq!(
                (query.format, query.layout),
                (Format::Jsonl, Layout::Compact)
            );
        }
        let get = |tty| opts(&["get", "u/1"], tty);
        assert_eq!(
            (get(true).format, get(true).layout),
            (Format::Json, Layout::Pretty)
        );
        assert_eq!(
            (get(false).format, get(false).layout),
            (Format::Json, Layout::Compact)
        );
        assert_eq!(
            opts(&["get", "u/1", "--pretty"], false).layout,
            Layout::Pretty
        );
        let compact_tty = opts(&["get", "u/1", "--compact"], true);
        assert_eq!(
            (compact_tty.format, compact_tty.layout),
            (Format::Json, Layout::Compact)
        );
        let piped_pretty = opts(&["query", "users", "--pretty"], false);
        assert_eq!(
            (piped_pretty.format, piped_pretty.layout),
            (Format::Jsonl, Layout::Compact)
        );
        let json_tty = opts(&["query", "users", "--format", "json"], true);
        assert_eq!(
            (json_tty.format, json_tty.layout),
            (Format::Json, Layout::Pretty)
        );
        assert_eq!(
            opts(&["query", "users", "--format", "table"], true).format,
            Format::Table
        );
        assert_eq!(
            opts(&["get", "u/1", "--format", "jsonl", "--plain"], true).format,
            Format::Jsonl
        );
        assert_eq!(opts(&["get", "u/1", "--plain"], true).typing, Typing::Plain);
        assert_eq!(opts(&["get", "u/1"], true).typing, Typing::Typed);
    }

    #[test]
    fn only_get_defaults_to_json() {
        let default = |args: &[&str]| {
            Cli::try_parse_from(["firectl"].into_iter().chain(args.iter().copied()))
                .unwrap()
                .cmd
                .default_format()
        };
        assert_eq!(default(&["get", "u/1"]), Format::Json);
        for args in [
            &["query", "users"][..],
            &["count", "users"],
            &["collections"],
            &["indexes", "orders"],
            &["set", "u/1", "--data", "{}"],
            &["update", "u/1", "--set", "a=1"],
            &["delete", "u/1"],
            &["completions", "bash"],
        ] {
            assert_eq!(default(args), Format::Jsonl, "{args:?}");
        }
    }

    #[test]
    fn completions_is_not_a_request() {
        let cli = Cli::try_parse_from(["firectl", "completions", "bash"]).unwrap();
        assert!(matches!(cli.into_request(empty()), Err(AppError::Input(_))));
    }

    const T: &str = "2026-01-05T10:00:00.123456789Z";

    #[derive(Clone, Copy)]
    enum Stored {
        FieldA,
        FieldData,
    }

    #[derive(Clone, Copy)]
    enum Cell {
        Writes(SetMode, Stored),
        Rejects(&'static str),
    }

    // Columns: create, --overwrite, --merge, --overwrite --force, --merge --force.
    const MODES: [&[&str]; 5] = [
        &[],
        &["--overwrite"],
        &["--merge"],
        &["--overwrite", "--force"],
        &["--merge", "--force"],
    ];

    fn writes_with(stored: Stored, implicit: Guard) -> [Cell; 5] {
        [
            Cell::Writes(SetMode::Create, stored),
            Cell::Writes(SetMode::Overwrite(implicit), stored),
            Cell::Writes(SetMode::Merge(implicit), stored),
            Cell::Writes(SetMode::Overwrite(Guard::None), stored),
            Cell::Writes(SetMode::Merge(Guard::None), stored),
        ]
    }

    fn only_merge_writes(guard: Guard) -> [Cell; 5] {
        let no = Cell::Rejects("--merge");
        [
            no,
            no,
            Cell::Writes(SetMode::Merge(guard), Stored::FieldA),
            no,
            Cell::Writes(SetMode::Merge(Guard::None), Stored::FieldA),
        ]
    }

    #[test]
    fn set_data_shape_and_mode_decide_what_is_written() {
        let t: UpdateTime = T.parse().unwrap();
        let guarded = Guard::UnchangedSince(t);
        let full = |extra: &str| {
            format!(r#"{{"_path":"users/u1","_updateTime":"{T}",{extra}"data":{{"a":1}}}}"#)
        };
        let a: Fields = [("a".to_owned(), Value::Int(1))].into();
        let data: Fields = [("data".to_owned(), Value::Map(a))].into();
        let stored = |s: Stored| match s {
            Stored::FieldA => [("a".to_owned(), Value::Int(1))].into(),
            Stored::FieldData => data.clone(),
        };
        let reject = |msg| [Cell::Rejects(msg); 5];
        let shapes = [
            (
                "A",
                r#"{"a":1}"#.to_owned(),
                writes_with(Stored::FieldA, Guard::None),
            ),
            (
                "B",
                r#"{"data":{"a":1}}"#.to_owned(),
                writes_with(Stored::FieldData, Guard::None),
            ),
            ("C", full(""), writes_with(Stored::FieldA, guarded)),
            (
                "D",
                r#"{"_path":"users/u1","data":{"a":1}}"#.to_owned(),
                writes_with(Stored::FieldA, Guard::None),
            ),
            (
                "E",
                format!(r#"{{"_updateTime":"{T}","data":{{"a":1}}}}"#),
                writes_with(Stored::FieldA, guarded),
            ),
            ("F", full(r#""_plain":true,"#), reject("plain output")),
            ("G", full(r#""_select":["a"],"#), only_merge_writes(guarded)),
            (
                "H",
                r#"{"_select":["a"],"data":{"a":1}}"#.to_owned(),
                only_merge_writes(Guard::None),
            ),
            (
                "I",
                r#"{"_plain":true,"data":{"a":1}}"#.to_owned(),
                reject("plain output"),
            ),
            (
                "J",
                r#"{"_select":["a"],"a":1}"#.to_owned(),
                reject("inside `data`"),
            ),
            (
                "K",
                r#"{"_plain":true,"a":1}"#.to_owned(),
                reject("plain output"),
            ),
            (
                "L",
                full("").replace("users/u1", "users/u2"),
                reject("does not match"),
            ),
            ("M", full(r#""x":1,"#), reject("inside `data`")),
            (
                "N",
                r#"{"_path":"users/u1"}"#.to_owned(),
                reject("`data` object"),
            ),
            (
                "O",
                r#"{"_select":["a"],"b":1,"data":{"a":1}}"#.to_owned(),
                reject("inside `data`"),
            ),
        ];
        for (id, json, row) in shapes {
            for (mode, cell) in MODES.iter().zip(row) {
                let args = [&["set", "users/u1", "--data", &json][..], mode].concat();
                let got = request(&args);
                match cell {
                    Cell::Writes(mode_written, fields) => assert_eq!(
                        got,
                        Ok(Request::Write(Write::Set(SetDoc {
                            path: "users/u1".parse().unwrap(),
                            data: stored(fields),
                            mode: mode_written,
                        }))),
                        "shape {id} with {mode:?}"
                    ),
                    Cell::Rejects(why) => assert!(
                        matches!(&got, Err(AppError::Input(m)) if m.contains(why)),
                        "shape {id} with {mode:?} gave {got:?}, wanted exit 2 mentioning {why}"
                    ),
                }
            }
        }
    }

    #[test]
    fn an_explicit_guard_replaces_the_one_a_printed_document_carries() {
        let other: UpdateTime = "2026-02-01T00:00:00Z".parse().unwrap();
        let printed = format!(r#"{{"_path":"users/u1","_updateTime":"{T}","data":{{"a":1}}}}"#);
        let mode = set_of(&[
            "set",
            "users/u1",
            "--data",
            &printed,
            "--overwrite",
            "--if-unchanged-since",
            "2026-02-01T00:00:00Z",
        ])
        .mode;
        assert_eq!(mode, SetMode::Overwrite(Guard::UnchangedSince(other)));
    }

    #[test]
    fn force_conflicts_with_an_explicit_guard_and_needs_a_mode() {
        let parse = |args: &[&str]| {
            Cli::try_parse_from(
                ["firectl", "set", "u/1", "--data", "{}"]
                    .into_iter()
                    .chain(args.iter().copied()),
            )
        };
        assert!(
            parse(&[
                "--overwrite",
                "--force",
                "--if-unchanged-since",
                "2026-01-01T00:00:00Z"
            ])
            .is_err()
        );
        assert!(parse(&["--force"]).is_err());
        assert!(parse(&["--merge", "--force"]).is_ok());
    }

    #[test]
    fn only_a_plain_query_uses_the_default_limit() {
        let default = |args: &[&str]| {
            Cli::try_parse_from(
                ["firectl", "--project", "p"]
                    .into_iter()
                    .chain(args.iter().copied()),
            )
            .unwrap()
            .cmd
            .uses_default_limit()
        };
        assert!(default(&["query", "users"]));
        assert!(!default(&["query", "users", "--limit", "100"]));
        assert!(!default(&["query", "users", "--all"]));
        assert!(!default(&["get", "users/u1"]));
        assert!(!default(&["count", "users"]));
    }

    fn documents(n: usize) -> Output {
        let doc = |i: usize| crate::domain::Document {
            path: format!("users/u{i}").parse().unwrap(),
            update_time: "2026-01-01T00:00:00Z".parse().unwrap(),
            data: Fields::new(),
        };
        Output::Documents {
            docs: (0..n).map(doc).collect(),
            select: None,
        }
    }

    #[test]
    fn a_default_limited_query_that_filled_the_limit_says_so() {
        assert_eq!(
            truncation_note(true, &documents(100)),
            Some("note: showing the first 100 documents; use --limit N or --all\n".to_owned())
        );
        assert_eq!(truncation_note(true, &documents(99)), None);
        assert_eq!(truncation_note(true, &documents(0)), None);
        assert_eq!(truncation_note(false, &documents(100)), None);
        assert_eq!(truncation_note(true, &Output::Collections(vec![])), None);
    }
}
