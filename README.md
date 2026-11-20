# fsq

`fsq` queries Google Cloud Firestore from the command line: read documents, run
queries and counts, list collections and indexes, and (only where you allow it) write.
It is built to be driven by scripts and coding agents as well as people, so output is
machine-readable JSON when piped and writes are off unless you opt a database in.

Run `fsq --help` for the value syntax, field quoting, typed JSON tags and exit codes;
`fsq <command> --help` for one command.

## Install

```sh
nix build                 # ./result/bin/fsq
nix develop               # dev shell: rust toolchain, cargo-nextest, firebase-tools, jdk
nix flake check           # fmt, clippy (-D warnings), tests
```

To put `fsq` on your PATH, add the flake to your NixOS or home-manager config:

```nix
# flake.nix inputs (private repo: the ssh URL uses your SSH key)
inputs.fsq.url = "git+ssh://git@github.com/qhquanghuy/fsq";

# home-manager (or environment.systemPackages on NixOS)
home.packages = [ inputs.fsq.packages.${pkgs.system}.default ];
```

## Connecting

Every command needs a project; the database defaults to `(default)`.

| Flag | Environment | Default |
| --- | --- | --- |
| `--project <id>` | `FSQ_PROJECT` | none (required) |
| `--database <id>` | `FSQ_DATABASE` | `(default)` |

Flags win over environment variables.

Authentication:

1. Application Default Credentials, if present (`GOOGLE_APPLICATION_CREDENTIALS`, the
   login from `gcloud auth application-default login`, or a Google-hosted environment).
   If ADC is present but fails (expired login, unreachable token endpoint), that failure
   is reported; `fsq` never falls back to another identity.
2. Only when there is no ADC source at all, a token from `gcloud auth print-access-token`.

If `FIRESTORE_EMULATOR_HOST` is set, it takes precedence: requests go to that emulator with
no Google credentials (project ids like `demo-foo` need no login). A bare `host:port` is
contacted over plain HTTP; a value with a scheme (`https://...`) is used as given.

Not logged in or expired credentials exit with code 8 and say how to log in.

## Write safety

`set`, `update` and `delete` only work on databases you list in `FSQ_WRITABLE`. Unset or
empty means read-only; a write then fails with exit code 6.
This is a guardrail against unintended writes by scripts and agents, not a security
boundary: whoever controls the environment can set the variable. Set it yourself, per
session or per database, rather than in a shared profile. It is not mentioned in `--help`.

```sh
export FSQ_WRITABLE='my-project/(default),my-project/staging-db'
```

Prefer exact `project/database` entries. A bare database name matches that database in EVERY
project you can reach, which is rarely what you mean.

- A comma-separated list of `project/database` (exactly that one) or `database` (that
  database in any project).
- The default database must be written `project/(default)`; a bare `(default)` is rejected
  because it would match every project's default database.
- No wildcards.
- A malformed value fails writes (exit 1) and leaves reads working.

## Commands

```sh
# Read
fsq get users/u1
fsq query users --where 'age >= 18' --where 'city == "New York"' --order-by age:desc --limit 20
fsq query users --select name,address.city --offset 40
fsq query users --all                        # no limit (default: 100 documents)
fsq query --group orders --where 'total > 100'     # every collection named "orders", any depth
fsq query --group orders shops/s1            # only below one document
fsq count users --where 'active == true' --sum balance --avg age
fsq collections                              # root collections
fsq collections users/u1                     # subcollections of a document
fsq indexes orders                           # composite indexes of a collection id

# Write (need FSQ_WRITABLE)
fsq set users/u1 --data '{"name": "Ann", "age": 30}'     # create; exit 4 if it exists
fsq set users/u1 --data @user.json --overwrite           # replace
fsq set users/u1 --data '{"age": 31}' --merge            # merge fields, create if missing
echo '{"name": "Ann"}' | fsq set users/u1 --data - --overwrite
fsq update users/u1 --set age=31 --set 'tags+=[vip, new]' --set 'old-=1' --set address.city=\"Oslo\"
fsq delete users/u1                          # exit 3 if it does not exist

# Shell completion
fsq completions zsh                          # prints the line to add to your rc file
```

`--where` takes a whole filter as one shell argument and repeats for AND. Operators:
`== != < <= > >= in not-in array-contains array-contains-any`.

### Value syntax

In `--where`, `--set` and lists: `null`, `true`, `false`, numbers (`18`, `-3`, `1.5`),
`"quoted text"` (escapes `\"` and `\\` only), `ts:2026-01-05T10:00:00Z`,
`ref:users/u1`, lists `[a, "b c", 3]`. Anything else is a string; numbers with leading
zeros (`007`) stay strings; quote a value to force a string (`"18"`).

Dots separate nested fields (`address.city`). A name with spaces or punctuation is
wrapped in backticks: `` `my-field` ``, `` `a b`.c ``.

### Updating fields

`update --set` takes `field=value` (set), `field+=v` and `field-=v`. These are **array
union** and **array remove**, not numeric increment or decrement. `+=` adds the value (or
each element of a list) if absent; `-=` removes every occurrence. Each field may be changed
once per update.

If the field is not an array (or is missing), `+=` and `-=` do not fail: they replace it with
an array holding the given values (`-=` leaves an empty array). Check the field first when
that would lose data.

### Typed JSON

`get`, `query` and `set --data` use `{"_path": ..., "_updateTime": ..., "data": {...}}`
and tag values JSON cannot express:

