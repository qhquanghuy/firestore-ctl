//! End-to-end tests against the Firestore emulator, driven through the real binary.
//!
//! All tests are ignored by default. Run them with
//! `firebase emulators:exec --only firestore --project demo-fsq 'cargo nextest run --test emulator --run-ignored all'`.
//! Each test uses its own `demo-` project, so tests do not see each other's documents,
//! and refuses to run unless `FIRESTORE_EMULATOR_HOST` names a local address: these
//! tests write, and must never reach a real project.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use assert_cmd::Command;
use pretty_assertions::assert_eq;
use serde_json::{Value as Json, json};

/// One test's private project on the local emulator.
struct Emulator {
    project: String,
}

/// Whether `host` (a `FIRESTORE_EMULATOR_HOST` value) is a loopback `host:port`.
fn is_local(host: &str) -> bool {
    ["localhost:", "127.0.0.1:", "[::1]:"]
        .iter()
        .any(|prefix| host.starts_with(prefix))
}

impl Emulator {
    /// The project `demo-fsq-<name>` on the emulator. Fails the test when no local
    /// emulator is configured.
    fn project(name: &str) -> Self {
        let host = std::env::var("FIRESTORE_EMULATOR_HOST").unwrap_or_default();
        assert!(
            is_local(&host),
            "FIRESTORE_EMULATOR_HOST must be a local host:port (got {host:?}); run these \
             tests through `firebase emulators:exec`"
        );
        Self {
            project: format!("demo-fsq-{name}"),
        }
    }

    /// `fsq` pointed at this project, allowed to write to it.
    fn fsq(&self, args: &[&str]) -> Command {
        let mut cmd = Command::cargo_bin("fsq").expect("fsq binary builds");
        cmd.env_remove("FSQ_DATABASE")
            .env_remove("FSQ_COMPLETE")
            .env("XDG_CACHE_HOME", "/dev/null/fsq-test-cache")
            .env("FSQ_PROJECT", &self.project)
            .env("FSQ_WRITABLE", format!("{}/(default)", self.project))
            .args(args);
        cmd
    }

    /// Runs `fsq`, expecting exit 0, and returns its stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.fsq(args).assert().success().get_output().clone();
        String::from_utf8(out.stdout).expect("utf-8 output")
    }

    /// Runs `fsq` with `stdin`, expecting exit 0, and returns its stdout.
    fn ok_with_stdin(&self, args: &[&str], stdin: &str) -> String {
        let out = self
            .fsq(args)
            .write_stdin(stdin)
            .assert()
            .success()
            .get_output()
            .clone();
        String::from_utf8(out.stdout).expect("utf-8 output")
    }

    /// Runs `fsq`, expecting exactly exit code `code`.
    fn fails_with(&self, args: &[&str], code: i32) {
        self.fsq(args).assert().code(code);
    }

    /// The JSON `fsq args` prints.
    fn json(&self, args: &[&str]) -> Json {
        parse(&self.ok(args))
    }

    /// Creates `path` from inline typed JSON.
    fn create(&self, path: &str, data: &Json) {
        self.ok(&["set", path, "--data", &data.to_string()]);
    }
}

