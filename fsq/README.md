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
nix profile install .#    # put fsq on your PATH
nix develop               # dev shell: rust toolchain, cargo-nextest, firebase-tools, jdk
nix flake check           # fmt, clippy (-D warnings), tests
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
2. Otherwise (or if ADC fails) a token from `gcloud auth print-access-token`.

If `FIRESTORE_EMULATOR_HOST` is set, it takes precedence: requests go to that emulator over
plain HTTP with no Google credentials (project ids like `demo-foo` need no login).

Not logged in or expired credentials exit with code 8 and say how to log in.

## Write safety

`set`, `update` and `delete` only work on databases you list in `FSQ_WRITABLE`. Unset or
empty means read-only; a write then fails with exit code 6.
This is a guardrail against unintended writes by scripts and agents, not a security
boundary: whoever controls the environment can set the variable. Set it yourself, per
session or per database, rather than in a shared profile. It is not mentioned in `--help`.

```sh
export FSQ_WRITABLE='my-project/(default),staging-db'
```

- A comma-separated list of `database` (that database in any project) or
  `project/database` (exactly that one).
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

### Typed JSON

`get`, `query` and `set --data` use `{"_path": ..., "_updateTime": ..., "data": {...}}`
and tag values JSON cannot express:

| Tag | Meaning |
| --- | --- |
| `{"$ts": "2026-01-05T10:00:00.123456Z"}` | timestamp |
| `{"$ref": "users/u1"}` | document reference |
| `{"$double": 1.5}` | double (`"NaN"`, `"Infinity"` as strings); a bare JSON integer is an integer |
| `{"$bytes": "aGk="}` | bytes, standard base64 with padding |
| `{"$geo": {"lat": 1.5, "lng": -2.0}}` | geo point |
| `{"$map": {...}}` | escape: a map whose only key starts with `$` |

`get` output can be fed straight back to `set --overwrite --data -` (its `_path` must match
the target). `--plain` prints untagged JSON for humans and `jq`; it cannot be read back
losslessly. Firestore stores timestamps with microsecond precision.

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

```sh
t=$(fsq get users/u1 | jq -r ._updateTime)
fsq update users/u1 --set status=\"done\" --if-unchanged-since "$t"
```

### Limits

`query` returns at most 100 documents unless `--limit N` or `--all`. Every call has a 60
second deadline.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | ok |
| 1 | other error |
| 2 | usage error (bad flag, path, filter, field op or `--data`) |
| 3 | not found |
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
emulator. Its tests are `#[ignore]`d, use their own `demo-` projects, and refuse to run
unless `FIRESTORE_EMULATOR_HOST` is a local address. `firebase.json` configures the
emulator (port 8080). The first run downloads the emulator jar.

```sh
nix develop -c firebase emulators:exec --only firestore --project demo-fsq \
  'cargo nextest run --test emulator --run-ignored all'
```
