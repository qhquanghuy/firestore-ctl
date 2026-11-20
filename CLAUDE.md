# firectl

Rust CLI for Firestore, built for scripts and coding agents. Details: README.md.

## Commands (inside `nix develop`, or prefix `nix develop -c`)

- `cargo clippy --all-targets` (every warning is an error), `cargo fmt`
- `cargo nextest run`
- Emulator suite: `firebase emulators:exec --only firestore --project demo-firectl 'cargo nextest run --test emulator --run-ignored all'`
- Coverage (report-only, no gate): `cargo llvm-cov nextest --summary-only` (unit, CLI, app_flow; the emulator tests are ignored without an emulator), then `cargo llvm-cov report --html` for a report under `target/llvm-cov/html`. For missed lines use `--lcov --output-path <file>`.
- `nix flake check`: what CI runs (fmt, clippy, rustdoc, tests, build). `git add` new files first; flakes only see tracked files.

## Layout

`domain` types, no I/O. `parse` user text to domain. `codec` JSON to and from domain.
`policy` write guard (`FIRECTL_WRITABLE`). `app` use cases. `port` the `Firestore` trait, the
only trait. `grpc` its real adapter. `render` output to `String`. `cli` clap definitions.
`complete` shell completion. `exit` error to exit code.

Flow in `main.rs`: parse args, `app::authorize` (policy gate), `grpc::connect`, `app::run`,
`render`. `main.rs` is the only place that prints or exits.

## Rules the tooling cannot check

- Speak and name things in the terms of `docs/glossary.md`. Add a term there before introducing a new concept.
- Read `docs/decisions.md` before changing behaviour. Record a new or reversed decision there (edit, don't contradict).

- A trait only where there are several impls. No new trait or abstraction for one impl.
- Newtypes and parse, don't validate: make illegal states unrepresentable.
- Immutable first; `mut` only where it is clearer than the alternative.
- Comments state decisions only: no what, param or return docs. `///` on clap items is `--help` text.
- No test may reach real GCP. The emulator suite refuses non-local hosts; keep it that way.
- `FIRECTL_WRITABLE` must never appear in `--help` or in the write-denied message.
- Typed JSON must round-trip losslessly (`get` output fed to `set --overwrite`).
- Compare `--help` before and after any CLI change; it is a contract.

## Compile and lint on the first try

Lints are in `Cargo.toml`/`clippy.toml`; these are the ones that trip agents.

- No `unwrap`, `expect`, `panic!`, indexing or `&s[a..b]`: use `?`, `.get()`, `ok_or`,
  `split_at_checked`, `strip_prefix`, `split_once` (see `parse::split_around`). Tests may
  unwrap; helpers in `tests/` need a reasoned `#![expect(...)]` on the crate or `mod support`.
- Suppress with `#[expect(lint, reason = "why")]`, never `#[allow]`. A stale `expect` fails.
- Match every variant explicitly; no `_ =>`. For one variant use `if let ... else` or let-else.
- Printing, `exit` and `{:?}` in user output are denied; return a `String` and let `main.rs` write it.
- `&str` to owned: `.to_owned()`, not `.to_string()`. Appending: `push_str`/`push`, not `+`.
- Trait imported only for its methods: `use std::io::Write as _;`. Use `Self` inside impls.
- Items not part of the public API are `pub(crate)` or `pub(super)`; `pub` only when exported.
- No `mod.rs`: use `foo.rs` beside `foo/`.
- Discard a `Result` with `.ok()`, not `let _ =`; name a dropped error `|_why|`, not `|_|`.
- Pedantic: public fns returning a value get `#[must_use]`; avoid lossy `as` casts (use
  `try_from`); `.map(|x| format!(..)).collect::<String>()` is denied, push into a `String`.
- Any warning, even an unused variable mid-edit, fails the build; finish the edit before checking.
