# Decisions

Why firectl is the way it is. One entry per decision: what, why, and what it replaced or ruled out.
Newest decisions win; when one is reversed, edit its entry instead of adding a contradiction.
Terms: see `glossary.md`.

## Product

- **Purpose: a Firestore client for one human and LLM agents.** Typical use is filtering by one or two
  fields, fetching a document, counting, and occasionally fixing one document. Targets are native-mode,
  Standard-edition named databases, which are not MongoDB-compatible, so `mongosh` is not an option.
- **One-shot subcommands with flags, not an interactive shell.** Agents run single commands, read
  `--help`, can't answer prompts, and get real shell tab-completion with flags.
  - Ruled out: a SQL subset (it implies joins and GROUP BY that Firestore lacks, and needed invented
    syntax); SDK-style chains (nested quoting inline). A REPL can be added later on the same
    request model.
- **Rust, built with Nix.** Single static binary, instant start, simple flake packaging.
  - Ruled out: Scala Native (no JVM Firestore SDK); GraalVM + Kyo (slow native-image builds, awkward Nix).
- **Raw `gcloud-sdk` gRPC protos instead of the `firestore` crate.** Our own value and query model maps
  1:1 to the protos, and we need preconditions, transforms and the Admin API directly. Google has no
  official Rust Firestore *data* client yet; revisit when it ships.
- **Not in scope:** real-time listeners, import/export, multi-document or query-driven writes, batches,
  transactions, and index creation.

## Write safety

- **Writes are off by default; `FIRECTL_WRITABLE` lists the targets allowed.** Entries are `project/database`;
  a bare database name matches it in every project. A bare `(default)` is rejected, because it would match
  every project's default database. There are no wildcards.
  - Replaced: a `--write` flag. Agents simply add a flag; the env var is the human's switch.
- **It is a guardrail, not a security boundary.** An agent can set the variable itself. IAM is the real
  boundary; the README says so.
- **The denial message never names `FIRECTL_WRITABLE`, and neither does `--help`.** Don't teach agents the bypass.
- **Authorization is enforced by types.** Only `WritePolicy::authorize` creates a `WriteAccess`;
  `Firestore::write` requires one; `app::run` accepts only an `Authorized` request. The write's database
  comes from the grant, and the adapter refuses a grant for another target.
- **A denied write fails before reading `--data` or connecting.** It can't hang on stdin or reveal anything.
- **A malformed `FIRECTL_WRITABLE` fails writes only.** Reads keep working.
- **Single-document writes only, with preconditions:**
  - `set` is create-only by default (exit 4 if the document exists).
  - `--overwrite` replaces and `--merge` merges nested maps field by field.
  - `update` and `delete` require the document to exist (exit 3).
  - A guard (`--if-unchanged-since`) fails with exit 5 if the document is missing or changed.
- **Implicit guard.** A full document carrying `_updateTime`, fed to `set --overwrite/--merge`, is guarded by
  that time automatically; `--force` skips the guard. This protects agents' get → edit → set from lost updates.
- **`+=` / `-=` are array-union / array-remove, not increment.** This is the approved syntax. `--help` and the
  README warn that they turn a non-array field into an array.
- **Overlapping field ops in one update** (`a` with `a.b`, a field twice) are a usage error (exit 2).

## Data fidelity

- **Typed JSON is the default output and the only accepted input.** Tags `$ts $ref $double $bytes $geo` mark
  types JSON can't hold, so the round trip is exact.
  - Doubles are always tagged.
  - Integers never pass through f64; a bare number of 2^63 or more is an error that suggests `$double`.
  - `float_roundtrip` is used, not serde's crate-wide `arbitrary_precision`, which changed parsing for
    every crate in the graph, including auth.
- **`$map` escapes a map whose single key starts with `$`.** Without it, such a map would be read back as a tag.
- **Strict input.**
  - Unknown `$` tags, arrays directly inside arrays, and unknown top-level keys in a full document are errors.
  - An envelope is recognised only by `_path`, `_updateTime`, or a marker (`_plain`, `_select`); then fields must sit inside `data`, so `{"_select": ["a"], "a": 1}` is an error. `{"data": …}` alone is a field named `data`.
  - A full document's `_path` must match the target.
- **Markers on output that would lose data if written back.**
  - `--plain` adds `_plain` and `set` refuses it.
  - `--select` adds `_select` and `set` refuses it except with `--merge`.
- **References to another database are kept as full resource names** (`$ref: "projects/…"`).
  - Replaced: failing the whole read.
- **Timestamps:** Firestore stores field values to the microsecond. `_updateTime` is passed back verbatim,
  nanoseconds included.
- **Flag values.**
  - Bare tokens are typed: numbers, `true`/`false`, `null`.
  - Leading zeros and a leading `+` stay strings.
  - Field names with spaces or operator characters need backticks, so `createdAt desc` can't silently
    become a field name.

## Reads and output

