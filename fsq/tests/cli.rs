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
    // Nor the real completion cache: nothing can be created below `/dev/null`.
    cmd.env("FIRESTORE_EMULATOR_HOST", "127.0.0.1:1")
        .env("XDG_CACHE_HOME", "/dev/null/fsq-test-cache")
        .env_remove("COMPLETE")
        .env_remove("FSQ_COMPLETE")
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

/// What the bash script sends to complete the last of `words`: `fsq` called with
/// `FSQ_COMPLETE=bash`, the words after `--`, the index of the word being completed and
/// the command line up to the cursor (`line`, by default the words joined by spaces).
fn completion_line(words: &[&str], line: Option<&str>) -> Command {
    let mut cmd = fsq();
    cmd.env("FSQ_COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", (words.len() - 1).to_string())
        .env("_CLAP_IFS", "\n")
        .env(
            "_FSQ_COMP_LINE",
            line.map_or_else(|| words.join(" "), str::to_owned),
        )
        .arg("--")
        .args(words);
    cmd
}

fn completion(words: &[&str]) -> Command {
    completion_line(words, None)
}

#[test]
fn completions_prints_the_registration_line() {
    for shell in ["bash", "zsh"] {
        fsq()
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(format!("source <(FSQ_COMPLETE={shell} fsq)\n"));
    }
    fsq().args(["completions", "tcsh"]).assert().code(2);
}

#[test]
fn registration_script_calls_back_into_the_binary() {
    let script = |shell: &str| {
        let out = fsq().env("FSQ_COMPLETE", shell).assert().success();
        String::from_utf8(out.get_output().stdout.clone()).unwrap()
    };
    let zsh = script("zsh");
    assert!(zsh.contains("FSQ_COMPLETE") && zsh.contains("fsq"), "{zsh}");
    // The bash script also tells the binary where the cursor is, to quote candidates.
    let bash = script("bash");
    assert!(
        bash.contains("FSQ_COMPLETE") && bash.contains("_FSQ_COMP_LINE"),
        "{bash}"
    );
}

#[test]
fn a_stray_complete_variable_does_not_hijack_the_program() {
    for value in ["bash", "zsh", "fish"] {
        fsq()
            .env("COMPLETE", value)
            .args(["completions", "zsh"])
            .assert()
            .success()
            .stdout("source <(FSQ_COMPLETE=zsh fsq)\n");
    }
    fsq()
        .env("COMPLETE", "bash")
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("Usage"));
}

#[test]
fn completing_flags_needs_no_project() {
    completion(&["fsq", "query", "users", "--wh"])
        .assert()
        .success()
        .stdout(contains("--where"))
        .stderr("");
}

/// The completion candidates `cmd` prints, which must come with an empty stderr.
fn candidates_of(mut cmd: Command) -> Vec<String> {
    let assert = cmd
        .timeout(std::time::Duration::from_secs(10))
        .assert()
        .success()
        .stderr("");
    String::from_utf8(assert.get_output().stdout.clone())
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect()
}

fn candidates(words: &[&str]) -> Vec<String> {
    candidates_of(completion(words))
}

#[test]
fn collections_are_not_offered_without_a_project() {
    // Only the flags are left: nothing was looked up, nothing was said.
    let offered = candidates(&["fsq", "query", ""]);
    assert!(offered.iter().all(|c| c.starts_with("--")), "{offered:?}");
}

#[test]
fn completion_failures_are_silent() {
    // A project is known, but the database is unreachable (dead emulator port).
    let offered = candidates(&["fsq", "--project", "p", "query", ""]);
    assert!(offered.iter().all(|c| c.starts_with("--")), "{offered:?}");
    assert_eq!(
        candidates(&["fsq", "--project", "p", "query", "users", "--where", ""]),
        Vec::<String>::new()
    );
    assert_eq!(
        candidates(&["fsq", "--project", "p", "query", "users", "--select", "a,"]),
        Vec::<String>::new()
    );
}

