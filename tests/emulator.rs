//! Each test uses its own `demo-` project, cleared first so the suite can be re-run, and refuses to
//! run unless `FIRESTORE_EMULATOR_HOST` names a local address: these tests write, and must never
//! reach a real project.

#![expect(
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "test code fails by panicking"
)]

use std::collections::BTreeSet;
use std::io::{Read as _, Write as _};
use std::net::TcpStream;
use std::time::Duration;

use assert_cmd::Command;
use pretty_assertions::assert_eq;
use serde_json::{Value as Json, json};

struct Emulator {
    project: String,
}

fn is_local(host: &str) -> bool {
    ["localhost:", "127.0.0.1:", "[::1]:"].iter().any(|prefix| {
        host.strip_prefix(prefix)
            .is_some_and(|port| !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()))
    })
}

fn clear_project(host: &str, project: &str) {
    let request = format!(
        "DELETE /emulator/v1/projects/{project}/databases/(default)/documents HTTP/1.1\r\n\
         Host: {host}\r\nConnection: close\r\n\r\n"
    );
    let mut stream = TcpStream::connect(host).expect("emulator is reachable");
    // A hung emulator must fail the test, not hang the suite.
    let timeout = Some(Duration::from_secs(10));
    stream.set_read_timeout(timeout).expect("set read timeout");
    stream
        .set_write_timeout(timeout)
        .expect("set write timeout");
    stream
        .write_all(request.as_bytes())
        .expect("send clear request");
    // The emulator keeps the connection open, so stop at the end of the headers.
    let mut response = Vec::new();
    let mut chunk = [0_u8; 512];
    while !response.windows(4).any(|w| w == b"\r\n\r\n") {
        let n = stream.read(&mut chunk).expect("read clear response");
        assert!(n > 0, "emulator closed the connection clearing {project}");
        response.extend_from_slice(&chunk[..n]);
    }
    let response = String::from_utf8_lossy(&response);
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "clearing {project} failed: {response}"
    );
}

impl Emulator {
    fn project(name: &str) -> Self {
        let host = std::env::var("FIRESTORE_EMULATOR_HOST").unwrap_or_default();
        assert!(
            is_local(&host),
            "FIRESTORE_EMULATOR_HOST must be a local host:port (got {host:?}); run these \
             tests through `firebase emulators:exec`"
        );
        let project = format!("demo-firectl-{name}");
        clear_project(&host, &project);
        Self { project }
    }

    fn firectl(&self, args: &[&str]) -> Command {
        let mut cmd = Command::cargo_bin("firectl").expect("firectl binary builds");
        cmd.env_remove("FIRECTL_DATABASE")
            .env_remove("FIRECTL_COMPLETE")
            .env("XDG_CACHE_HOME", "/dev/null/firectl-test-cache")
            .env("FIRECTL_PROJECT", &self.project)
            .env("FIRECTL_WRITABLE", format!("{}/(default)", self.project))
            .args(args);
        cmd
    }

    fn ok(&self, args: &[&str]) -> String {
        let out = self.firectl(args).assert().success().get_output().clone();
        String::from_utf8(out.stdout).expect("utf-8 output")
    }

    fn ok_with_stdin(&self, args: &[&str], stdin: &str) -> String {
        let out = self
            .firectl(args)
            .write_stdin(stdin)
            .assert()
            .success()
            .get_output()
            .clone();
        String::from_utf8(out.stdout).expect("utf-8 output")
    }

    fn fails_with(&self, args: &[&str], code: i32) {
        self.firectl(args).assert().code(code);
    }

    fn json(&self, args: &[&str]) -> Json {
        parse(&self.ok(args))
    }

    fn create(&self, path: &str, data: &Json) {
        self.ok(&["set", path, "--data", &data.to_string()]);
    }
}

fn parse(text: &str) -> Json {
    serde_json::from_str(text).unwrap_or_else(|e| panic!("not JSON ({e}): {text}"))
}

