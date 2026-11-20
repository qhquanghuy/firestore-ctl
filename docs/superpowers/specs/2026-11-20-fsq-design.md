# fsq — Firestore query CLI — Design

Date: 2026-11-20
Status: Approved (design); builder-check questions resolved

## Intent

There is no CLI way to read Firestore documents. `gcloud firestore` and the
Firebase CLI only manage databases, indexes and import/export, and raw REST
calls are verbose. `fsq` is a small CLI client for Firestore, in the spirit of
psql/mongosh but shaped for one-shot use.

**Users:** the author and LLM coding agents. Typical use is simple: filter a
collection by one or two fields, fetch a document, count things, and
occasionally fix up a single document.

**Targets:** Firestore **native mode, Standard edition**, named databases (not
`(default)`), e.g. `app-db` (asia-southeast1) and
`jobs-db` (asia-northeast1). MongoDB compatibility is not
available on these databases, which is why this tool exists.

## Goals

- Query documents with filters, ordering, limit, field selection, and
  collection-group scope from a single shell command.
- Get, set, update and delete single documents.
- Aggregations: count, sum, avg.
- List collections / subcollections, and list composite indexes.
- Be agent-friendly: discoverable via `--help`, no interactive prompts,
  machine-readable output, actionable errors, nonzero exit codes on failure.
- Be human-friendly: table output in a TTY, zsh/bash tab-completion of
  collection and field names.

## Non-goals (v1)

- Interactive shell / REPL (may come later on the same internal command
  model; syntax for it — SDK-style chains or SQL subset — undecided).
- Multi-document writes (bulk update/delete by query), batches, transactions.
- Index creation/deletion, single-field overrides, TTL policies.
- Real-time listeners, import/export, MongoDB-compat databases, Datastore mode.

## Anti-goals

- An agent must never be able to modify data without the human having enabled
  writes for that database. Any write path that
  bypasses `FSQ_WRITABLE`, or that silently corrupts types on round-trip, is a
  failure even if it works.
- Not a schema/index management tool; indexes are owned by the team's normal
  deployment process (e.g. `firestore.indexes.json`).
- No query syntax that implies capabilities Firestore lacks (joins, GROUP BY,
  arbitrary OR across unindexed fields).

## Constraints

- Language: **Rust**.
- Dev environment and build via **Nix** (`flake.nix`): `nix develop` for the
  toolchain, `nix build` for the binary. No rustup on the machine.
- Auth via the user's existing gcloud identity; no service-account keys to
  manage. Application Default Credentials file does not currently exist on the
  author's machine, so the tool must either use ADC (and tell the user to run
  `gcloud auth application-default login`) or fall back to
  `gcloud auth print-access-token`.
- Lives in `<repo>` (currently empty).

## Approaches considered

Interface:

1. **Interactive SQL subset (psql-like)** — familiar, but the dialect needed
   so much invented syntax (`FROM GROUP`, backtick paths, `CONTAINS`, typed
   literals) and its `UPDATE … WHERE` hides non-atomic query+write semantics.
2. **Interactive SDK-style chains (mongosh-like)** — 1:1 with the SDK and
   completes well, but painful to quote inline.
3. **Subcommands + flags (gcloud/kubectl-like)** — **chosen.** Main users are
   the author and LLM agents running one-shot commands; this is the only form
   where real shell tab-completion works, needs no nested quoting, and each
   flag maps to one SDK method. An interactive shell can be added later.

Implementation stack:

- **Chosen:** Rust + `clap` (with dynamic completion) + the `firestore` crate
  (gRPC via `gcloud-sdk`). Alternatives rejected: Scala Native (no JVM
  Firestore SDK, would need a hand-written REST client); GraalVM + Kyo (viable,
  but slower builds, awkward Nix packaging, slower dev loop); raw REST via
  `reqwest` (reimplements auth/encoding/paging).

## Design (decided)

### Connection

- `--project <id>` / `--database <name>` flags, or `FSQ_PROJECT` /
  `FSQ_DATABASE` env vars. Flags win. Database defaults to `(default)` if
  unset.

### Commands

```bash
fsq get users/abc123
fsq query users --where 'status == active' --where 'age >= 18' \
    --order-by createdAt:desc --limit 10 [--offset N] [--select name,plan]
fsq query orders --group --where 'total > 100'     # collection group
fsq count users --where 'plan == pro'              # also --sum <field>, --avg <field>
fsq collections [users/abc123]                      # top-level or subcollections
fsq indexes users                                   # composite indexes + state
fsq set users/u1 --data '{"name":"Ann"}'            # create only; fails if exists
fsq set users/u1 --data @doc.json --overwrite       # replace existing document
fsq set users/u1 --data '{"plan":"pro"}' --merge    # merge given fields
fsq update users/u1 --set plan=pro --set 'tags+=vip'
fsq delete users/u1
```

- Paths are slash paths: odd segment count = collection, even = document.
- Multiple `--where` are ANDed.
- Operators: `==`, `!=`, `<`, `<=`, `>`, `>=`, `in`, `not-in`,
  `array-contains`, `array-contains-any`. Nested fields via dots
  (`address.city`).