#[test]
fn completing_operators_needs_no_database() {
    // Inside the quote bash was given, the candidate is inserted as it is.
    let line = "fsq query users --where 'age >";
    assert_eq!(
        candidates_of(completion_line(
            &["fsq", "query", "users", "--where", "age >"],
            Some(line)
        )),
        ["age >", "age >="]
    );
    // Outside any quote it has to be quoted to stay one word.
    assert_eq!(
        candidates(&["fsq", "query", "users", "--where", "age >"]),
        ["'age >'", "'age >='"]
    );
}

/// A cache directory holding the answers `fsq --project p` would have looked up: the
/// `collections` of the database and, per collection, the `fields` sampled from it.
fn seeded_cache(collections: &[&str], fields: &[(&str, &[&str])]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("fsq/p/(default)");
    std::fs::create_dir_all(&db).unwrap();
    let written = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let entry =
        |items: &[&str]| serde_json::json!({ "written": written, "items": items }).to_string();
    std::fs::write(db.join("collections.json"), entry(collections)).unwrap();
    for (collection, names) in fields {
        std::fs::write(db.join(format!("fields-c-{collection}.json")), entry(names)).unwrap();
    }
    dir
}

/// `completion_line` against the database `cache` describes: no lookup is made (the
/// emulator variable is dropped, so a lookup would reach the real service).
fn completion_cached(cache: &tempfile::TempDir, words: &[&str], line: Option<&str>) -> Command {
    let mut cmd = completion_line(words, line);
    cmd.env("XDG_CACHE_HOME", cache.path())
        .env_remove("FIRESTORE_EMULATOR_HOST")
        .env("GOOGLE_APPLICATION_CREDENTIALS", "/nonexistent");
    cmd
}

#[test]
fn cached_collections_are_offered_by_prefix() {
    let cache = seeded_cache(&["users", "orders", "user_logs"], &[]);
    let offered = |typed: &str| {
        candidates_of(completion_cached(
            &cache,
            &["fsq", "--project", "p", "query", typed],
            None,
        ))
    };
    assert_eq!(offered("us"), ["users", "user_logs"]);
    assert_eq!(offered("or"), ["orders"]);
    assert_eq!(offered("x"), Vec::<String>::new());
    let all = offered("");
    assert!(
        ["users", "orders", "user_logs"]
            .iter()
            .all(|c| all.contains(&(*c).to_owned())),
        "{all:?}"
    );
    // `indexes` and `--group` take collection ids too.
    assert_eq!(
        candidates_of(completion_cached(
            &cache,
            &["fsq", "--project", "p", "indexes", "o"],
            None
        )),
        ["orders"]
    );
    assert_eq!(
        candidates_of(completion_cached(
            &cache,
            &["fsq", "--project", "p", "query", "--group", "o"],
            None
        )),
        ["orders"]
    );
}

#[test]
fn cached_fields_are_offered_per_flag() {
    let fields: &[&str] = &["address", "address.city", "name", "`my-field`"];
    let cache = seeded_cache(&["users"], &[("users", fields)]);
    let base = ["fsq", "--project", "p", "query", "users"];
    let offered = |flag: &str, typed: &str, line: Option<&str>| {
        let words = [&base[..], &[flag, typed]].concat();
        candidates_of(completion_cached(&cache, &words, line))
    };
    assert_eq!(
        offered("--select", "name,ad", None),
        ["name,address", "name,address.city"]
    );
    assert_eq!(offered("--select", "na", None), ["name"]);
    assert_eq!(
        offered("--order-by", "addr", None),
        ["address", "address.city"]
    );
    assert_eq!(offered("--where", "na", None), ["name"]);
    // A name that bash would take for a command substitution is quoted for it ...
    assert_eq!(offered("--select", "my", None), ["'`my-field`'"]);
    // ... unless the cursor is already inside a quote (which then stays as it is).
    let in_single = "fsq --project p query users --where 'my";
    assert_eq!(offered("--where", "my", Some(in_single)), ["`my-field`"]);
    let in_double = r#"fsq --project p query users --where "my"#;
    assert_eq!(offered("--where", "my", Some(in_double)), [r"\`my-field\`"]);
    // Other flags' syntax is not completed.
    assert_eq!(offered("--select", "na ", None), Vec::<String>::new());
    assert_eq!(offered("--order-by", "name,a", None), Vec::<String>::new());
}
