//! The command-line surface, driven through the real binary. None of these tests
//! reach the network: usage errors and write denial both happen before connecting.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

fn fsq() -> Command {
    let mut cmd = Command::cargo_bin("fsq").expect("fsq binary builds");
    cmd.env_remove("FSQ_PROJECT")
        .env_remove("FSQ_DATABASE")
        .env_remove("FSQ_WRITABLE");
    cmd
}

#[test]
fn help_lists_commands_and_exit_codes() {
    fsq().arg("--help").assert().success().stdout(
        contains("query")
            .and(contains("exit codes"))
            .and(contains("FSQ_WRITABLE").not()),
    );
}

#[test]
fn help_documents_value_syntax_and_env() {
    fsq().arg("--help").assert().success().stdout(
        contains("FSQ_PROJECT")
            .and(contains("FSQ_DATABASE"))
            .and(contains("ts:"))
            .and(contains("ref:"))
            .and(contains("leading zeros"))
            .and(contains("$ts"))
            .and(contains("$ref"))
            .and(contains("$double"))
            .and(contains("$bytes"))
            .and(contains("$geo"))
            .and(contains("$map"))
            .and(contains("ARRAY-UNION"))
            .and(contains("ARRAY-REMOVE"))
            .and(contains("not numeric")),
    );
}

#[test]
fn no_subcommand_help_mentions_writable() {
    for sub in [
        "get",
        "query",
        "count",
        "collections",
        "indexes",
        "set",
        "update",
        "delete",
    ] {
        fsq()
            .args([sub, "--help"])
            .assert()
            .success()
            .stdout(contains("FSQ_WRITABLE").not());
    }
}

#[test]
fn bad_filter_is_usage_error() {
    fsq()
        .args([
            "--project",
            "p",
            "query",
            "users",
            "--where",
            "status active",
        ])
        .assert()
        .code(2);
}

#[test]
fn get_requires_doc_path() {
    fsq()
        .args(["--project", "p", "get", "users"])
        .assert()
        .code(2);
}

#[test]
fn missing_project_is_usage_error() {
    fsq()
        .args(["get", "users/u1"])
        .assert()
        .code(2)
        .stderr(contains("--project"));
}

#[test]
fn write_denied_without_network() {
    fsq()
        .env_remove("FSQ_WRITABLE")
        .args(["--project", "p", "--database", "d", "delete", "users/u1"])
        .assert()
        .code(6)
        .stderr("writes are disabled for database p/d\n");
}

#[test]
fn write_denied_uses_env_project_and_database() {
    fsq()
        .env("FSQ_PROJECT", "envp")
        .env("FSQ_DATABASE", "envd")
        .args(["delete", "users/u1"])
        .assert()
        .code(6)
        .stderr("writes are disabled for database envp/envd\n");
}

#[test]
fn bad_writable_env_is_reported_and_exits_1() {
    fsq()
        .env("FSQ_WRITABLE", "*")
        .args(["--project", "p", "get", "users/u1"])
        .assert()
        .code(1)
        .stderr(contains("wildcards"));
}

// TEMPORARY: pinned to the connection stub ("not implemented"); Task 9 must replace this
// with a test that does not reach the network.
#[test]
fn allowed_write_passes_authorization_then_fails_at_the_stub() {
    fsq()
        .env("FSQ_WRITABLE", "p/d")
        .args(["--project", "p", "--database", "d", "delete", "users/u1"])
        .assert()
        .code(1)
        .stderr(contains("not implemented"));
}

#[test]
fn overwrite_and_merge_conflict() {
    fsq()
        .args([
            "--project",
            "p",
            "set",
            "users/u1",
            "--data",
            "{}",
            "--overwrite",
            "--merge",
        ])
        .assert()
        .code(2);
}

#[test]
fn pretty_and_compact_conflict() {
    fsq()
        .args(["--project", "p", "--pretty", "--compact", "get", "users/u1"])
        .assert()
        .code(2);
}

#[test]
fn if_unchanged_since_needs_overwrite_or_merge_on_set() {
    fsq()
        .args([
            "--project",
            "p",
            "set",
            "users/u1",
            "--data",
            "{}",
            "--if-unchanged-since",
            "2026-01-01T00:00:00Z",
        ])
        .assert()
        .code(2);
}

#[test]
fn update_requires_a_change() {
    fsq()
        .args(["--project", "p", "update", "users/u1"])
        .assert()
        .code(2);
}

#[test]
fn set_data_path_mismatch_is_an_input_error() {
    fsq()
        .args([
            "--project",
            "p",
            "set",
            "users/u1",
            "--overwrite",
            "--data",
            r#"{"_path":"users/u2","data":{"a":1}}"#,
        ])
        .assert()
        .code(2)
        .stderr("_path in --data (users/u2) does not match target users/u1\n");
}

#[test]
fn set_data_decode_error_names_the_pointer() {
    fsq()
        .args([
            "--project",
            "p",
            "set",
            "users/u1",
            "--data",
            r#"{"a":{"$bogus":1}}"#,
        ])
        .assert()
        .code(2)
        .stderr(contains("/a"));
}

#[test]
fn set_data_invalid_json_is_an_input_error() {
    fsq()
        .args(["--project", "p", "set", "users/u1", "--data", "{nope"])
        .assert()
        .code(2)
        .stderr(contains("--data"));
}

#[test]
fn count_rejects_duplicate_sum_fields() {
    fsq()
        .args([
            "--project",
            "p",
            "count",
            "orders",
            "--sum",
            "total",
            "--sum",
            "total",
        ])
        .assert()
        .code(2)
        .stderr(contains("total"));
}

#[test]
fn query_needs_a_collection_unless_grouped() {
    fsq().args(["--project", "p", "query"]).assert().code(2);
}

#[test]
fn completions_work_without_a_project() {
    fsq()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(contains("fsq"));
}

// TEMPORARY: pinned to the connection stub ("not implemented"); Task 9 must replace this
// with a test that does not reach the network.
#[test]
fn read_hits_the_stub_after_authorization() {
    fsq()
        .args(["--project", "p", "get", "users/u1"])
        .assert()
        .code(1)
        .stderr("not implemented\n");
}
