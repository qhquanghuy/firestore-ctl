//! The request flow end to end against the in-memory fake.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::process::ExitCode;

use fsq::app::{AppError, Context, Output, Request, authorize, run};
use fsq::domain::{
    AggregateQuery, Aggregation, CollectionPath, DbTarget, DeleteDoc, Direction, DocPath, Document,
    FieldOp, Fields, Filter, Guard, IndexAdvice, NonEmptyVec, Op, OrderBy, Query, Scope, SetDoc,
    SetMode, UpdateDoc, UpdateTime, Value, Write,
};
use fsq::exit::{exit_code, exit_code_value};
use fsq::policy::{WriteDenied, WritePolicy};
use fsq::port::{GatewayError, Written};
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

/// Authorizes `req` under `ctx`, then runs it: the flow `main` follows.
async fn exec(ctx: &Context, fs: &FakeFirestore, req: Request) -> Result<Output, AppError> {
    run(fs, authorize(ctx, req)?).await
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

fn update(p: &str, guard: Guard) -> Write {
    Write::Update(UpdateDoc {
        path: path(p),
        ops: NonEmptyVec::try_from(vec![FieldOp::Set("age".parse().unwrap(), Value::Int(7))])
            .unwrap(),
        guard,
    })
}

fn stale() -> Guard {
    Guard::UnchangedSince(time("2025-12-31T00:00:00Z"))
}

fn current() -> Guard {
    Guard::UnchangedSince(time("2026-01-01T00:00:00Z"))
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

fn gateway(e: GatewayError) -> Result<Output, AppError> {
    Err(AppError::Gateway(e))
}

#[tokio::test]
async fn write_denied_never_reaches_gateway() {
    let fs = FakeFirestore::default();
    let r = authorize(&ctx_read_only(), Request::Write(delete("users/u1")));
    assert!(matches!(r, Err(AppError::WritesDisabled(_))));
    assert_eq!(fs.calls(), Vec::<&str>::new());
}

#[tokio::test]
async fn authorized_write_reaches_gateway_with_its_grant() {
    let fs = FakeFirestore::with(doc("users/u1"));
    let r = exec(&ctx_writable(), &fs, Request::Write(delete("users/u1"))).await;
    assert_eq!(
        r,
        Ok(Output::Written {
            path: path("users/u1"),
            result: Written::Deleted
        })
    );
    assert_eq!(fs.calls(), ["write"]);
}

#[tokio::test]
async fn run_uses_the_grant_it_was_given() {
    // The grant is for database p/d; the fake is connected to p/other and refuses it.
    let other = DbTarget {
        project: "p".parse().unwrap(),
        database: "other".parse().unwrap(),
    };
    let fs = FakeFirestore::for_target(other);
    let r = exec(
        &ctx_writable(),
        &fs,
        Request::Write(set("users/u1", SetMode::Create)),
    )
    .await;
    assert!(matches!(r, Err(AppError::Gateway(GatewayError::Other(_)))));
    assert!(fs.stored(&path("users/u1")).is_none());
}

#[tokio::test]
async fn reads_need_no_write_access() {
    let fs = FakeFirestore::with(doc("users/u1"));
    let r = exec(&ctx_read_only(), &fs, Request::Get(path("users/u1"))).await;
    assert_eq!(r, Ok(Output::Document(doc("users/u1"))));
}

#[tokio::test]
async fn set_create_fails_when_exists() {
    let fs = FakeFirestore::with(doc("users/u1"));
    let r = exec(
        &ctx_writable(),
        &fs,
        Request::Write(set("users/u1", SetMode::Create)),
    )
    .await;
    assert_eq!(r, gateway(GatewayError::AlreadyExists(path("users/u1"))));
}

#[tokio::test]
async fn set_create_writes_new_document() {
    let fs = FakeFirestore::default();
    let r = exec(
        &ctx_writable(),
        &fs,
        Request::Write(set("users/u1", SetMode::Create)),
    )
    .await
    .unwrap();
    let stored = fs.stored(&path("users/u1")).unwrap();
    assert_eq!(
        r,
        Output::Written {
            path: path("users/u1"),
            result: Written::Updated(stored.update_time)
        }
    );
}

#[tokio::test]
async fn unguarded_overwrite_and_merge_upsert() {
    for mode in [SetMode::Overwrite(Guard::None), SetMode::Merge(Guard::None)] {
        let fs = FakeFirestore::default();
        let r = exec(&ctx_writable(), &fs, Request::Write(set("users/u1", mode))).await;
        assert!(r.is_ok(), "{mode:?}");
        assert!(fs.stored(&path("users/u1")).is_some(), "{mode:?}");
    }
}

#[tokio::test]
async fn guarded_overwrite_and_merge_check_update_time() {
    for mode in [
        SetMode::Overwrite as fn(Guard) -> SetMode,
        SetMode::Merge as fn(Guard) -> SetMode,
    ] {
        let fs = FakeFirestore::with(doc("users/u1"));
        let changed = exec(
            &ctx_writable(),
            &fs,
            Request::Write(set("users/u1", mode(stale()))),
        )
        .await;
        assert_eq!(
            changed,
            gateway(GatewayError::PreconditionFailed(path("users/u1")))
        );
        let fresh = exec(
            &ctx_writable(),
            &fs,
            Request::Write(set("users/u1", mode(current()))),
        )
        .await;
        assert!(fresh.is_ok());
        // The guard fails on a missing document too (no upsert under a guard).
        let missing = exec(
            &ctx_writable(),
            &FakeFirestore::default(),
            Request::Write(set("users/u1", mode(current()))),
        )
        .await;
        assert_eq!(
            missing,
            gateway(GatewayError::PreconditionFailed(path("users/u1")))
        );
    }
}

#[tokio::test]
async fn update_applies_field_ops() {
    let fs = FakeFirestore::with(doc("users/u1"));
    exec(
        &ctx_writable(),
        &fs,
        Request::Write(update("users/u1", Guard::None)),
    )
    .await
    .unwrap();
    let data = fs.stored(&path("users/u1")).unwrap().data;
    assert_eq!(data.get("age"), Some(&Value::Int(7)));
    assert_eq!(data.get("name"), Some(&Value::from("Ann")));
}

#[tokio::test]
async fn update_missing_is_not_found() {
    let fs = FakeFirestore::default();
    let r = exec(
        &ctx_writable(),
        &fs,
        Request::Write(update("users/u1", Guard::None)),
    )
    .await;
    assert_eq!(r, gateway(GatewayError::NotFound(path("users/u1"))));
}

#[tokio::test]
async fn delete_missing_is_not_found() {
    let fs = FakeFirestore::default();
    let r = exec(&ctx_writable(), &fs, Request::Write(delete("users/u1"))).await;
    assert_eq!(r, gateway(GatewayError::NotFound(path("users/u1"))));
}

#[tokio::test]
async fn guarded_delete_on_missing_document_fails_precondition() {
    let fs = FakeFirestore::default();
    let r = exec(
        &ctx_writable(),
        &fs,
        Request::Write(delete_guarded("users/u1", current())),
    )
    .await;
    assert_eq!(
        r,
        gateway(GatewayError::PreconditionFailed(path("users/u1")))
    );
}

#[tokio::test]
async fn guarded_write_fails_after_change() {
    let fs = FakeFirestore::with(doc("users/u1"));
    let r = exec(
        &ctx_writable(),
        &fs,
        Request::Write(delete_guarded("users/u1", stale())),
    )
    .await;
    assert_eq!(
        r,
        gateway(GatewayError::PreconditionFailed(path("users/u1")))
    );
    assert!(fs.stored(&path("users/u1")).is_some());
}

#[tokio::test]
async fn guarded_write_succeeds_when_unchanged() {
    let fs = FakeFirestore::with(doc("users/u1"));
    let r = exec(
        &ctx_writable(),
        &fs,
        Request::Write(delete_guarded("users/u1", current())),
    )
    .await;
    assert!(r.is_ok());
    assert!(fs.stored(&path("users/u1")).is_none());
}

#[tokio::test]
async fn get_returns_document() {
    let fs = FakeFirestore::with(doc("users/u1"));
    let r = exec(&ctx_read_only(), &fs, Request::Get(path("users/u1"))).await;
    assert_eq!(r, Ok(Output::Document(doc("users/u1"))));
}

#[tokio::test]
async fn get_missing_is_not_found() {
    let fs = FakeFirestore::default();
    let r = exec(&ctx_read_only(), &fs, Request::Get(path("users/u1"))).await;
    assert_eq!(r, Err(AppError::NotFound(path("users/u1"))));
}

#[tokio::test]
async fn query_filters_by_equality() {
    let fs = FakeFirestore::with(doc("users/u1"));
    let query = |name| Request::Query(users_query(vec![eq("name", name)], vec![]));
    let hit = exec(&ctx_read_only(), &fs, query("Ann")).await;
    let miss = exec(&ctx_read_only(), &fs, query("Zed")).await;
    assert_eq!(hit, Ok(Output::Documents(vec![doc("users/u1")])));
    assert_eq!(miss, Ok(Output::Documents(vec![])));
}

#[tokio::test]
async fn fake_refuses_what_it_does_not_model() {
    let fs = FakeFirestore::with(doc("users/u1"));
    let r = exec(
        &ctx_read_only(),
        &fs,
        Request::Query(users_query(vec![], vec![order("name")])),
    )
    .await;
    assert!(matches!(r, Err(AppError::Gateway(GatewayError::Other(_)))));
}

#[tokio::test]
async fn missing_index_carries_advice() {
    let fs = FakeFirestore::default().without_index();
    let q = users_query(vec![eq("name", "Ann")], vec![order("age")]);
    let r = exec(&ctx_read_only(), &fs, Request::Query(q)).await;
    assert!(
        matches!(
            &r,
            Err(AppError::MissingIndex {
                advice: Some(IndexAdvice::Composite(_)),
                console_url: Some(_),
            })
        ),
        "{r:?}"
    );
}

#[tokio::test]
async fn aggregate_missing_index_carries_advice() {
    let fs = FakeFirestore::default().without_index();
    let q = AggregateQuery {
        query: users_query(vec![eq("name", "Ann")], vec![order("age")]),
        aggregations: NonEmptyVec::try_from(vec![Aggregation::Count]).unwrap(),
    };
    let r = exec(&ctx_read_only(), &fs, Request::Aggregate(q)).await;
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
    let r = exec(&ctx_read_only(), &fs, Request::Collections(None)).await;
    assert_eq!(r, Ok(Output::Collections(vec!["users".parse().unwrap()])));
}

#[tokio::test]
async fn indexes_request_reaches_list_indexes() {
    let fs = FakeFirestore::default();
    let r = exec(
        &ctx_read_only(),
        &fs,
        Request::Indexes("users".parse().unwrap()),
    )
    .await;
    assert_eq!(r, Ok(Output::Indexes(vec![])));
    assert_eq!(fs.calls(), ["list_indexes"]);
}

#[test]
fn exit_codes() {
    let p = || path("users/u1");
    let gw = |e: GatewayError| exit_code_value(&AppError::Gateway(e));
    assert_eq!(exit_code_value(&AppError::WritesDisabled(denied())), 6);
    assert_eq!(gw(GatewayError::Auth("x".into())), 8);
    assert_eq!(gw(GatewayError::NotFound(p())), 3);
    assert_eq!(gw(GatewayError::ResourceNotFound("db".into())), 3);
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