fn names(array: &Json) -> Vec<&str> {
    array
        .as_array()
        .expect("an array")
        .iter()
        .map(|v| v.as_str().expect("a string"))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn paths(docs: &Json) -> Vec<&str> {
    docs.as_array()
        .expect("an array of documents")
        .iter()
        .map(|d| d["_path"].as_str().expect("_path"))
        .collect()
}

#[test]
fn is_local_accepts_only_loopback() {
    assert!(is_local("localhost:8080"));
    assert!(is_local("127.0.0.1:8080"));
    assert!(is_local("[::1]:8080"));
    assert!(!is_local("firestore.googleapis.com:443"));
    assert!(!is_local("localhost.example.com:8080"));
    assert!(!is_local("localhost:"));
    assert!(!is_local("localhost:80/x"));
    assert!(!is_local("127.0.0.1:80.evil.com:8080"));
    assert!(!is_local(""));
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn typed_document_survives_get_then_set_overwrite() {
    let emu = Emulator::project("roundtrip");
    let input = json!({
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
    });
    emu.create("things/t1", &input);

    let first_text = emu.ok(&["get", "things/t1"]);
    let first = parse(&first_text);
    assert_eq!(first["_path"], "things/t1");
    assert_eq!(first["data"], input);
    let data = &first["data"];
    assert_eq!(data["count"], 7);
    assert_eq!(data["ratio"], json!({"$double": 1.0}));
    assert_eq!(data["when"], json!({"$ts": "2026-01-05T10:00:00.123456Z"}));
    assert_eq!(data["owner"], json!({"$ref": "users/u1"}));
    assert_eq!(data["blob"], json!({"$bytes": "aGk="}));
    assert_eq!(data["place"], json!({"$geo": {"lat": 1.5, "lng": -2.0}}));
    assert_eq!(data["nested"]["a"]["b"][1], json!({"$double": 2.0}));
    assert_eq!(data["escaped"], json!({"$map": {"$odd": 1}}));
    assert_eq!(first_text.trim_end().lines().count(), 1);

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
    assert_eq!(
        paths(&below),
        ["shops/s2/orders/o2", "shops/s2/stalls/x1/orders/o3"]
    );

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

    assert_eq!(
        names(&emu.json(&["collections", "users/u1"])),
        ["notes", "orders"]
    );
    assert_eq!(names(&emu.json(&["collections"])), ["teams", "users"]);
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn writes_are_refused_without_a_matching_writable_entry() {
    let emu = Emulator::project("readonly");
    emu.firectl(&["set", "users/u1", "--data", r#"{"n": 1}"#])
        .env_remove("FIRECTL_WRITABLE")
        .assert()
        .code(6);
    emu.firectl(&["set", "users/u1", "--data", r#"{"n": 1}"#])
        .env("FIRECTL_WRITABLE", "(default)")
        .assert()
        .code(1);
    emu.fails_with(&["get", "users/u1"], 3);
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn a_writable_entry_for_another_project_does_not_allow_writes() {
    let emu = Emulator::project("other-proj");
    emu.firectl(&["set", "users/u1", "--data", r#"{"n": 1}"#])
        .env("FIRECTL_WRITABLE", "other-proj/(default)")
        .assert()
        .code(6);
    emu.fails_with(&["get", "users/u1"], 3);
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn set_merge_combines_nested_maps_field_by_field() {
    let emu = Emulator::project("merge");
    emu.create(
        "users/u1",
        &json!({"address": {"city": "Y", "zip": "1"}, "n": 1}),
    );
    emu.ok(&[
        "set",
        "users/u1",
        "--merge",
        "--data",
        r#"{"address": {"city": "X"}}"#,
    ]);
    let merged = emu.json(&["get", "users/u1"]);
    assert_eq!(
        merged["data"],
        json!({"address": {"city": "X", "zip": "1"}, "n": 1})
    );

    let stale = merged["_updateTime"]
        .as_str()
        .expect("_updateTime")
        .to_owned();
    emu.ok(&["update", "users/u1", "--set", "n=2"]);
    emu.fails_with(
        &[
            "set",
            "users/u1",
            "--merge",
            "--data",
            r#"{"n": 3}"#,
            "--if-unchanged-since",
            &stale,
        ],
        5,
    );
    assert_eq!(emu.json(&["get", "users/u1"])["data"]["n"], 2);
    let fresh = emu.json(&["get", "users/u1"])["_updateTime"]
        .as_str()
        .expect("_updateTime")
        .to_owned();
    emu.ok(&[
        "set",
        "users/u1",
        "--merge",
        "--data",
        r#"{"n": 3}"#,
        "--if-unchanged-since",
        &fresh,
    ]);
    assert_eq!(emu.json(&["get", "users/u1"])["data"]["n"], 3);
}

fn update_time(emu: &Emulator, path: &str) -> String {
    emu.json(&["get", path])["_updateTime"]
        .as_str()
        .expect("_updateTime")
        .to_owned()
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn writing_back_a_stale_document_fails_unless_forced() {
    let emu = Emulator::project("stale-input");
    emu.create("users/u1", &json!({"n": 1, "m": 1}));
    let read = emu.ok(&["get", "users/u1"]);
    emu.ok(&["update", "users/u1", "--set", "n=2"]);
    let edited = read.replace(r#""n":1"#, r#""n":9"#);
    assert_ne!(edited, read);

    for mode in ["--overwrite", "--merge"] {
        emu.firectl(&["set", "users/u1", mode, "--data", "-"])
            .write_stdin(edited.clone())
            .assert()
            .code(5);
    }
    assert_eq!(
        emu.json(&["get", "users/u1"])["data"],
        json!({"n": 2, "m": 1})
    );

    emu.ok_with_stdin(
        &["set", "users/u1", "--overwrite", "--force", "--data", "-"],
        &edited,
    );
    assert_eq!(
        emu.json(&["get", "users/u1"])["data"],
        json!({"n": 9, "m": 1})
    );

    let fresh = emu.ok(&["get", "users/u1"]);
    emu.ok_with_stdin(&["set", "users/u1", "--merge", "--data", "-"], &fresh);

    let current = update_time(&emu, "users/u1");
    emu.firectl(&[
        "set",
        "users/u1",
        "--overwrite",
        "--data",
        "-",
        "--if-unchanged-since",
        "2020-01-01T00:00:00Z",
    ])
    .write_stdin(fresh.clone())
    .assert()
    .code(5);
    emu.ok_with_stdin(
        &[
            "set",
            "users/u1",
            "--overwrite",
            "--data",
            "-",
            "--if-unchanged-since",
            &current,
        ],
        &fresh,
    );
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn plain_output_cannot_be_set_back() {
    let emu = Emulator::project("plain-guard");
    emu.create(
        "users/u1",
        &json!({"when": {"$ts": "2026-01-05T10:00:00Z"}}),
    );
    let plain = emu.ok(&["get", "users/u1", "--plain"]);
    assert_eq!(parse(&plain)["_plain"], json!(true));
    for mode in ["--overwrite", "--merge"] {
        emu.firectl(&["set", "users/u1", mode, "--data", "-"])
            .write_stdin(plain.clone())
            .assert()
            .code(2);
    }
    assert_eq!(
        emu.json(&["get", "users/u1"])["data"]["when"],
        json!({"$ts": "2026-01-05T10:00:00Z"})
    );
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn a_select_projection_can_be_merged_but_not_overwritten() {
    let emu = Emulator::project("select-guard");
    emu.create("users/u1", &json!({"a": 1, "b": 2, "c": 3}));
    let projected = emu.json(&["query", "users", "--select", "a,b"]);
    assert_eq!(projected[0]["_select"], json!(["a", "b"]));
    assert_eq!(projected[0]["data"], json!({"a": 1, "b": 2}));

    let doc = projected[0].to_string().replace(r#""a":1"#, r#""a":10"#);
    emu.firectl(&["set", "users/u1", "--overwrite", "--data", "-"])
        .write_stdin(doc.clone())
        .assert()
        .code(2);
    assert_eq!(
        emu.json(&["get", "users/u1"])["data"],
        json!({"a": 1, "b": 2, "c": 3})
    );

    emu.ok_with_stdin(&["set", "users/u1", "--merge", "--data", "-"], &doc);
    assert_eq!(
        emu.json(&["get", "users/u1"])["data"],
        json!({"a": 10, "b": 2, "c": 3})
    );
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn a_field_named_data_is_data_not_an_envelope() {
    let emu = Emulator::project("field-data");
    emu.create("users/u1", &json!({"data": {"n": 1}}));
    assert_eq!(
        emu.json(&["get", "users/u1"])["data"],
        json!({"data": {"n": 1}})
    );
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn a_query_stopping_at_the_default_limit_says_so_on_stderr() {
    let emu = Emulator::project("truncation");
    for i in 0..100 {
        emu.create(&format!("nums/n{i:03}"), &json!({"i": i}));
    }
    let note = "note: showing the first 100 documents; use --limit N or --all\n";
    let run = |args: &[&str]| {
        let out = emu.firectl(args).assert().success().get_output().clone();
        (
            String::from_utf8(out.stdout).expect("utf-8 stdout"),
            String::from_utf8(out.stderr).expect("utf-8 stderr"),
        )
    };
    let (stdout, stderr) = run(&["query", "nums"]);
    assert_eq!(stderr, note);
    assert_eq!(parse(&stdout).as_array().map(Vec::len), Some(100));
    for args in [
        &["query", "nums", "--limit", "100"][..],
        &["query", "nums", "--all"],
        &["query", "nums", "--limit", "5"],
    ] {
        assert_eq!(run(args).1, "", "{args:?}");
    }
    emu.create("nums/extra", &json!({"i": 100}));
    assert_eq!(run(&["query", "nums"]).1, note);
    assert_eq!(
        parse(&run(&["query", "nums", "--all"]).0)
            .as_array()
            .map(Vec::len),
        Some(101)
    );
}

#[test]
#[ignore = "needs FIRESTORE_EMULATOR_HOST"]
fn references_into_another_database_roundtrip() {
    let emu = Emulator::project("foreign-ref");
    let foreign = "projects/some-other-project/databases/(default)/documents/users/u9";
    emu.create(
        "things/t1",
        &json!({"mine": {"$ref": "users/u1"}, "theirs": {"$ref": foreign}}),
    );
    let read = emu.json(&["get", "things/t1"]);
    assert_eq!(read["data"]["mine"], json!({"$ref": "users/u1"}));
    assert_eq!(read["data"]["theirs"], json!({"$ref": foreign}));
    let plain = emu.json(&["get", "things/t1", "--plain"]);
    assert_eq!(plain["data"]["theirs"], json!(foreign));
    emu.ok_with_stdin(
        &["set", "things/t1", "--overwrite", "--data", "-"],
        &read.to_string(),
    );
    assert_eq!(emu.json(&["get", "things/t1"])["data"], read["data"]);
}
