# Glossary

The shared vocabulary of firectl. Use these words in code, docs, reviews and conversation.
Add a term here before introducing a new concept; rename code when a term changes.
`code` names are Rust types; the file is where they live.

## Target and paths

| Term | Meaning | Code |
|---|---|---|
| target | The project + database a command talks to. | `DbTarget` (domain/ids.rs) |
| project | A GCP project id. | `ProjectId` |
| database | A Firestore database id: `(default)` or a named one (e.g. `jobs-db`). | `DatabaseId` |
| collection path | Slash path with an odd number of segments: `users`, `users/u1/orders`. | `CollectionPath` (domain/path.rs) |
| doc path | Slash path with an even number of segments: `users/u1`. | `DocPath` |
| collection group | Every collection with a given id, at any depth (`--group`). | `Scope::Group` (domain/query.rs) |
| scope | What a query reads: one collection or a collection group. | `Scope` |
| field path | Dotted path into a document's data: `address.city`; odd names are backtick-quoted: `` `my-field` ``. | `FieldPath` (domain/field.rs) |
| resource name | Full Firestore name: `projects/P/databases/D/documents/...`. Only used at the gRPC edge and for foreign refs. | `DocResourceName` (domain/value.rs) |

## Data

| Term | Meaning | Code |
|---|---|---|
| value | One Firestore value (null, bool, int, double, string, timestamp, bytes, ref, geo, array, map). | `Value` (domain/value.rs) |
| fields | A document's data: field name → value. | `Fields` |
| document | Doc path + update time + fields. | `Document` (domain/document.rs) |
| update time | Server time of a document's last write; what a guard compares against. | `UpdateTime` |
| ref | Reference to a document in the same database (relative doc path). | `Value::Ref` |
| foreign ref | Reference to a document in another project/database (full resource name). | `Value::ForeignRef` |

## Typed JSON (what `get` prints and `set --data` reads)

| Term | Meaning | Code |
|---|---|---|
| typed JSON | Lossless JSON form of values; the default output. | codec/typed_json.rs |
| plain JSON | Untagged, human-readable, lossy output (`--plain`). Cannot be written back. | codec/plain_json.rs |
| tag | Single-key object marking a non-JSON type: `$ts $ref $double $bytes $geo $map`. | codec/typed_json.rs |
| envelope / full document | `{"_path", "_updateTime", "data"}`. Recognised by `_path`, `_updateTime`, or a marker. | `parse_input` |
| bare data | A JSON object that is just the fields (no envelope). | `parse_input` |
| marker | Top-level key flagging output that must not be written back as-is: `_plain`, `_select`. | codec |
| round trip | `get` → edit → `set --overwrite --data -` with no type or precision loss. | — |

## Reads

| Term | Meaning | Code |
|---|---|---|
| query | Scope + filters + order + limit/offset + select. | `Query` (domain/query.rs) |
| filter | `field op value`, e.g. `status == active`. | `Filter`, `Op` |
| projection / select | Only some fields returned (`--select`); output carries the `_select` marker. | `Query::select` |
| aggregation | `count`, `sum`, `avg` computed by the server. | `Aggregation`, `AggregateQuery` |
| default limit | 100 documents unless `--limit`/`--all`; hitting it prints a note on stderr. | cli.rs |
| index advice | What firectl prints when a query needs a missing index: a composite `gcloud` command, or (single-field) a description + console URL. | `IndexAdvice` (domain/index.rs) |

## Writes

| Term | Meaning | Code |
|---|---|---|
| write | One single-document mutation: set, update or delete. | `Write` (domain/write.rs) |
| set mode | `Create` (default, fails if exists), `Overwrite`, `Merge`. | `SetMode` |
| field op | `f=v` set, `f+=v` array-union, `f-=v` array-remove (not numeric increment). | `FieldOp` |
| guard | firectl's optimistic-concurrency check: "only if the document is unchanged since T". | `Guard` (`None`, `UnchangedSince`) |
| implicit guard | Guard taken from a full document's `_updateTime` on `set --overwrite/--merge`; `--force` skips it. | cli.rs |
| precondition | Firestore's own term (`exists` / `update_time`); a guard is sent as a precondition. Use "guard" for firectl's concept. | grpc/to_proto.rs |
| written | Result of a write: `Updated(UpdateTime)` or `Deleted`. | `Written` (port.rs) |

## Safety

| Term | Meaning | Code |
|---|---|---|
| write policy | Which targets may be written, parsed from `FIRECTL_WRITABLE`. Empty = read-only. | `WritePolicy` (policy.rs) |
| write access (grant) | Proof that the policy allowed a write to one target. Only `WritePolicy::authorize` creates it; `Firestore::write` requires it. | `WriteAccess` |
| write denied | The policy refused; exit 6. Message never names `FIRECTL_WRITABLE`. | `WriteDenied` |
| authorized request | A request cleared by policy; the only thing `app::run` accepts. | `Authorized` (app.rs) |

## Architecture

| Term | Meaning | Code |
|---|---|---|
| request | What the user asked for, parsed from the CLI. | `Request` (app.rs) |
| output | What a command produced, before rendering. | `Output` (app.rs) |
| port | The boundary to Firestore: the `Firestore` trait, the only trait. | `Firestore` (port.rs) |
| adapter | An implementation of the port: real gRPC, or the in-memory fake in tests. | `GrpcFirestore` (grpc.rs), `FakeFirestore` (tests/support/fake.rs) |
| gateway error | Failure reported by the port (not found, already exists, precondition failed, missing index, auth, permission…). "Gateway" = the port's error side. | `GatewayError` (port.rs) |
| render | Turning an `Output`/error into text; pure, returns `String`. | `RenderOpts`, `Format`, `Layout`, `Typing` (render.rs) |
| exit code | 0 ok, 1 other, 2 usage/input, 3 document not found, 4 already exists, 5 guard failed, 6 write denied, 7 missing index, 8 auth/permission. | exit.rs |

## Auth and connection

| Term | Meaning |
|---|---|
| credential source | Exactly one per run: emulator (`Bearer owner`), ADC (google-cloud-auth), or `gcloud auth print-access-token`. No silent fallback. |
| ADC | Application Default Credentials: `GOOGLE_APPLICATION_CREDENTIALS`, `~/.config/gcloud/application_default_credentials.json`, or GCP metadata. |
| routing headers | `google-cloud-resource-prefix` + `x-goog-request-params`, so requests reach the named database. |
| emulator | Local Firestore (`FIRESTORE_EMULATOR_HOST`); used by the end-to-end tests. |

## Completion

| Term | Meaning |
|---|---|
| dynamic completion | The shell calls `firectl` on TAB (`FIRECTL_COMPLETE=zsh|bash`) to get candidates. |
| lookup | Read-only Firestore call made during completion (list collections, sample fields), 2 s limit. |
| completion cache | `~/.cache/firectl/<project>/<database>/`: answers kept 10 min, failures 60 s (negative cache). |

## Quality

| Term | Meaning |
|---|---|
| coverage | Share of lines, functions and regions that tests execute, from `cargo llvm-cov`. Report-only; never a gate. |
| coverage badge | README badge fed by `coverage.json` on the orphan `badges` branch; line coverage of the last `main` run. |
| property test | A `proptest` test asserting a law (e.g. parse then print is the identity) over generated values, not hand-picked examples. |
| generator | A `proptest` strategy in `domain::arb` that builds only legal domain values; a test checks it reaches each class of value it claims. |
