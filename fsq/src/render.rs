//! Rendering: turns an [`Output`] or [`AppError`] into the exact text to print.
//! Pure: no I/O, no environment; `main` prints the returned strings.

use std::iter;
use std::slice;

use comfy_table::{ContentArrangement, Table, presets};
use itertools::Itertools;
use serde_json::{Map, Value as Json, json};

use crate::app::{AppError, Output};
use crate::codec::{plain_json, typed_json};
use crate::domain::{Aggregation, DbTarget, Document, IndexMode, QueryScope};
use crate::port::{AggregateResult, GatewayError, IndexInfo, IndexState, Written};

/// Shown in a table cell where a document has no such field.
const MISSING_CELL: &str = "—";

/// How results are laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    /// A human-readable table.
    Table,
    /// One JSON value.
    Json,
    /// One compact JSON document per line.
    Jsonl,
}

/// Whether JSON is indented.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Layout {
    /// Indented, multi-line.
    Pretty,
    /// A single line.
    Compact,
}

/// Whether JSON values carry type tags (`$ts`, `$double`, ...).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Typing {
    /// Lossless typed JSON.
    Typed,
    /// Untagged JSON for humans and `jq`.
    Plain,
}

/// Fully resolved output options.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenderOpts {
    /// Table, JSON or JSON lines.
    pub format: Format,
    /// JSON indentation.
    pub layout: Layout,
    /// JSON type tags.
    pub typing: Typing,
}

impl RenderOpts {
    /// Applies the defaults for a terminal (`is_tty`) or a pipe, then the flags.
    ///
    /// Format defaults to a table on a terminal and JSON otherwise. JSON is indented
    /// on a terminal and compact otherwise; `jsonl` is always compact, and `compact`
    /// wins over `pretty` if both are given. `plain` drops the type tags.
    #[must_use]
    // The booleans mirror the independent CLI flags one to one.
    #[allow(clippy::fn_params_excessive_bools)]
    pub fn resolve(
        is_tty: bool,
        format: Option<Format>,
        pretty: bool,
        compact: bool,
        plain: bool,
    ) -> Self {
        let format = format.unwrap_or(if is_tty { Format::Table } else { Format::Json });
        let layout = if format == Format::Jsonl || compact {
            Layout::Compact
        } else if pretty || is_tty {
            Layout::Pretty
        } else {
            Layout::Compact
        };
        let typing = if plain { Typing::Plain } else { Typing::Typed };
        Self {
            format,
            layout,
            typing,
        }
    }
}

/// Text for stdout. Every line, the last included, ends with `\n`; an empty result
/// of `jsonl` is the empty string. Print it as is.
#[must_use]
pub fn render(o: &Output, opts: &RenderOpts) -> String {
    match opts.format {
        Format::Table => table_text(&grid(o)),
        Format::Json => {
            let json = match shape(o, opts.typing) {
                Shape::One(j) => j,
                Shape::Many(js) => Json::Array(js),
            };
            let text = match opts.layout {
                Layout::Pretty => format!("{json:#}"),
                Layout::Compact => json.to_string(),
            };
            format!("{text}\n")
        }
        Format::Jsonl => shape(o, opts.typing)
            .records()
            .into_iter()
            .map(|j| j.to_string() + "\n")
            .collect(),
    }
}

/// Text for stderr, without a trailing newline: the error's message once, plus what
/// to do about it (the `gcloud` command and console link for a missing index, the
/// login command for an authentication failure).
#[must_use]
pub fn render_error(e: &AppError, t: &DbTarget) -> String {
    let message = e.to_string();
    match e {
        AppError::MissingIndex {
            advice,
            console_url,
        } => iter::once(message)
            .chain(advice.as_ref().map(|a| a.gcloud_command(t)))
            .chain(console_url.clone())
            .join("\n"),
        AppError::Gateway(GatewayError::MissingIndex { console_url }) => {
            iter::once(message).chain(console_url.clone()).join("\n")
        }
        AppError::Gateway(GatewayError::Auth(_)) => {
            format!("{message}\nrun: gcloud auth application-default login")
        }
        _ => message,
    }
}

/// The JSON form of an output: one value, or a list of them.
enum Shape {
    One(Json),
    Many(Vec<Json>),
}

impl Shape {
    /// The JSON values, one per `jsonl` line.
    fn records(self) -> Vec<Json> {
        match self {
            Self::One(j) => vec![j],
            Self::Many(js) => js,
        }
    }
}

