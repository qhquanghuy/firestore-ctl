//! The command-line surface, driven through the real binary. None of these tests
//! reach a real service: `FIRESTORE_EMULATOR_HOST` points every run at a dead port, so
//! a test that gets as far as connecting fails there (usage errors and write denial
//! happen before connecting).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use assert_cmd::Command;
use predicates::prelude::*;
use predicates::str::contains;

fn firectl() -> Command {
    let mut cmd = Command::cargo_bin("firectl").expect("firectl binary builds");
    // Never reach a real project: a test that gets as far as connecting hits a dead port.
    // Nor the real completion cache: nothing can be created below `/dev/null`.
    cmd.env("FIRESTORE_EMULATOR_HOST", "127.0.0.1:1")
        .env("XDG_CACHE_HOME", "/dev/null/firectl-test-cache")
        .env_remove("COMPLETE")
        .env_remove("FIRECTL_COMPLETE")
        .env_remove("FIRECTL_PROJECT")
        .env_remove("FIRECTL_DATABASE")
        .env_remove("FIRECTL_WRITABLE");
    cmd
}

#[test]
fn help_lists_commands_and_exit_codes() {
    firectl().arg("--help").assert().success().stdout(
        contains("query")
            .and(contains("exit codes"))
            .and(contains("FIRECTL_WRITABLE").not()),
    );
}

#[test]
fn help_documents_value_syntax_and_env() {
    firectl().arg("--help").assert().success().stdout(
        contains("FIRECTL_PROJECT")
            .and(contains("FIRECTL_DATABASE"))
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
        firectl()
            .args([sub, "--help"])
            .assert()
            .success()
            .stdout(contains("FIRECTL_WRITABLE").not());
    }
}

#[test]
fn bad_filter_is_usage_error() {
    firectl()
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
    firectl()
        .args(["--project", "p", "get", "users"])
        .assert()
        .code(2);
}

#[test]
fn missing_project_is_usage_error() {
    firectl()
        .args(["get", "users/u1"])
        .assert()
        .code(2)
        .stderr(contains("--project"));
}

#[test]
fn write_denied_without_network() {
    firectl()
        .env_remove("FIRECTL_WRITABLE")
        .args(["--project", "p", "--database", "d", "delete", "users/u1"])
        .assert()
        .code(6)
        .stderr("writes are disabled for database p/d\n");
}

#[test]
fn write_denied_uses_env_project_and_database() {
    firectl()
        .env("FIRECTL_PROJECT", "envp")
        .env("FIRECTL_DATABASE", "envd")
        .args(["delete", "users/u1"])
        .assert()
        .code(6)
        .stderr("writes are disabled for database envp/envd\n");
}

#[test]
fn overwrite_and_merge_conflict() {
    firectl()
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
    firectl()
        .args(["--project", "p", "--pretty", "--compact", "get", "users/u1"])
        .assert()
        .code(2);
}

#[test]
fn if_unchanged_since_needs_overwrite_or_merge_on_set() {
    firectl()
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
    firectl()
        .args(["--project", "p", "update", "users/u1"])
        .assert()
        .code(2);
}

#[test]
fn set_data_path_mismatch_is_an_input_error() {
    firectl()
        .env("FIRECTL_WRITABLE", "p/d")
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
    firectl()
        .env("FIRECTL_WRITABLE", "p/d")
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
    firectl()
        .env("FIRECTL_WRITABLE", "p/d")
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
    firectl()
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
    firectl().args(["--project", "p", "query"]).assert().code(2);
}

#[test]
fn completions_work_without_a_project() {
    firectl()
        .args(["completions", "bash"])
        .assert()
        .success()
        .stdout(contains("firectl"));
}

#[test]
fn malformed_writable_fails_writes_with_exit_1() {
    firectl()
        .env("FIRECTL_WRITABLE", "*")
        .args(["--project", "p", "delete", "users/u1"])
        .assert()
        .code(1)
        .stderr(contains("wildcards"));
}

#[test]
fn malformed_writable_does_not_block_reads() {
    // `query` on a document path is rejected while building the request (exit 2),
    // which is only reached if the policy error did not stop the command (exit 1).
    firectl()
        .env("FIRECTL_WRITABLE", "*")
        .args(["--project", "p", "query", "users/u1"])
        .assert()
        .code(2)
        .stderr(contains("document path"));
}

#[test]
fn denied_write_does_not_read_a_file() {
    firectl()
        .args([
            "--project",
            "p",
            "--database",
            "d",
            "set",
            "users/u1",
            "--data",
            "@/nonexistent/firectl-data.json",
        ])
        .assert()
        .code(6)
        .stderr("writes are disabled for database p/d\n");
}

