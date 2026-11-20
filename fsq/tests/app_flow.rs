//! The request flow end to end against the in-memory fake.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod support;

use fsq::app::{AppError, Context, Output, Request, preflight, run};
use fsq::domain::{
    AggregateQuery, Aggregation, CollectionPath, DbTarget, DeleteDoc, Direction, DocPath, Document,
    FieldOp, Fields, Filter, Guard, IndexAdvice, NonEmptyVec, Op, OrderBy, Query, Scope, SetDoc,
    SetMode, UpdateDoc, UpdateTime, Value, Write,
};
use std::process::ExitCode;

use fsq::exit::{exit_code, exit_code_value};
use fsq::policy::{WriteDenied, WritePolicy};
use fsq::port::GatewayError;
use support::fake::{FakeFirestore, default_target};

fn path(s: &str) -> DocPath {
    s.parse().unwrap()
}

fn time(s: &str) -> UpdateTime {
    s.parse().unwrap()
}

fn doc(p: &str) -> Document {
    Document {
        path: path(p),
        update_time: time("2026-01-01T00:00:00Z"),
        data: Fields::from([("name".to_owned(), Value::from("Ann"))]),
    }
}

fn ctx(policy: Option<&str>, target: DbTarget) -> Context {
    Context {
        target,
        policy: WritePolicy::from_env_value(policy).unwrap(),
    }
}

fn ctx_read_only() -> Context {
    ctx(None, default_target())
}

fn ctx_writable() -> Context {
    ctx(Some("d"), default_target())
}

fn delete(p: &str) -> Write {
    delete_guarded(p, Guard::None)
}

fn delete_guarded(p: &str, guard: Guard) -> Write {
    Write::Delete(DeleteDoc {
        path: path(p),
        guard,
    })
}

fn set(p: &str, mode: SetMode) -> Write {
    Write::Set(SetDoc {
        path: path(p),
        data: Fields::from([("name".to_owned(), Value::from("Bob"))]),
        mode,
    })
}

fn denied() -> WriteDenied {
    WritePolicy::from_env_value(None)
        .unwrap()
        .authorize(&default_target())
        .unwrap_err()
}

fn users_query(filters: Vec<Filter>, order: Vec<OrderBy>) -> Query {
    Query {
        scope: Scope::Collection(CollectionPath::new(None, "users".parse().unwrap())),
        filters,
        order,
        limit: None,
        offset: 0,
        select: None,
    }
}

fn eq(field: &str, v: &str) -> Filter {
    Filter::new(field.parse().unwrap(), Op::Eq, Value::from(v)).unwrap()
}

fn order(field: &str) -> OrderBy {
    OrderBy {
        field: field.parse().unwrap(),
        direction: Direction::Asc,
    }
}

#[tokio::test]
async fn write_denied_never_reaches_gateway() {
    let fs = FakeFirestore::default();
    let r = run(&ctx_read_only(), &fs, Request::Write(delete("users/u1"))).await;
    assert!(matches!(r, Err(AppError::WritesDisabled(_))));
    assert_eq!(fs.calls(), Vec::<&str>::new());
}

#[tokio::test]
async fn preflight_denies_writes_and_ignores_reads() {
    let write = Request::Write(delete("users/u1"));
    assert!(matches!(
        preflight(&ctx_read_only(), &write),
        Err(AppError::WritesDisabled(_))
    ));
    assert!(preflight(&ctx_writable(), &write).unwrap().is_some());
    let read = Request::Get(path("users/u1"));
    assert!(preflight(&ctx_read_only(), &read).unwrap().is_none());
}

#[tokio::test]
async fn set_create_fails_when_exists() {
    let fs = FakeFirestore::with(doc("users/u1"));
    let r = run(
        &ctx_writable(),
        &fs,
        Request::Write(set("users/u1", SetMode::Create)),
    )
    .await;
    assert!(matches!(
        r,
        Err(AppError::Gateway(GatewayError::AlreadyExists(_)))
    ));
}

#[tokio::test]
async fn set_create_writes_new_document() {
    let fs = FakeFirestore::default();
    let r = run(
        &ctx_writable(),
        &fs,
        Request::Write(set("users/u1", SetMode::Create)),
    )
    .await
    .unwrap();
    let Output::Written {
        path: written,
        update_time,
    } = r
    else {
        panic!("expected Written, got {r:?}");
    };
    assert_eq!(written, path("users/u1"));
    assert_eq!(fs.stored(&written).unwrap().update_time, update_time);
}

#[tokio::test]
async fn update_applies_field_ops() {
    let fs = FakeFirestore::with(doc("users/u1"));
    let ops =
        NonEmptyVec::try_from(vec![FieldOp::Set("age".parse().unwrap(), Value::Int(7))]).unwrap();
    run(
        &ctx_writable(),
        &fs,
        Request::Write(Write::Update(UpdateDoc {
            path: path("users/u1"),
            ops,
            guard: Guard::None,
        })),
    )
    .await
    .unwrap();
    let data = fs.stored(&path("users/u1")).unwrap().data;
    assert_eq!(data.get("age"), Some(&Value::Int(7)));
    assert_eq!(data.get("name"), Some(&Value::from("Ann")));
}

#[tokio::test]
async fn delete_missing_is_not_found() {
    let fs = FakeFirestore::default();
    let r = run(&ctx_writable(), &fs, Request::Write(delete("users/u1"))).await;
    assert!(matches!(
        r,
        Err(AppError::Gateway(GatewayError::NotFound(_)))
    ));
}