fn shape(o: &Output, typing: Typing) -> Shape {
    match o {
        Output::Document(d) => Shape::One(document_json(d, typing)),
        Output::Documents(ds) => Shape::Many(ds.iter().map(|d| document_json(d, typing)).collect()),
        Output::Aggregate(r) => Shape::One(object(aggregate_entries(r, typing))),
        Output::Collections(cs) => {
            Shape::Many(cs.iter().map(|c| Json::String(c.to_string())).collect())
        }
        Output::Indexes(is) => Shape::Many(is.iter().map(index_json).collect()),
        Output::Written { path, result } => {
            Shape::One(object(written_entries(&path.to_string(), *result)))
        }
    }
}

fn document_json(d: &Document, typing: Typing) -> Json {
    match typing {
        Typing::Typed => typed_json::document_to_json(d),
        Typing::Plain => plain_json::document_to_json(d),
    }
}

fn object(entries: Vec<(String, Json)>) -> Json {
    Json::Object(entries.into_iter().collect::<Map<_, _>>())
}

/// `count`, `sum_<field>` and `avg_<field>` keys with their values, in request order
/// (a table keeps that order; JSON objects print keys sorted).
fn aggregate_entries(r: &AggregateResult, typing: Typing) -> Vec<(String, Json)> {
    r.entries()
        .map(|(aggregation, value)| {
            let key = match aggregation {
                Aggregation::Count => "count".to_owned(),
                Aggregation::Sum(f) => format!("sum_{}", f.to_firestore()),
                Aggregation::Avg(f) => format!("avg_{}", f.to_firestore()),
            };
            let value = match typing {
                Typing::Typed => typed_json::value_to_json(value),
                Typing::Plain => plain_json::value_to_json(value),
            };
            (key, value)
        })
        .collect()
}

fn written_entries(path: &str, result: Written) -> Vec<(String, Json)> {
    let outcome = match result {
        Written::Updated(t) => ("_updateTime".to_owned(), Json::String(t.to_string())),
        Written::Deleted => ("deleted".to_owned(), Json::Bool(true)),
    };
    vec![("_path".to_owned(), Json::String(path.to_owned())), outcome]
}

fn index_json(i: &IndexInfo) -> Json {
    json!({
        "collection_group": i.spec.collection_group.to_string(),
        "scope": scope_name(i.spec.scope),
        "fields": i.spec.fields.iter().map(|f| json!({
            "field": f.field.to_firestore(),
            "mode": mode_name(f.mode),
        })).collect::<Vec<_>>(),
        "state": state_name(i.state),
    })
}

fn scope_name(s: QueryScope) -> &'static str {
    match s {
        QueryScope::Collection => "collection",
        QueryScope::CollectionGroup => "collection-group",
    }
}

fn mode_name(m: IndexMode) -> &'static str {
    match m {
        IndexMode::Asc => "asc",
        IndexMode::Desc => "desc",
        IndexMode::ArrayContains => "array-contains",
    }
}

fn state_name(s: IndexState) -> &'static str {
    match s {
        IndexState::Ready => "ready",
        IndexState::Creating => "creating",
        IndexState::NeedsRepair => "needs-repair",
        IndexState::Unknown => "unknown",
    }
}