| Tag | Meaning |
| --- | --- |
| `{"$ts": "2026-01-05T10:00:00.123456Z"}` | timestamp |
| `{"$ref": "users/u1"}` | document reference in this database |
| `{"$ref": "projects/P/databases/D/documents/users/u1"}` | reference into another database or project (its full resource name; written back verbatim) |
| `{"$double": 1.5}` | double (`"NaN"`, `"Infinity"` as strings); a bare JSON integer is an integer |
| `{"$bytes": "aGk="}` | bytes, standard base64 with padding |
| `{"$geo": {"lat": 1.5, "lng": -2.0}}` | geo point |
| `{"$map": {...}}` | escape: a map whose only key starts with `$` |

`set --data` takes a bare object of fields (`{"name": "Ann"}`) or a whole document as
`get` prints it, which can be fed straight back to `set --overwrite --data -` (its `_path`
must match the target). A whole document is recognised by its `_path` or `_updateTime`
key; any other object is bare field data, so `{"data": {...}}` alone is a document with
one field named `data`. With `--merge`, nested maps are merged field by field: only the
fields you give are changed, siblings (including inside nested maps) are kept. Without
`--merge`, `--overwrite` replaces the whole document. Firestore stores timestamps with
microsecond precision.

Two kinds of output are lossy, so they are marked and `set` refuses to write them back:

- `--plain` prints untagged JSON for humans and `jq` and adds `"_plain": true` to every
  document. `set --data` rejects any input carrying it (exit 2): a timestamp
  would be written back as a string.
- `query --select a,b` documents carry `"_select": ["a", "b"]` and hold only those fields.
  `set --overwrite` (and a plain create) reject them (exit 2): they would drop every other
  field. `set --merge` accepts them and changes only the selected fields; `_select`
  itself is never stored.

The `_plain` and `_select` keys are markers wherever they appear at the top level of
`--data`, even after `_path` and `_updateTime` were stripped; to store a field with one of
those names, use the full-document form and put it inside `data`.

### Indexes

`fsq indexes` lists composite indexes through the Firestore Admin API, so it needs credentials
with the `datastore.indexes.list` permission, and it is not supported by the emulator.

When a query fails for want of an index (exit 7), the error shows the `gcloud` command for a
composite index and the console link. A collection-group query that needs only a single-field
index gets a description and the console link but no command: `gcloud firestore indexes
fields update --index` replaces all of a field's indexes, so it is not suggested. `count`
with `--sum` or `--avg` gets the console link only.

### Output

A table on a terminal, JSON otherwise (indented on a terminal, compact in a pipe); `jsonl`
is one compact document per line. Override with `--format table|json|jsonl`, `--pretty`,
`--compact`, `--plain`. Writes print `{"_path", "_updateTime"}`; deletes print
`{"_path", "deleted": true}`.

### Guarded writes

`--if-unchanged-since <_updateTime>` makes a write succeed only if the document is
unchanged since you read it. Pass the `_updateTime` from `get` verbatim (nanoseconds
included). The write exits 5 if the document changed or is missing. It works on `set`
(with `--overwrite` or `--merge`), `update` and `delete`.

`set --overwrite` and `set --merge` guard themselves when `--data` is a whole document
from `get`: its `_updateTime` is used as the guard, so get, edit, set fails with exit 5 if
the document changed meanwhile. Pass `--force` to overwrite anyway, or an explicit
`--if-unchanged-since` to name another time. Bare field data carries no time and is not
guarded.

```sh
t=$(fsq get users/u1 | jq -r ._updateTime)
fsq update users/u1 --set status=\"done\" --if-unchanged-since "$t"
```

### Limits

`query` returns at most 100 documents unless `--limit N` or `--all`; when it stops at exactly
100 by default, a note on stderr says there may be more (stdout is unchanged). Every call
has a 60 second deadline.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | ok |
| 1 | other error (including a missing database or project) |
| 2 | usage error (bad flag, path, filter, field op or `--data`) |
| 3 | document not found |
| 4 | already exists |
| 5 | precondition failed (changed since `--if-unchanged-since`, or missing) |
| 6 | writes disabled for this database |
| 7 | missing index (the error says which index to create) |
| 8 | authentication or permission failure |

## Completion

```sh
source <(FSQ_COMPLETE=zsh fsq)     # or FSQ_COMPLETE=bash; add to ~/.zshrc / ~/.bashrc
```

TAB completes subcommands and flags, and, using the database you are addressing
(`--project`/`FSQ_PROJECT`, `--database`/`FSQ_DATABASE`), collection ids and field names.
Lookups are best effort (no candidates on any failure, 2 second limit) and cached under
`~/.cache/fsq` (`$XDG_CACHE_HOME/fsq`): 10 minutes for answers, 60 seconds for failures, so
an offline shell is not slowed on every keypress. Field names come from sampled documents.

bash limitation: bash splits words at `:` and `=`, so a value typed after one (unquoted
`--order-by age:d`, `--set a=`) gets no completion. Quote it (`--order-by "age:d"`) or use
zsh, which does not split. Names that need quoting are shell-escaped for you.

## Tests

Unit and CLI tests need nothing: `cargo nextest run` (inside `nix develop`).

The end-to-end suite in `tests/emulator.rs` runs the real binary against the Firestore
emulator. Its tests (all but one, which runs anywhere) are `#[ignore]`d, use their own `demo-` projects, and refuse to run
unless `FIRESTORE_EMULATOR_HOST` is a local address. `firebase.json` configures the
emulator (port 8080). The first run downloads the emulator jar. Each test clears its own
project first, so the suite can be re-run against a running emulator.

The emulator cannot test named databases (it has only `(default)`), index requirements
(it does not enforce composite indexes, so exit 7 is not exercised) or `indexes`.

```sh
nix develop -c firebase emulators:exec --only firestore --project demo-fsq \
  'cargo nextest run --test emulator --run-ignored all'
```