- `update --set` supports `field=value`, `field+=value` (array-union),
  `field-=value` (array-remove). Field deletion and increment are left to the
  builder to decide syntax for, or defer.
- No `--write` flag: the command name already says it is a write. Write
  permission comes from `FSQ_WRITABLE` (see Writes and safety).

### Values

- In flags (`--where`, `--set`): bare tokens are typed automatically:
  integers, floats, `true`/`false`, `null`; everything else is a string.
  Quoting forces a string (`"18"`). Explicit prefixes: `ts:2026-01-01T00:00:00Z`
  (timestamp), `ref:users/abc` (document reference). `in`/`array-contains-any`
  take a list (e.g. `[a, b]`).
- In JSON (`set --data`, and all JSON output): **typed JSON** (below).

### Typed JSON (decided)

Firestore types that plain JSON can't represent are tagged, so `get` → edit →
`set` round-trips exactly. Strings, booleans, null, integers, arrays and maps
stay plain.

```json
{
  "_path": "users/abc",
  "_updateTime": "2026-11-20T03:12:45.123456Z",
  "data": {
    "name": "Ann",
    "createdAt": {"$ts": "2026-01-05T10:00:00Z"},
    "team": {"$ref": "teams/t1"},
    "score": {"$double": 1.0}
  }
}
```

- Tags: `$ts`, `$ref`, `$double` (needed so whole-number doubles don't become
  integers), `$bytes` (base64), `$geo` (`{"lat":…, "lng":…}`). Integers beyond
  2^53 are emitted as JSON numbers without precision loss (serialize from i64,
  not f64). Non-whole doubles may stay plain numbers. *(Exact tag spellings
  and whether non-whole doubles are tagged: builder's call, but output and
  input must use the same scheme.)*
- Document fields are nested under `data` so a user field named `id`/`_path`
  can't collide with metadata. `_updateTime` is included so it can feed
  `--if-unchanged-since` (builder's call, added to support guard #3).
- `set --data` accepts either just the `data` object or a whole `get` output
  object (metadata ignored, `data` used). `--data @file.json` reads from a
  file, `--data -` from stdin. *(Builder's call, for the round-trip workflow.)*
- `--plain` emits untagged, lossy JSON for reading only.

### Output

- Default format: TTY → table; not a TTY (piped, agent) → JSON.
  `--format json|jsonl|table` overrides.
- JSON layout: pretty in a TTY, compact when not a TTY (like `jq`/`gh`).
  `--format jsonl` is always compact, one document per line.
  `--pretty` / `--compact` override.
- `query` JSON output is an array of document objects; `count`/aggregations
  output a flat object (e.g. `{"count": 42}`).

### Writes and safety

- **Reads always allowed. Writes allowed only on databases listed in
  `FSQ_WRITABLE`**, a comma-separated list set by the human in their own
  shell profile. Entries are either `<database>` (that name in any project) or
  `<project>/<database>` (exact). No wildcard. Unset/empty = read-only
  everywhere.
- Refusal message says writes are disabled for this database, and must not
  explain how to enable them (agents shouldn't be told how to bypass it).
  Exit nonzero.
- Writes target exactly one document path; no query-driven writes in v1.
- No interactive prompts anywhere.
- Guards:
  - `set` is create-only by default (fails if the document exists).
    `--overwrite` replaces; `--merge` merges given fields. They are mutually
    exclusive.
  - `delete` fails if the document doesn't exist (precondition `exists`).
    `update` already fails on a missing document.
  - `--if-unchanged-since <updateTime>` on `set --overwrite`/`--merge`,
    `update` and `delete`: precondition on update time; the write fails if
    the document changed since.
  - Distinct exit codes for "not found", "already exists" and "precondition
    failed" vs. other errors (builder chooses values; document them in
    `--help`).

### Errors

- Human-readable message on stderr, nonzero exit code.
- Missing-index (`FAILED_PRECONDITION`) errors are rewritten to show the exact
  composite index needed and a ready-to-run `gcloud firestore indexes
  composite create …` command.

### Completion

- `fsq completions zsh|bash` installs/prints completion scripts.
- Dynamic completion of collection names (positional path) and field names
  (`--where`, `--order-by`, `--select`, `--set`). Field names come from
  sampling a few documents; results cached locally with a short TTL.

### Build

- Cargo project at the repo root (or `fsq/` subdirectory — builder's call).
- `flake.nix` with a devShell (rust toolchain, rust-analyzer, plus whatever
  native deps the crates need) and a package output (e.g. via crane).

## Left to the builder

- Crate versions; whether `firestore` crate covers the Admin API for
  `indexes` or `gcloud-sdk` is used directly for it.
- Exact auth fallback mechanics (ADC vs `gcloud auth print-access-token`).
- Table rendering crate, cache location/TTL for completion, default `--limit`.
- Test strategy (Firestore emulator for integration tests is suggested).