#[tokio::test]
async fn guarded_write_fails_after_change() {
    let fs = FakeFirestore::with(doc("users/u1"));
    let old = time("2025-12-31T00:00:00Z");
    let r = run(
        &ctx_writable(),
        &fs,
        Request::Write(delete_guarded("users/u1", Guard::UnchangedSince(old))),
    )
    .await;
    assert!(matches!(
        r,
        Err(AppError::Gateway(GatewayError::PreconditionFailed(_)))
    ));
    assert!(fs.stored(&path("users/u1")).is_some());
}

#[tokio::test]
async fn guarded_write_succeeds_when_unchanged() {
    let fs = FakeFirestore::with(doc("users/u1"));
    let current = time("2026-01-01T00:00:00Z");
    let r = run(
        &ctx_writable(),
        &fs,
        Request::Write(delete_guarded("users/u1", Guard::UnchangedSince(current))),
    )
    .await;
    assert!(r.is_ok());
    assert!(fs.stored(&path("users/u1")).is_none());
}

#[tokio::test]
async fn grant_for_one_database_cannot_write_another() {
    let other = DbTarget {
        project: "p".parse().unwrap(),
        database: "other".parse().unwrap(),
    };
    let fs = FakeFirestore::for_target(other);
    let r = run(
        &ctx_writable(),
        &fs,
        Request::Write(set("users/u1", SetMode::Create)),
    )
    .await;
    assert!(matches!(r, Err(AppError::Gateway(GatewayError::Other(_)))));
    assert!(fs.stored(&path("users/u1")).is_none());
}

#[tokio::test]
async fn get_returns_document() {
    let fs = FakeFirestore::with(doc("users/u1"));
    let r = run(&ctx_read_only(), &fs, Request::Get(path("users/u1"))).await;
    assert_eq!(r.unwrap(), Output::Document(doc("users/u1")));
}

#[tokio::test]
async fn get_missing_is_not_found() {
    let fs = FakeFirestore::default();
    let r = run(&ctx_read_only(), &fs, Request::Get(path("users/u1"))).await;
    assert_eq!(r.unwrap_err(), AppError::NotFound(path("users/u1")));
}

#[tokio::test]
async fn query_filters_by_equality() {
    let fs = FakeFirestore::with(doc("users/u1"));
    let hit = run(
        &ctx_read_only(),
        &fs,
        Request::Query(users_query(vec![eq("name", "Ann")], vec![])),
    )
    .await;
    let miss = run(
        &ctx_read_only(),
        &fs,
        Request::Query(users_query(vec![eq("name", "Zed")], vec![])),
    )
    .await;
    assert_eq!(hit.unwrap(), Output::Documents(vec![doc("users/u1")]));
    assert_eq!(miss.unwrap(), Output::Documents(vec![]));
}

#[tokio::test]
async fn missing_index_carries_advice() {
    let fs = FakeFirestore::default().without_index();
    let q = users_query(vec![eq("name", "Ann")], vec![order("age")]);
    let r = run(&ctx_read_only(), &fs, Request::Query(q)).await;
    let Err(AppError::MissingIndex {
        advice,
        console_url,
    }) = r
    else {
        panic!("expected MissingIndex, got {r:?}");
    };
    assert!(matches!(advice, Some(IndexAdvice::Composite(_))));
    assert!(console_url.is_some());
}

#[tokio::test]
async fn aggregate_missing_index_carries_advice() {
    let fs = FakeFirestore::default().without_index();
    let q = AggregateQuery {
        query: users_query(vec![eq("name", "Ann")], vec![order("age")]),
        aggregations: NonEmptyVec::try_from(vec![Aggregation::Count]).unwrap(),
    };
    let r = run(&ctx_read_only(), &fs, Request::Aggregate(q)).await;
    assert!(matches!(
        r,
        Err(AppError::MissingIndex {
            advice: Some(_),
            ..
        })
    ));
}

#[tokio::test]
async fn collections_lists_root_collections() {
    let fs = FakeFirestore::with(doc("users/u1"));
    let r = run(&ctx_read_only(), &fs, Request::Collections(None)).await;
    assert_eq!(
        r.unwrap(),
        Output::Collections(vec!["users".parse().unwrap()])
    );
}

#[test]
fn exit_codes() {
    let p = || path("users/u1");
    let gw = |e: GatewayError| exit_code_value(&AppError::Gateway(e));
    assert_eq!(exit_code_value(&AppError::WritesDisabled(denied())), 6);
    assert_eq!(gw(GatewayError::Auth("x".into())), 8);
    assert_eq!(gw(GatewayError::NotFound(p())), 3);
    assert_eq!(exit_code_value(&AppError::NotFound(p())), 3);
    assert_eq!(gw(GatewayError::AlreadyExists(p())), 4);
    assert_eq!(gw(GatewayError::PreconditionFailed(p())), 5);
    assert_eq!(gw(GatewayError::MissingIndex { console_url: None }), 7);
    assert_eq!(
        exit_code_value(&AppError::MissingIndex {
            advice: None,
            console_url: None
        }),
        7
    );
    assert_eq!(gw(GatewayError::Other("x".into())), 1);
    assert_eq!(exit_code_value(&AppError::Input("x".into())), 2);
}

#[test]
fn exit_code_wraps_the_value_as_process_exit_code() {
    let e = AppError::WritesDisabled(denied());
    assert_eq!(exit_code(&e), ExitCode::from(6));
}