/// A table's header and rows, as the text of each cell.
struct Grid {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

fn grid(o: &Output) -> Grid {
    match o {
        Output::Document(d) => documents_grid(slice::from_ref(d)),
        Output::Documents(ds) => documents_grid(ds),
        Output::Aggregate(r) => entries_grid(&aggregate_entries(r, Typing::Plain)),
        Output::Collections(cs) => Grid {
            header: vec!["collection".to_owned()],
            rows: cs.iter().map(|c| vec![c.to_string()]).collect(),
        },
        Output::Indexes(is) => Grid {
            header: ["collection_group", "scope", "fields", "state"]
                .map(str::to_owned)
                .to_vec(),
            rows: is
                .iter()
                .map(|i| {
                    vec![
                        i.spec.collection_group.to_string(),
                        scope_name(i.spec.scope).to_owned(),
                        i.spec
                            .fields
                            .iter()
                            .map(|f| format!("{} {}", f.field.to_firestore(), mode_name(f.mode)))
                            .join(", "),
                        state_name(i.state).to_owned(),
                    ]
                })
                .collect(),
        },
        Output::Written { path, result } => {
            entries_grid(&written_entries(&path.to_string(), *result))
        }
    }
}

/// `_path` plus the union of the documents' top-level keys (first-seen order).
fn documents_grid(ds: &[Document]) -> Grid {
    let columns: Vec<&String> = ds.iter().flat_map(|d| d.data.keys()).unique().collect();
    Grid {
        header: iter::once("_path".to_owned())
            .chain(columns.iter().map(|c| (*c).clone()))
            .collect(),
        rows: ds
            .iter()
            .map(|d| {
                iter::once(d.path.to_string())
                    .chain(columns.iter().map(|c| {
                        d.data.get(*c).map_or_else(
                            || MISSING_CELL.to_owned(),
                            |v| cell(&plain_json::value_to_json(v)),
                        )
                    }))
                    .collect()
            })
            .collect(),
    }
}

/// One row: keys as columns, values as cells.
fn entries_grid(entries: &[(String, Json)]) -> Grid {
    Grid {
        header: entries.iter().map(|(k, _)| k.clone()).collect(),
        rows: vec![entries.iter().map(|(_, v)| cell(v)).collect()],
    }
}

/// Strings raw, everything else as compact JSON.
fn cell(j: &Json) -> String {
    match j {
        Json::String(s) => s.clone(),
        other => other.to_string(),
    }
}

fn table_text(g: &Grid) -> String {
    let mut table = Table::new();
    table
        .load_preset(presets::UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Disabled)
        .set_header(&g.header)
        .add_rows(&g.rows);
    format!("{table}\n")
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use crate::domain::{
        Aggregation, Document, Fields, Filter, OrderBy, Query, Scope, Value, required_index,
    };
    use crate::policy::WritePolicy;
    use crate::port::{AggregateResult, GatewayError, IndexInfo, IndexState, Written};
    use serde_json::{Value as Json, json};

    fn doc(path: &str, data: &[(&str, Value)]) -> Document {
        Document {
            path: path.parse().unwrap(),
            update_time: "2026-01-05T10:00:00.123456789Z".parse().unwrap(),
            data: data
                .iter()
                .map(|(k, v)| ((*k).to_owned(), v.clone()))
                .collect::<Fields>(),
        }
    }

    fn two_docs() -> Output {
        Output::Documents(vec![
            doc(
                "users/a",
                &[("name", "Ann".into()), ("score", Value::Double(1.5))],
            ),
            doc("users/b", &[("name", "Bob".into()), ("plan", "pro".into())]),
        ])
    }

    fn opts(format: Format, layout: Layout, typing: Typing) -> RenderOpts {
        RenderOpts {
            format,
            layout,
            typing,
        }
    }

    fn compact_json() -> RenderOpts {
        opts(Format::Json, Layout::Compact, Typing::Typed)
    }

    fn jsonl() -> RenderOpts {
        opts(Format::Jsonl, Layout::Compact, Typing::Typed)
    }

    fn table() -> RenderOpts {
        opts(Format::Table, Layout::Compact, Typing::Typed)
    }

    fn target() -> DbTarget {
        DbTarget {
            project: "p".parse().unwrap(),
            database: "d".parse().unwrap(),
        }
    }

    fn parse(s: &str) -> Json {
        serde_json::from_str(s).unwrap()
    }

    #[test]
    fn resolve_defaults() {
        assert_eq!(
            RenderOpts::resolve(true, None, false, false, false).format,
            Format::Table
        );
        let piped = RenderOpts::resolve(false, None, false, false, false);
        assert_eq!(
            (piped.format, piped.layout),
            (Format::Json, Layout::Compact)
        );
        assert_eq!(
            RenderOpts::resolve(true, Some(Format::Json), false, false, false).layout,
            Layout::Pretty
        );
        assert_eq!(
            RenderOpts::resolve(true, Some(Format::Jsonl), true, false, false).layout,
            Layout::Compact
        );
    }

    #[test]
    fn resolve_flags_override_defaults() {
        let o = RenderOpts::resolve(false, Some(Format::Json), true, false, false);
        assert_eq!(o.layout, Layout::Pretty);
        let o = RenderOpts::resolve(true, Some(Format::Json), false, true, false);
        assert_eq!(o.layout, Layout::Compact);
        assert_eq!(
            RenderOpts::resolve(true, None, false, false, false).typing,
            Typing::Typed
        );
        assert_eq!(
            RenderOpts::resolve(true, None, false, false, true).typing,
            Typing::Plain
        );
    }

    #[test]
    fn compact_json_is_one_line() {
        let out = render(&two_docs(), &compact_json());
        assert!(!out.trim_end().contains('\n'));
        assert!(out.ends_with('\n'));
        assert_eq!(parse(&out).as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn pretty_json_is_indented() {
        let out = render(
            &two_docs(),
            &opts(Format::Json, Layout::Pretty, Typing::Typed),
        );
        assert!(out.trim_end().contains('\n'));
        assert_eq!(parse(&out).as_array().map(Vec::len), Some(2));
    }

    #[test]
    fn jsonl_one_doc_per_line() {
        let out = render(&two_docs(), &jsonl());
        assert_eq!(out.lines().count(), 2);
        assert!(out.lines().all(|l| parse(l)["_path"].is_string()));
        assert_eq!(render(&Output::Documents(vec![]), &jsonl()), "");
    }

    #[test]
    fn json_of_a_single_document_is_an_object() {
        let out = render(
            &Output::Document(doc("users/a", &[("n", Value::Int(1))])),
            &compact_json(),
        );
        assert_eq!(
            parse(&out),
            json!({"_path": "users/a", "_updateTime": "2026-01-05T10:00:00.123456789Z", "data": {"n": 1}})
        );
    }

    #[test]
    fn typing_selects_codec() {
        let o = Output::Document(doc("users/a", &[("x", Value::Double(1.5))]));
        let typed = parse(&render(&o, &compact_json()));
        assert_eq!(typed["data"]["x"], json!({"$double": 1.5}));
        let plain = parse(&render(
            &o,
            &opts(Format::Json, Layout::Compact, Typing::Plain),
        ));
        assert_eq!(plain["data"]["x"], json!(1.5));
    }

    /// The trimmed cells of one table line.
    fn cells(line: &str) -> Vec<String> {
        line.trim_matches('│')
            .split('┆')
            .map(|c| c.trim().to_owned())
            .collect()
    }

    #[test]
    fn table_has_path_and_union_columns() {
        let out = render(&two_docs(), &table());
        assert_eq!(
            cells(out.lines().nth(1).unwrap()),
            ["_path", "name", "score", "plan"]
        );
    }

    #[test]
    fn table_cells_are_plain_json_with_dash_for_missing() {
        let out = render(&two_docs(), &table());
        let row = |needle: &str| cells(out.lines().find(|l| l.contains(needle)).unwrap());
        assert_eq!(row("users/a"), ["users/a", "Ann", "1.5", "—"]);
        assert_eq!(row("users/b"), ["users/b", "Bob", "—", "pro"]);
    }

    #[test]
    fn table_renders_nested_values_as_compact_plain_json() {
        let nested = Value::Map(
            [(
                "k".to_owned(),
                Value::Array(vec![Value::Int(1), "x".into()]),
            )]
            .into_iter()
            .collect(),
        );
        let out = render(
            &Output::Document(doc("users/a", &[("m", nested)])),
            &table(),
        );
        assert!(out.contains(r#"{"k":[1,"x"]}"#), "{out}");
    }

    #[test]
    fn aggregate_json_is_a_flat_object() {
        let fp = |s: &str| s.parse().unwrap();
        let o = Output::Aggregate(AggregateResult::new(vec![
            (Aggregation::Count, Value::Int(42)),
            (Aggregation::Sum(fp("total")), Value::Int(10)),
            (Aggregation::Avg(fp("a.b")), Value::Double(2.5)),
        ]));
        assert_eq!(
            parse(&render(&o, &compact_json())),
            json!({"count": 42, "sum_total": 10, "avg_a.b": {"$double": 2.5}})
        );
        assert_eq!(
            render(&o, &compact_json()),
            "{\"avg_a.b\":{\"$double\":2.5},\"count\":42,\"sum_total\":10}\n"
        );
        assert_eq!(render(&o, &jsonl()).lines().count(), 1);
        let t = render(&o, &table());
        assert!(t.contains("count") && t.contains("sum_total") && t.contains("avg_a.b"));
        assert_eq!(t.lines().count(), 5, "{t}");
    }

    #[test]
    fn collections_render_as_ids() {
        let o = Output::Collections(vec!["users".parse().unwrap(), "teams".parse().unwrap()]);
        assert_eq!(
            parse(&render(&o, &compact_json())),
            json!(["users", "teams"])
        );
        assert_eq!(render(&o, &jsonl()), "\"users\"\n\"teams\"\n");
        let t = render(&o, &table());
        assert!(t.lines().nth(1).unwrap().contains("collection"));
        assert!(t.contains("users") && t.contains("teams"));
    }

    #[test]
    fn indexes_render_fields_and_state() {
        let q = Query {
            scope: Scope::Collection("users".parse().unwrap()),
            filters: vec!["status == a".parse::<Filter>().unwrap()],
            order: vec!["age:desc".parse::<OrderBy>().unwrap()],
            limit: None,
            offset: 0,
            select: None,
        };
        let spec = match required_index(&q) {
            Some(crate::domain::IndexAdvice::Composite(s)) => s,
            other => panic!("{other:?}"),
        };
        let o = Output::Indexes(vec![IndexInfo {
            spec,
            state: IndexState::Ready,
        }]);
        assert_eq!(
            parse(&render(&o, &compact_json())),
            json!([{
                "collection_group": "users",
                "scope": "collection",
                "fields": [
                    {"field": "status", "mode": "asc"},
                    {"field": "age", "mode": "desc"}
                ],
                "state": "ready"
            }])
        );
        let t = render(&o, &table());
        assert!(
            t.contains("status asc, age desc") && t.contains("ready"),
            "{t}"
        );
    }

    #[test]
    fn writes_render_path_and_update_time_or_deleted() {
        let updated = Output::Written {
            path: "users/a".parse().unwrap(),
            result: Written::Updated("2026-01-05T10:00:00.5Z".parse().unwrap()),
        };
        let stamp: crate::domain::UpdateTime = "2026-01-05T10:00:00.5Z".parse().unwrap();
        assert_eq!(
            parse(&render(&updated, &compact_json())),
            json!({"_path": "users/a", "_updateTime": stamp.to_string()})
        );
        let deleted = Output::Written {
            path: "users/a".parse().unwrap(),
            result: Written::Deleted,
        };
        assert_eq!(
            parse(&render(&deleted, &compact_json())),
            json!({"_path": "users/a", "deleted": true})
        );
        assert_eq!(
            render(&deleted, &compact_json()),
            "{\"_path\":\"users/a\",\"deleted\":true}\n"
        );
        let t = render(&deleted, &table());
        assert!(t.contains("deleted") && t.contains("true"), "{t}");
    }

    fn missing_index() -> AppError {
        let q = Query {
            scope: Scope::Collection("users".parse().unwrap()),
            filters: vec!["status == a".parse::<Filter>().unwrap()],
            order: vec!["age:desc".parse::<OrderBy>().unwrap()],
            limit: None,
            offset: 0,
            select: None,
        };
        AppError::MissingIndex {
            advice: required_index(&q),
            console_url: Some("https://console.firebase.google.com/idx".to_owned()),
        }
    }

    #[test]
    fn missing_index_error_shows_gcloud_command() {
        let out = render_error(&missing_index(), &target());
        assert!(
            out.contains("gcloud firestore indexes composite create"),
            "{out}"
        );
        assert!(out.contains("--database=d"));
        let lines: Vec<&str> = out.lines().collect();
        assert_eq!(lines.len(), 3, "{out}");
        assert!(lines[1].starts_with("gcloud "));
        assert_eq!(lines[2], "https://console.firebase.google.com/idx");
    }

    #[test]
    fn missing_index_without_advice_or_url_is_just_the_message() {
        let e = AppError::MissingIndex {
            advice: None,
            console_url: None,
        };
        assert_eq!(render_error(&e, &target()), e.to_string());
    }

    #[test]
    fn auth_error_suggests_login() {
        let e = AppError::Gateway(GatewayError::Auth("expired".to_owned()));
        let out = render_error(&e, &target());
        assert!(out.contains("authentication failed: expired"));
        assert!(out.contains("gcloud auth application-default login"));
    }

    #[test]
    fn writes_disabled_is_exactly_its_display_without_env_hint() {
        let denied = WritePolicy::from_env_value(None)
            .unwrap()
            .authorize(&target())
            .unwrap_err();
        let out = render_error(&AppError::WritesDisabled(denied), &target());
        assert_eq!(out, "writes are disabled for database p/d");
        assert!(!out.contains("FSQ_WRITABLE"));
    }

    #[test]
    fn other_errors_render_their_display_once() {
        let e = AppError::Gateway(GatewayError::Other("boom".to_owned()));
        assert_eq!(render_error(&e, &target()), "boom");
        let e = AppError::NotFound("users/a".parse().unwrap());
        assert_eq!(render_error(&e, &target()), "document not found: users/a");
        let e = AppError::Gateway(GatewayError::ResourceNotFound("db".to_owned()));
        assert_eq!(render_error(&e, &target()), "not found: db");
    }

    #[test]
    fn gateway_missing_index_includes_console_url() {
        let e = AppError::Gateway(GatewayError::MissingIndex {
            console_url: Some("https://x".to_owned()),
        });
        let out = render_error(&e, &target());
        assert_eq!(out.lines().last(), Some("https://x"));
    }
}
