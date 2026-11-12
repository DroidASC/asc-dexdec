# asc-dexdec-worker

This repository is upstream [dexdec](https://github.com/asLody/dexdec)
(Apache-2.0, by asLody and contributors) plus two additions:

- `asc-dexdec-worker`, the program [asc](https://github.com/Ylarod/asc-rs) runs
  as its built-in `dexdec` decompile backend. Everything in this directory is
  glue that serves asc's decompile backend protocol (asc-rs
  `docs/decompile-backends.md` §5–§6) on top of dexdec's public API.
- User name mappings in the library: `Decompiler::open_with_names` reads a
  program under the class, field and method names of a mapping the user
  supplies (asc translates R8 / ProGuard `mapping.txt`). The renaming lives in
  `dexdec/src/frontend/names*` and is applied to rusty-dex's type, prototype,
  field and method tables right after the program is opened (with a few
  generic accessors added to `rusty-dex`). Without mappings, nothing changes.

Otherwise the decompiler (`dexdec/`, `rusty-dex/`, …) is upstream's code.

## How asc finds it

asc looks for `asc-dexdec-worker`, in order: the `command` of
`[decompilers.dexdec]` in its config; the directory `asc` itself is in; the
copy asc installed under `$XDG_DATA_HOME/asc/tools/asc-dexdec-worker-p<P>-<target>/`
(`~/.local/share/asc/…` by default); `PATH`.

When none is found, asc downloads
`asc-dexdec-worker-p<P>-<target>.tar.gz` and its `.sha256` from
`https://github.com/DroidASC/asc-dexdec/releases/latest/download/`, where `P`
is asc's backend protocol version, checks the digest and installs it
(`asc install dexdec` does the same on demand). The worker carries upstream
dexdec's version (the root workspace's, kept equal by `tests/version.rs`),
independent of asc's; only the protocol version has to match, which asc checks
with `asc-dexdec-worker --protocol-version`.

## Build

asc-rs must be checked out next to this repository, as `../asc-rs`: the worker
takes the protocol crate from it.

```sh
git clone https://github.com/Ylarod/asc-rs.git ../asc-rs
cd asc-dexdec-worker
cargo build --release          # target/release/asc-dexdec-worker
cargo test
```

The worker is a Cargo workspace of its own, so upstream's workspace (the
library, CLI, MCP server and desktop app) builds as before without asc-rs.
Put the binary next to `asc`, or name it in asc's config:

```toml
[decompilers.dexdec]
command = "/path/to/asc-dexdec-worker"
```

## The program

- No arguments: serve the protocol on stdin / stdout until stdin ends. Only
  protocol frames go to stdout; on Unix, stdout is moved onto stderr after
  the start, so anything else printed lands in the log asc keeps.
- `--version`: `asc-dexdec-worker <version>`. `--protocol-version`: the
  protocol version, one integer. Anything else: usage on stderr, exit code 2.

What `initialize` declares, and why:

| Field | Value |
|---|---|
| `backend` | `{"name": "dexdec", "version": <upstream dexdec's version>}` |
| `inputs` | `["archive"]`: one APK, jar or DEX file per program (upstream opens a single file); `slice` input is refused with `-33002` |
| `languages` | `["java", "kotlin"]` |
| `mappings` | `true`: see *Name mappings* below |
| `methodStatus` | `true`: methods dexdec prints as failure stubs are reported as `failed`, from its `AnalysisObserver` `MethodFailure` events; a unit with any is `partial` |
| `maxConcurrency` | `4`: requests on one program take turns, programs work in parallel |

- `options`: only `indent` (a string, Java and Kotlin alike); unknown keys are
  refused with `-32602`. `optionsHash` covers the options and the worker
  binary's size and modification time, so a rebuilt worker never replays
  cached output of the old one.
- Nested classes: with `nested` (the default), a nested class is answered with
  the unit of its top-level class when that unit folds it in, and every unit
  lists the nested classes whose code dexdec went through while generating it
  in `nestedClasses`. Otherwise the nested class is decompiled on its own.
- Name mappings: `program/open` `mappings` (`classes`: DEX descriptor → new
  descriptor; `fields`, `methods`: `Lowner;->member` → new simple name) open
  the program with `Decompiler::open_with_names`, for Java and Kotlin alike. A
  malformed entry is refused with `-32602`; a rename dexdec drops (two classes
  or members of one class to one name, a class onto a platform class) keeps the
  DEX's name and is logged (`asc-dexdec-worker: mapping: …`). The protocol keeps
  the DEX's names (`class`, `nestedClasses`, failed method signatures); the
  source text and its `path` show the new ones.
- Cancellation: `notifications/cancelled` stops a request waiting for its
  program at once, and a running one at dexdec's next cancellation checkpoint
  (between methods and pipeline stages); it is answered with `-32800`.
- Each class is decompiled on a 64 MiB-stack thread pool, and a panic fails
  that class only (`reason: "generation panicked: …"`). Progress
  notifications are sent per class when asked for.
- Not provided: `lines` (source line to dex pc) and `diagnostics`.

`ASC_DEXDEC_WORKER_FAULT=panic:<class>`, `stall:<class>` or `print:<class>`
injects a fault while decompiling that class; it exists for this crate's tests
and asc's, never set it otherwise.

## Releases

Pushing a `v<version>` tag (upstream dexdec's version, which
`asc-dexdec-worker/Cargo.toml` carries) builds the worker for x86_64 / aarch64 Linux, macOS and Windows and publishes
`asc-dexdec-worker-p<P>-<target>.tar.gz` (a directory holding the binary and
`LICENSE`) with its `.sha256` as the latest release. Worker-only changes on
the same upstream version are released as `v<version>-r<N>`. Upstream's own
tags are not fetched (`git config remote.upstream.tagOpt --no-tags`) and
never pushed here: they name upstream's releases, not this repository's.

## License

Apache-2.0, as upstream dexdec; see [LICENSE](../LICENSE).