fn parse(text: &str) -> Json {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

/// The `_path`s of a JSON array of documents.
fn paths(docs: &Json) -> Vec<&str> {
    docs.as_array()
        .expect("an array of documents")
        .iter()
        .map(|d| d["_path"].as_str().expect("_path"))
        .collect()
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn refuses_to_run_against_a_non_local_host() {
    assert!(is_local("localhost:8080"));
    assert!(is_local("127.0.0.1:8080"));
    assert!(is_local("[::1]:8080"));
    assert!(!is_local("firestore.googleapis.com:443"));
    assert!(!is_local("localhost.example.com:8080"));
    assert!(!is_local(""));
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn typed_document_survives_get_then_set_overwrite() {
    let emu = Emulator::project("roundtrip");
    emu.create(
        "things/t1",
        &json!({
            "name": "widget",
            "count": 7,
            "ratio": {"$double": 1.0},
            "tiny": {"$double": 0.1},
            "when": {"$ts": "2026-01-05T10:00:00.123456Z"},
            "owner": {"$ref": "users/u1"},
            "blob": {"$bytes": "aGk="},
            "place": {"$geo": {"lat": 1.5, "lng": -2.0}},
            "nothing": null,
            "flag": true,
            "nested": {"a": {"b": [1, {"$double": 2.0}, "x"]}},
            "escaped": {"$map": {"$odd": 1}},
            "tags": ["a", "b", {"$ref": "users/u2"}],
        }),
    );

    let first_text = emu.ok(&["get", "things/t1"]);
    let first = parse(&first_text);
    assert_eq!(first["_path"], "things/t1");
    let data = &first["data"];
    assert_eq!(data["count"], 7);
    assert_eq!(data["ratio"], json!({"$double": 1.0}));
    assert_eq!(data["when"], json!({"$ts": "2026-01-05T10:00:00.123456Z"}));
    assert_eq!(data["owner"], json!({"$ref": "users/u1"}));
    assert_eq!(data["blob"], json!({"$bytes": "aGk="}));
    assert_eq!(data["place"], json!({"$geo": {"lat": 1.5, "lng": -2.0}}));
    assert_eq!(data["nested"]["a"]["b"][1], json!({"$double": 2.0}));
    assert_eq!(data["escaped"], json!({"$map": {"$odd": 1}}));
    // Piped output is compact: one line.
    assert_eq!(first_text.trim_end().lines().count(), 1);

    // Feed the whole printed document back in.
    emu.ok_with_stdin(
        &["set", "things/t1", "--overwrite", "--data", "-"],
        &first_text,
    );

    let second = emu.json(&["get", "things/t1"]);
    assert_eq!(second["data"], first["data"]);
    assert_eq!(second["_path"], first["_path"]);
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn set_without_overwrite_conflicts_when_the_document_exists() {
    let emu = Emulator::project("exists");
    emu.create("users/u1", &json!({"n": 1}));
    emu.fails_with(&["set", "users/u1", "--data", r#"{"n": 2}"#], 4);
    assert_eq!(emu.json(&["get", "users/u1"])["data"], json!({"n": 1}));
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn missing_documents_exit_with_not_found() {
    let emu = Emulator::project("missing");
    emu.fails_with(&["get", "users/ghost"], 3);
    emu.fails_with(&["delete", "users/ghost"], 3);
    emu.fails_with(&["update", "users/ghost", "--set", "n=1"], 3);
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn delete_removes_an_existing_document() {
    let emu = Emulator::project("delete");
    emu.create("users/u1", &json!({"n": 1}));
    emu.ok(&["delete", "users/u1"]);
    emu.fails_with(&["get", "users/u1"], 3);
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn guarded_writes_fail_with_precondition_failed_when_stale() {
    let emu = Emulator::project("guard");
    emu.create("users/u1", &json!({"n": 1}));
    let old = emu.json(&["get", "users/u1"])["_updateTime"]
        .as_str()
        .expect("_updateTime")
        .to_owned();

    emu.ok(&["update", "users/u1", "--set", "n=2"]);
    let current = emu.json(&["get", "users/u1"])["_updateTime"]
        .as_str()
        .expect("_updateTime")
        .to_owned();
    assert_ne!(old, current);

    emu.fails_with(
        &[
            "set",
            "users/u1",
            "--overwrite",
            "--data",
            r#"{"n": 3}"#,
            "--if-unchanged-since",
            &old,
        ],
        5,
    );
    emu.fails_with(
        &[
            "update",
            "users/u1",
            "--set",
            "n=3",
            "--if-unchanged-since",
            &old,
        ],
        5,
    );
    emu.fails_with(&["delete", "users/u1", "--if-unchanged-since", &old], 5);
    assert_eq!(emu.json(&["get", "users/u1"])["data"], json!({"n": 2}));

    // The exact timestamp `get` printed is accepted (nanoseconds intact).
    emu.ok(&[
        "update",
        "users/u1",
        "--set",
        "n=4",
        "--if-unchanged-since",
        &current,
    ]);
    assert_eq!(emu.json(&["get", "users/u1"])["data"], json!({"n": 4}));
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn guarded_write_on_a_missing_document_is_a_precondition_failure() {
    let emu = Emulator::project("guard-missing");
    emu.fails_with(
        &[
            "set",
            "users/ghost",
            "--overwrite",
            "--data",
            r#"{"n": 1}"#,
            "--if-unchanged-since",
            "2026-01-05T10:00:00Z",
        ],
        5,
    );
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn update_applies_set_array_union_and_array_remove() {
    let emu = Emulator::project("update");
    emu.create("users/u1", &json!({"n": 1, "tags": ["a", "b"]}));
    emu.ok(&[
        "update",
        "users/u1",
        "--set",
        "n=2",
        "--set",
        "tags+=[b, c]",
        "--set",
        "address.city=\"Oslo\"",
    ]);
    assert_eq!(
        emu.json(&["get", "users/u1"])["data"],
        json!({"n": 2, "tags": ["a", "b", "c"], "address": {"city": "Oslo"}})
    );
    emu.ok(&["update", "users/u1", "--set", "tags-=a"]);
    assert_eq!(
        emu.json(&["get", "users/u1"])["data"]["tags"],
        json!(["b", "c"])
    );
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn query_filters_orders_and_limits() {
    let emu = Emulator::project("query");
    for (id, city, age) in [
        ("u1", "Tokyo", 31),
        ("u2", "Tokyo", 25),
        ("u3", "Oslo", 40),
        ("u4", "Tokyo", 52),
        ("u5", "Tokyo", 19),
    ] {
        emu.create(&format!("users/{id}"), &json!({"city": city, "age": age}));
    }

    let found = emu.json(&[
        "query",
        "users",
        "--where",
        r#"city == "Tokyo""#,
        "--order-by",
        "age:desc",
        "--limit",
        "3",
    ]);
    assert_eq!(paths(&found), ["users/u4", "users/u1", "users/u2"]);

    let range = emu.json(&[
        "query",
        "users",
        "--where",
        "age >= 25",
        "--where",
        "age < 50",
        "--order-by",
        "age",
        "--select",
        "age",
    ]);
    assert_eq!(paths(&range), ["users/u2", "users/u1", "users/u3"]);
    assert_eq!(range[0]["data"], json!({"age": 25}));

    let skipped = emu.json(&["query", "users", "--order-by", "age", "--offset", "3"]);
    assert_eq!(paths(&skipped), ["users/u3", "users/u4"]);
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn query_prints_one_document_per_line_as_jsonl() {
    let emu = Emulator::project("jsonl");
    emu.create("users/u1", &json!({"n": 1}));
    emu.create("users/u2", &json!({"n": 2}));
    let out = emu.ok(&["query", "users", "--order-by", "n", "--format", "jsonl"]);
    let lines: Vec<Json> = out.lines().map(parse).collect();
    assert_eq!(
        lines.iter().map(|d| d["_path"].clone()).collect::<Vec<_>>(),
        [json!("users/u1"), json!("users/u2")]
    );
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn count_sum_and_avg_are_computed_on_the_server() {
    let emu = Emulator::project("count");
    for n in 1..=4 {
        emu.create(&format!("nums/n{n}"), &json!({"n": n, "even": n % 2 == 0}));
    }

    assert_eq!(emu.json(&["count", "nums"]), json!({"count": 4}));
    assert_eq!(
        emu.json(&["count", "nums", "--where", "even == true"]),
        json!({"count": 2})
    );
    let all = emu.json(&["count", "nums", "--sum", "n", "--avg", "n"]);
    assert_eq!(all["count"], 4);
    assert_eq!(all["sum_n"], 10);
    assert_eq!(all["avg_n"], json!({"$double": 2.5}));
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn collection_group_query_reads_every_depth() {
    let emu = Emulator::project("group");
    emu.create("shops/s1/orders/o1", &json!({"total": 10}));
    emu.create("shops/s2/orders/o2", &json!({"total": 20}));
    emu.create("shops/s2/stalls/x1/orders/o3", &json!({"total": 30}));
    emu.create("orders/o4", &json!({"total": 40}));
    emu.create("shops/s1/invoices/i1", &json!({"total": 99}));

    let all = emu.json(&["query", "--group", "orders", "--order-by", "total"]);
    assert_eq!(
        paths(&all),
        [
            "shops/s1/orders/o1",
            "shops/s2/orders/o2",
            "shops/s2/stalls/x1/orders/o3",
            "orders/o4"
        ]
    );

    let below = emu.json(&["query", "--group", "orders", "shops/s2"]);
    assert_eq!(paths(&below).len(), 2);

    assert_eq!(
        emu.json(&["count", "--group", "orders", "--where", "total > 15"]),
        json!({"count": 3})
    );
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn collections_lists_root_and_subcollections() {
    let emu = Emulator::project("collections");
    emu.create("users/u1", &json!({"n": 1}));
    emu.create("users/u1/orders/o1", &json!({"n": 1}));
    emu.create("users/u1/notes/n1", &json!({"n": 1}));
    emu.create("teams/t1", &json!({"n": 1}));

    let mut below = emu.json(&["collections", "users/u1"]);
    below
        .as_array_mut()
        .expect("array")
        .sort_by_key(Json::to_string);
    assert_eq!(below, json!(["notes", "orders"]));

    let mut root = emu.json(&["collections"]);
    root.as_array_mut()
        .expect("array")
        .sort_by_key(Json::to_string);
    assert_eq!(root, json!(["teams", "users"]));
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn writes_are_refused_without_a_matching_writable_entry() {
    let emu = Emulator::project("readonly");
    emu.fsq(&["set", "users/u1", "--data", r#"{"n": 1}"#])
        .env_remove("FSQ_WRITABLE")
        .assert()
        .code(6);
    emu.fsq(&["set", "users/u1", "--data", r#"{"n": 1}"#])
        .env("FSQ_WRITABLE", "(default)")
        .assert()
        .code(1);
    emu.fails_with(&["get", "users/u1"], 3);
}