#[test]
fn denied_write_does_not_read_stdin() {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("firectl"))
        .env("FIRESTORE_EMULATOR_HOST", "127.0.0.1:1")
        .env_remove("FIRECTL_WRITABLE")
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
    firectl()
        .env("FIRECTL_PROJECT", "")
        .args(["completions", "bash"])
        .assert()
        .success();
    firectl()
        .env("FIRECTL_PROJECT", "")
        .args(["get", "users/u1"])
        .assert()
        .code(2)
        .stderr(contains("--project"));
}

#[test]
fn empty_database_env_means_default() {
    firectl()
        .env("FIRECTL_DATABASE", "")
        .args(["--project", "p", "delete", "users/u1"])
        .assert()
        .code(6)
        .stderr("writes are disabled for database p/(default)\n");
}

#[test]
fn invalid_database_env_is_a_usage_error() {
    firectl()
        .env("FIRECTL_DATABASE", "Bad Name")
        .args(["--project", "p", "get", "users/u1"])
        .assert()
        .code(2)
        .stderr(contains("FIRECTL_DATABASE"));
}

#[test]
fn allowed_write_passes_authorization() {
    // Past the policy gate the bad --data is an input error (exit 2), not a denial (exit 6).
    firectl()
        .env("FIRECTL_WRITABLE", "p/d")
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
    firectl()
        .args(["query", "--help"])
        .assert()
        .success()
        .stdout(
            contains("array-contains-any")
                .and(contains("<field> <op> <value>"))
                .and(contains("firectl --help")),
        );
    firectl()
        .args(["update", "--help"])
        .assert()
        .success()
        .stdout(
            contains("ARRAY-UNION")
                .and(contains("ARRAY-REMOVE"))
                .and(contains("firectl --help")),
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
        firectl()
            .args([sub, "--help"])
            .assert()
            .success()
            .stdout(contains("run `firectl --help`"));
    }
}

#[test]
fn global_flags_have_their_own_help_section() {
    firectl()
        .args(["get", "--help"])
        .assert()
        .success()
        .stdout(contains("Global options:"));
}

// Nothing listens on port 1: these reach the connection step (policy, parsing and
// authorization all passed) and fail there, without touching any real service.
#[test]
fn authorized_write_to_an_unreachable_endpoint_exits_1_not_6() {
    firectl()
        .env("FIRECTL_WRITABLE", "p/d")
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
    firectl()
        .timeout(std::time::Duration::from_secs(10))
        .args(["--project", "p", "--database", "d", "get", "users/u1"])
        .assert()
        .code(1)
        .stderr(contains("cannot connect to Firestore"));
}

#[test]
fn overlapping_update_paths_are_a_usage_error() {
    firectl()
        .env("FIRECTL_WRITABLE", "p/d")
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

/// What the bash script sends: `FIRECTL_COMPLETE=bash`, the words after `--`, the index of the
/// word being completed and the line up to the cursor.
fn completion_line(words: &[&str], line: Option<&str>) -> Command {
    let mut cmd = firectl();
    cmd.env("FIRECTL_COMPLETE", "bash")
        .env("_CLAP_COMPLETE_INDEX", (words.len() - 1).to_string())
        .env("_CLAP_IFS", "\n")
        .env(
            "_FIRECTL_COMP_LINE",
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
        firectl()
            .args(["completions", shell])
            .assert()
            .success()
            .stdout(format!("source <(FIRECTL_COMPLETE={shell} firectl)\n"));
    }
    firectl().args(["completions", "tcsh"]).assert().code(2);
}

#[test]
fn registration_script_calls_back_into_the_binary() {
    let script = |shell: &str| {
        let out = firectl().env("FIRECTL_COMPLETE", shell).assert().success();
        String::from_utf8(out.get_output().stdout.clone()).unwrap()
    };
    let zsh = script("zsh");
    assert!(
        zsh.contains("FIRECTL_COMPLETE") && zsh.contains("firectl"),
        "{zsh}"
    );
    // The bash script also tells the binary where the cursor is, to quote candidates.
    let bash = script("bash");
    assert!(
        bash.contains("FIRECTL_COMPLETE") && bash.contains("_FIRECTL_COMP_LINE"),
        "{bash}"
    );
}

#[test]
fn a_stray_complete_variable_does_not_hijack_the_program() {
    for value in ["bash", "zsh", "fish"] {
        firectl()
            .env("COMPLETE", value)
            .args(["completions", "zsh"])
            .assert()
            .success()
            .stdout("source <(FIRECTL_COMPLETE=zsh firectl)\n");
    }
    firectl()
        .env("COMPLETE", "bash")
        .arg("--help")
        .assert()
        .success()
        .stdout(contains("Usage"));
}

#[test]
fn completing_flags_needs_no_project() {
    completion(&["firectl", "query", "users", "--wh"])
        .assert()
        .success()
        .stdout(contains("--where"))
        .stderr("");
}

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
    let offered = candidates(&["firectl", "query", ""]);
    assert!(offered.iter().all(|c| c.starts_with("--")), "{offered:?}");
}

