//! The command-line surface, driven through the real binary. None of these tests
//! reach a real service: `FIRESTORE_EMULATOR_HOST` points every run at a dead port, so
//! a test that gets as far as connecting fails there (usage errors and write denial
//! happen before connecting).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

fn fsq() -> Command {
    let mut cmd = Command::cargo_bin("fsq").expect("fsq binary builds");
    // Never reach a real project: a test that gets as far as connecting hits a dead port.
    cmd.env("FIRESTORE_EMULATOR_HOST", "127.0.0.1:1")
        .env_remove("FSQ_PROJECT")
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
        .env("FSQ_WRITABLE", "p/d")
        .args([
            "--project",
            "p",
            "--database",
            "d",
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
        .env("FSQ_WRITABLE", "p/d")
        .args([
            "--project",
            "p",
            "--database",
            "d",
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
        .env("FSQ_WRITABLE", "p/d")
        .args([
            "--project",
            "p",
            "--database",
            "d",
            "set",
            "users/u1",
            "--data",
            "{nope",
        ])
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

#[test]
fn malformed_writable_fails_writes_with_exit_1() {
    fsq()
        .env("FSQ_WRITABLE", "*")
        .args(["--project", "p", "delete", "users/u1"])
        .assert()
        .code(1)
        .stderr(contains("wildcards"));
}

#[test]
fn malformed_writable_does_not_block_reads() {
    // `query` on a document path is rejected while building the request (exit 2),
    // which is only reached if the policy error did not stop the command (exit 1).
    fsq()
        .env("FSQ_WRITABLE", "*")
        .args(["--project", "p", "query", "users/u1"])
        .assert()
        .code(2)
        .stderr(contains("document path"));
}

#[test]
fn denied_write_does_not_read_a_file() {
    fsq()
        .args([
            "--project",
            "p",
            "--database",
            "d",
            "set",
            "users/u1",
            "--data",
            "@/nonexistent/fsq-data.json",
        ])
        .assert()
        .code(6)
        .stderr("writes are disabled for database p/d\n");
}

#[test]
fn denied_write_does_not_read_stdin() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("fsq"))
        .env("FIRESTORE_EMULATOR_HOST", "127.0.0.1:1")
        .env_remove("FSQ_WRITABLE")
        .args([
            "--project",
            "p",
            "--database",
            "d",
            "set",
            "users/u1",
            "--data",
            "-",
        ])
        .stdin(Stdio::piped()) // held open and never written: a read would block
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if Instant::now() > deadline {
            child.kill().unwrap();
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    assert_eq!(status.and_then(|s| s.code()), Some(6), "blocked on stdin?");
}

#[test]
fn empty_project_env_counts_as_unset() {
    fsq()
        .env("FSQ_PROJECT", "")
        .args(["completions", "bash"])
        .assert()
        .success();
    fsq()
        .env("FSQ_PROJECT", "")
        .args(["get", "users/u1"])
        .assert()
        .code(2)
        .stderr(contains("--project"));
}

#[test]
fn empty_database_env_means_default() {
    fsq()
        .env("FSQ_DATABASE", "")
        .args(["--project", "p", "delete", "users/u1"])
        .assert()
        .code(6)
        .stderr("writes are disabled for database p/(default)\n");
}

#[test]
fn invalid_database_env_is_a_usage_error() {
    fsq()
        .env("FSQ_DATABASE", "Bad Name")
        .args(["--project", "p", "get", "users/u1"])
        .assert()
        .code(2)
        .stderr(contains("FSQ_DATABASE"));
}

#[test]
fn allowed_write_passes_authorization() {
    // Past the policy gate the request is built; the bad --data is then an input
    // error (exit 2), not a denial (exit 6).
    fsq()
        .env("FSQ_WRITABLE", "p/d")
        .args([
            "--project",
            "p",
            "--database",
            "d",
            "set",
            "users/u1",
            "--data",
            "{nope",
        ])
        .assert()
        .code(2);
}

#[test]
fn subcommand_help_is_self_sufficient() {
    fsq().args(["query", "--help"]).assert().success().stdout(
        contains("array-contains-any")
            .and(contains("<field> <op> <value>"))
            .and(contains("fsq --help")),
    );
    fsq().args(["update", "--help"]).assert().success().stdout(
        contains("ARRAY-UNION")
            .and(contains("ARRAY-REMOVE"))
            .and(contains("fsq --help")),
    );
    for sub in [
        "get",
        "count",
        "collections",
        "indexes",
        "set",
        "delete",
        "completions",
    ] {
        fsq()
            .args([sub, "--help"])
            .assert()
            .success()
            .stdout(contains("run `fsq --help`"));
    }
}

#[test]
fn global_flags_have_their_own_help_section() {
    fsq()
        .args(["get", "--help"])
        .assert()
        .success()
        .stdout(contains("Global options:"));
}

// Nothing listens on port 1. These reach the connection step (so policy, parsing and
// authorization all passed) and fail there, quickly, without touching any real service.
#[test]
fn authorized_write_to_an_unreachable_endpoint_exits_1_not_6() {
    fsq()
        .env("FSQ_WRITABLE", "p/d")
        .timeout(std::time::Duration::from_secs(10))
        .args(["--project", "p", "--database", "d", "delete", "users/u1"])
        .assert()
        .code(1)
        .stderr(contains(
            "cannot connect to Firestore at http://127.0.0.1:1",
        ));
}

#[test]
fn read_from_an_unreachable_endpoint_exits_1() {
    fsq()
        .timeout(std::time::Duration::from_secs(10))
        .args(["--project", "p", "--database", "d", "get", "users/u1"])
        .assert()
        .code(1)
        .stderr(contains("cannot connect to Firestore"));
}

#[test]
fn overlapping_update_paths_are_a_usage_error() {
    // Usage error (2) while building the request: before any connection.
    fsq()
        .env("FSQ_WRITABLE", "p/d")
        .args([
            "--project",
            "p",
            "--database",
            "d",
            "update",
            "users/u1",
            "--set",
            "a={\"x\":1}",
            "--set",
            "a.b=2",
        ])
        .assert()
        .code(2)
        .stderr(contains("conflicting changes to `a` and `a.b`"));
}