- **`query` returns 100 documents by default.** `--all` removes the limit; hitting the default prints a note on stderr.
- **Output format.**
  - On a terminal: a table, and pretty JSON when `--format json` is given.
  - Piped: compact JSON.
  - `jsonl` is always compact.
  - Agent-facing keys are camelCase (`_updateTime`, `collectionGroup`).
- **Control characters from stored data are escaped in tables and stderr.** Stored data must not drive the terminal.
- **Index advice.**
  - Composite: print the `gcloud … composite create` command.
  - Single-field (collection group): no command, because `gcloud … fields update --index` replaces the field's
    existing indexes. Describe the index and point to the console URL.
  - Aggregates with sum/avg: console URL only.
- **Exit codes:** 0 ok, 1 other, 2 usage/input, 3 document not found, 4 already exists, 5 guard failed,
  6 write denied, 7 missing index, 8 auth/permission. A missing database or project is 1, not 3, because
  3 must reliably mean "the document doesn't exist".

## Connection and auth

- **Exactly one credential source per run, with no silent fallback.**
  - Emulator (`FIRESTORE_EMULATOR_HOST`).
  - ADC, only if an ADC source exists (env var, the `$HOME` login file, or GCP signals). The metadata server
    is never probed on a laptop.
  - Otherwise `gcloud auth print-access-token`.
  - If ADC exists but fails, report the failure; never switch to the human's (possibly more privileged) gcloud identity.
- **Named-database routing on every call:** the `google-cloud-resource-prefix` header plus per-call
  `x-goog-request-params`. Requests must never land on `(default)` by accident.
- **Limits:** 10 s to get a token, 15 s to open the channel, 60 s per Firestore call (including stream
  drains). A hung call must not hang an agent.
- **`get` uses BatchGetDocuments.** A missing document arrives in-band, so NOT_FOUND can only mean a missing database.
- **Network failures are exit 1, not 8.** Exit 8 should mean "fix your login or IAM".

## Completion

- **Dynamic completion through `FIRECTL_COMPLETE`, not the generic `COMPLETE`.** A stray variable in an
  agent's environment can't hijack the program.
- **Best effort.**
  - 2 s budget, read-only lookups.
  - Never prints to stdout outside the protocol.
  - Any failure yields no candidates.
  - Panics are silenced only in release builds, so tests still see them.
- **Cache** under `~/.cache/firectl`: answers for 10 min, failures for 60 s, so offline TABs don't stall
  every time. The cache key includes the emulator host, and directories are 0700.
- **bash.**
  - Candidates are shell-escaped for the cursor's quote state.
  - When the quote state is unknown, only plain names are offered (fail closed).
  - zsh needs none of this.
- **Only bash and zsh.** That is what's tested and documented.
- **Depends on `clap_complete` unstable-dynamic internals, pinned `~4.6`.** Re-check the bash script anchor on upgrade.

## Code and process

- **Every rule a compiler, linter or test can express is enforced there, at deny.**
  - All warnings are errors, plus clippy pedantic, rustdoc and a set of restriction lints: `#[expect(reason)]`
    over `#[allow]`, no wildcard enum arms, no panicking index or slice, `unreachable_pub`, and more.
  - The toolchain is pinned by `flake.lock`, so new lints arrive only on a deliberate update.
  - `CLAUDE.md` holds only what tooling can't check, plus tips for writing code that compiles first time.
- **Release builds trade build time for speed and size; dev builds for compile speed.** Release:
  `opt-level=3`, fat LTO, `codegen-units=1`, stripped, `overflow-checks` kept on (same overflow behaviour
  as the tests). `panic` stays `unwind`, because completion relies on `catch_unwind`. Dev: line tables
  only, no debuginfo for dependencies. Result: 12.9 MB → 9.4 MB binary.
- **One trait, `Firestore` (the port), because it has two implementations** (gRPC, in-memory fake).
  Everything else is concrete types and free functions.
- **Types carry meaning.** Newtypes, parse-don't-validate, illegal states unrepresentable. Names don't
  repeat what the context already implies (`ProjectId`, not `GcpProjectId`).
- **Comments state decisions only.** No what, param or return docs (`missing_errors_doc` is allowed).
  clap `///` docs are `--help` text.
- **Tests never reach real GCP.** CLI tests point at a dead emulator port; the emulator suite refuses
  non-local hosts and uses `demo-` projects.
- **Releases are cut by pushing a `v*` tag, and ship a static musl binary.** A Nix-built binary links
  against `/nix/store`, so it runs only where that store exists; the `static` flake package builds a
  fully static `x86_64-unknown-linux-musl` binary instead (default package unchanged, for Nix users).
  The tag must equal `v` plus the `Cargo.toml` version, or the release workflow fails. CI builds
  `.#static` too, so a broken static build is caught before tagging.
- **Public repo.** History was rewritten before publishing: internal names were removed and authors use a
  noreply email.