#[test]
fn completion_failures_are_silent() {
    let offered = candidates(&["firectl", "--project", "p", "query", ""]);
    assert!(offered.iter().all(|c| c.starts_with("--")), "{offered:?}");
    assert_eq!(
        candidates(&["firectl", "--project", "p", "query", "users", "--where", ""]),
        Vec::<String>::new()
    );
    assert_eq!(
        candidates(&[
            "firectl",
            "--project",
            "p",
            "query",
            "users",
            "--select",
            "a,"
        ]),
        Vec::<String>::new()
    );
}

#[test]
fn completing_operators_needs_no_database() {
    let line = "firectl query users --where 'age >";
    assert_eq!(
        candidates_of(completion_line(
            &["firectl", "query", "users", "--where", "age >"],
            Some(line)
        )),
        ["age >", "age >="]
    );
    assert_eq!(
        candidates(&["firectl", "query", "users", "--where", "age >"]),
        ["'age >'", "'age >='"]
    );
}

fn seeded_cache(collections: &[&str], fields: &[(&str, &[&str])]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    // `firectl()` points every run at the emulator 127.0.0.1:1, whose answers are cached apart.
    let db = dir
        .path()
        .join("firectl/p/(default)/emulator-127%2E0%2E0%2E1%3A1");
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

/// A lookup the cache does not answer fails at once against the dead emulator port: no test
/// can reach a real service.
fn completion_cached(cache: &tempfile::TempDir, words: &[&str], line: Option<&str>) -> Command {
    let mut cmd = completion_line(words, line);
    cmd.env("XDG_CACHE_HOME", cache.path());
    cmd
}

#[test]
fn cached_collections_are_offered_by_prefix() {
    let cache = seeded_cache(&["users", "orders", "user_logs"], &[]);
    let offered = |typed: &str| {
        candidates_of(completion_cached(
            &cache,
            &["firectl", "--project", "p", "query", typed],
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
    assert_eq!(
        candidates_of(completion_cached(
            &cache,
            &["firectl", "--project", "p", "indexes", "o"],
            None
        )),
        ["orders"]
    );
    assert_eq!(
        candidates_of(completion_cached(
            &cache,
            &["firectl", "--project", "p", "query", "--group", "o"],
            None
        )),
        ["orders"]
    );
}

#[test]
fn cached_fields_are_offered_per_flag() {
    let fields: &[&str] = &["address", "address.city", "name", "`my-field`"];
    let cache = seeded_cache(&["users"], &[("users", fields)]);
    let base = ["firectl", "--project", "p", "query", "users"];
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
    let in_single = "firectl --project p query users --where 'my";
    assert_eq!(offered("--where", "my", Some(in_single)), ["`my-field`"]);
    let in_double = r#"firectl --project p query users --where "my"#;
    assert_eq!(offered("--where", "my", Some(in_double)), [r"\`my-field\`"]);
    assert_eq!(offered("--select", "na ", None), Vec::<String>::new());
    assert_eq!(offered("--order-by", "name,a", None), Vec::<String>::new());
}

#[test]
fn without_the_command_line_only_plain_candidates_are_offered() {
    // A bash registration that does not pass the cursor's quoting (an older script, or one
    // not made by `firectl completions`): names that need quoting are left out, not guessed.
    let fields: &[&str] = &["name", "`my-field`", "a b"];
    let cache = seeded_cache(&["users"], &[("users", fields)]);
    let words = [
        "firectl",
        "--project",
        "p",
        "query",
        "users",
        "--select",
        "",
    ];
    let mut cmd = completion_cached(&cache, &words, None);
    cmd.env_remove("_FIRECTL_COMP_LINE");
    assert_eq!(candidates_of(cmd), ["name"]);
    assert_eq!(
        candidates_of(completion_cached(&cache, &words, None)),
        ["name", "'`my-field`'", "'a b'"]
    );
}
