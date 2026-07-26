# Hooks Shared Helpers Reference

`amplihack-hooks` no longer depends on `amplihack-cli`. The shared helpers that
hooks and the CLI both need live in the lower-level crates `amplihack-utils`
(process/launcher/asset helpers) and `amplihack-memory` (code-graph and learning
helpers). Both `amplihack-cli` and `amplihack-hooks` depend *down* into these
crates, so the compile-dependency graph is a clean DAG with no layering
inversion (resolves [#875](https://github.com/rysweet/amplihack-rs/issues/875)).

This page documents the finished crate layout, the public API surface at the new
locations, the CLI back-compat re-exports, and how to validate that the
`amplihack-hooks → amplihack-cli` edge is gone.

## Why This Layout

`amplihack-cli` is the top-level aggregator — both the `amplihack` and
`amplihack-asset-resolver` binaries depend on it. When the low-level
`amplihack-hooks` crate depended *up* into `amplihack-cli`, three problems
followed:

- The standalone `amplihack-hooks-bin` had to compile the entire CLI surface.
- `amplihack-cli` could not be slimmed or split.
- The layering was inverted: an interceptor crate reached into the aggregator.

The fix moves only the helpers hooks actually consume into crates that already
sit below both hooks and cli. No behavior changes — function bodies were moved
verbatim, preserving timeout enforcement, ANSI sanitization, truncation bounds,
and `AMPLIHACK_HOME` path canonicalization.

## Crate Layering (After)

```
amplihack-types        (leaf types)
      ▲
      │
amplihack-utils  ──────────────┐         amplihack-memory
  launcher_context             │           code_index::*        (code-graph + learning)
  binary_finder                │           memory facade
  runtime_assets               │              ▲            ▲
  resolve_bundle_asset         │              │            │
  agent_binary::active_agent_binary          │            │
  test_env (test-only HOME lock)             │            │
      ▲                    ▲                 │            │
      │                    │                 │            │
amplihack-cli  ────────────┴─────────────────┘            │
  (pub use re-exports; public API unchanged)              │
      ▲                                                    │
      │                                                    │
amplihack-hooks  ───────────────────────────────────────┘
  (depends on utils + memory only — NO amplihack-cli edge)
```

`amplihack-utils` gained a dependency on `amplihack-types` (strictly lower
level, no cycle). `amplihack-memory` gained `prost` and `lbug` plus a no-op
`cxx-build` `build.rs` stub to link the code-graph backend.

## API: `amplihack-utils`

### `amplihack_utils::launcher_context`

Launcher context read/write helpers, moved verbatim from the CLI.

```rust
use amplihack_utils::launcher_context::{
    LauncherContext, LauncherKind, write_launcher_context,
};
```

| Symbol | Kind | Purpose |
| ------ | ---- | ------- |
| `LauncherContext` | struct | Serialized launcher metadata handed to child processes |
| `LauncherKind` | enum | Which launcher wrote the context (e.g. Claude, Copilot) |
| `write_launcher_context(...)` | fn | Persists a `LauncherContext` to the runtime location |

### `amplihack_utils::binary_finder`

```rust
use amplihack_utils::binary_finder::BinaryFinder;
```

`BinaryFinder` resolves external tool binaries (search order, timeout-bounded
probes). The three process/text helpers it relies on moved alongside it into
`amplihack-utils` under a neutral `proc_text` module (they are generic
process/string helpers, not binary-finder internals):

```rust
use amplihack_utils::proc_text::{
    run_output_with_timeout, strip_ansi, truncate_chars_with_notice,
};
```

| Symbol | Purpose |
| ------ | ------- |
| `run_output_with_timeout` | Run a command, enforcing a wall-clock timeout |
| `strip_ansi` | Remove ANSI escape sequences from captured output |
| `truncate_chars_with_notice` | Bound a string to N chars, appending a truncation notice |

> Companion fns kept private to the module: `run_output_with_timeout_limited`
> and `run_output_with_timeout_inner` moved together to preserve the shared
> timeout/child-termination path (behavior verbatim).

### `amplihack_utils::runtime_assets`

```rust
use amplihack_utils::runtime_assets::iter_runtime_roots;
```

| Symbol | Purpose |
| ------ | ------- |
| `iter_runtime_roots()` | Iterate candidate runtime asset root directories |

### `amplihack_utils::resolve_bundle_asset`

Bundle-asset resolution (search logic moved verbatim; `Cli`/`Commands`-coupled
cases were dropped, they remain CLI-only).

```rust
use amplihack_utils::resolve_bundle_asset::resolve_bundle_asset;
```

### `amplihack_utils::agent_binary::active_agent_binary`

Thin wrapper around `amplihack_utils::agent_binary::resolve`. It takes no
arguments — it resolves against the current working directory and falls back to
`amplihack_utils::agent_binary::DEFAULT_BINARY` on error (logging a warning).
Relocated so the named symbol hooks and the CLI already expect is preserved (no
call-site rewrites to `resolve` were needed).

```rust
use amplihack_utils::agent_binary::active_agent_binary;
```

### `amplihack_utils::test_env` (test-only)

Shared `HOME`-lock helpers for tests, behind a single process-wide `Mutex` so
utils and CLI tests share one lock instance (no duplicate lock). Gated to test
builds; absent from release artifacts.

## API: `amplihack-memory`

The code-graph and learning helpers hooks consume live under a namespaced
`code_index` module (isolated from the existing `graph_db` — no symbol merge),
and are re-exported through a `memory` facade that mirrors the old CLI one.

### `amplihack_memory::memory` facade

```rust
use amplihack_memory::memory::{
    CodeGraphSummary, IndexStatus, PromptContextMemory, SessionSummary,
    background_index_job_active, background_index_job_path, check_index_status,
    default_code_graph_db_path_for_project, record_background_index_pid,
    resolve_code_graph_db_path_for_project, retrieve_prompt_context_memories,
    store_session_learning, summarize_code_graph,
};
```

| Symbol | Kind | Purpose |
| ------ | ---- | ------- |
| `CodeGraphSummary` | struct | Summary of a project's code graph |
| `IndexStatus` | enum | Background index job state |
| `PromptContextMemory` | struct | A memory item injected into prompt context |
| `SessionSummary` | struct | Session-learning summary record |
| `background_index_job_active(...)` | fn | Whether a background index job is running |
| `background_index_job_path(...)` | fn | Path to the background index PID/state file |
| `check_index_status(...)` | fn | Current `IndexStatus` for a project |
| `default_code_graph_db_path_for_project(...)` | fn | Default graph DB path |
| `record_background_index_pid(...)` | fn | Record a background index job PID |
| `resolve_code_graph_db_path_for_project(...)` | fn | Resolve the graph DB path |
| `retrieve_prompt_context_memories(...)` | fn | Fetch prompt-context memories |
| `store_session_learning(...)` | fn | Persist a session-learning record |
| `summarize_code_graph(...)` | fn | Produce a `CodeGraphSummary` |

The CLI facade also exposes a hidden `memory::ffi_test_support` submodule
(`graph_rows`, `init_graph_backend_schema`, `list_graph_sessions_from_conn`)
used only by integration tests. These re-export from
`amplihack_memory::memory::ffi_test_support` for parity; they are
`#[doc(hidden)]` and not part of the stable surface.

### `amplihack_memory::code_index::code_graph`

Direct access to code-graph internals (used by tests and the blarify import
path):

```rust
use amplihack_memory::code_index::code_graph::import_blarify_json;
```

`import_blarify_json` ingests a blarify JSON export into the code-graph backend
using typed serde deserialization (unchanged). It is a module-level re-export of
the implementation in `code_graph/import.rs` (which also houses `import_scip_file`
and `summarize_code_graph`); the SCIP command surface stays CLI-side.

## CLI Back-Compat Re-Exports

`amplihack-cli`'s public API is unchanged. Every moved module is now a
`pub use` re-export, so existing consumers of `amplihack_cli::*` keep compiling
with zero source changes:

| Old CLI path (still valid) | Now re-exports from |
| -------------------------- | ------------------- |
| `amplihack_cli::binary_finder` | `amplihack_utils::binary_finder` |
| `amplihack_cli::launcher_context` | `amplihack_utils::launcher_context` |
| `amplihack_cli::runtime_assets` | `amplihack_utils::runtime_assets` |
| `amplihack_cli::resolve_bundle_asset` | `amplihack_utils::resolve_bundle_asset` |
| `amplihack_cli::env_builder::active_agent_binary` | `amplihack_utils::agent_binary::active_agent_binary` |
| `amplihack_cli::util::{run_output_with_timeout, strip_ansi, truncate_chars_with_notice}` | `amplihack_utils::proc_text` |
| `amplihack_cli::memory::*` | `amplihack_memory::memory::*` |
| `amplihack_cli::commands::memory::code_graph::import_blarify_json` | `amplihack_memory::code_index::code_graph::import_blarify_json` |

The CLI command surface (`clean`, `transfer`, `scip_indexing`, `agent_kv`,
`tree`, and the rest of the `commands/memory` tree) stays in `amplihack-cli` —
only the helper closure hooks consume was relocated.

## Migration Guide

If your crate previously reached into the CLI for these helpers, update the use
paths. The CLI re-exports keep old paths working, but new code should import
from the lower-level crate directly.

```rust
// Before
use amplihack_cli::binary_finder::BinaryFinder;
use amplihack_cli::launcher_context::{LauncherKind, write_launcher_context};
use amplihack_cli::runtime_assets::iter_runtime_roots;
use amplihack_cli::env_builder::active_agent_binary;
use amplihack_cli::memory::{resolve_code_graph_db_path_for_project, summarize_code_graph};
use amplihack_cli::commands::memory::code_graph::import_blarify_json;

// After
use amplihack_utils::binary_finder::BinaryFinder;
use amplihack_utils::proc_text::{run_output_with_timeout, strip_ansi, truncate_chars_with_notice};
use amplihack_utils::launcher_context::{LauncherKind, write_launcher_context};
use amplihack_utils::runtime_assets::iter_runtime_roots;
use amplihack_utils::agent_binary::active_agent_binary;
use amplihack_memory::memory::{resolve_code_graph_db_path_for_project, summarize_code_graph};
use amplihack_memory::code_index::code_graph::import_blarify_json;
```

In `Cargo.toml`, a hooks-like crate now declares:

```toml
[dependencies]
amplihack-utils = { workspace = true }
amplihack-memory = { workspace = true }
# amplihack-cli dependency removed
```

## Configuration

No new runtime configuration. Behavior-affecting environment variables continue
to work exactly as before — they are now read from the relocated helpers:

| Variable | Consumed by | Effect |
| -------- | ----------- | ------ |
| `AMPLIHACK_HOME` | `runtime_assets`, `resolve_bundle_asset` | Root for runtime asset/bundle resolution (path is canonicalized) |
| `AMPLIHACK_AGENT_BINARY` | `agent_binary::active_agent_binary` | Selects the active agent binary |

`amplihack-memory` enables its `sqlite` feature (default-on) for the
`code_index` backend, and links the code-graph backend via `lbug` + `prost`
pinned to the versions already in `Cargo.lock` (no version bump, no lockfile
churn).

## Validation

The extraction is verified by the following gate. All checks must pass.

**The dependency edge is gone:**

```sh
# No amplihack-cli entry in the hooks manifest
grep amplihack-cli crates/amplihack-hooks/Cargo.toml        # (empty)

# No amplihack_cli use-sites in hooks source
grep -rn amplihack_cli crates/amplihack-hooks/src           # (zero matches)

# Cargo agrees there is no path from hooks to cli
cargo tree -p amplihack-hooks -i amplihack-cli              # (empty / errors "not found")
```

**The workspace is healthy:**

```sh
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets      # clean, no warnings
cargo fmt --all --check
```

**The standalone binaries still build:**

```sh
cargo build -p amplihack-hooks-bin          # no longer pulls in the CLI
cargo build -p amplihack-signal
```

> Tip: for large builds, the saved workspace preference sets
> `NODE_OPTIONS=--max-old-space-size=32768`. Adjust in
> `~/.amplihack/config` if needed.

## Notes

- `session_start/context_loaders.rs` previously carried a doc comment that
  mirrored `amplihack_cli::VERSION` (which prefers `AMPLIHACK_RELEASE_VERSION`
  at build time, falling back to `CARGO_PKG_VERSION`). The comment now names
  that build-time version source directly instead of the CLI constant, since
  hooks no longer names the CLI crate. `amplihack_cli::VERSION` itself is
  unchanged and still available to CLI consumers.
- The `cxx-build` `build.rs` in `amplihack-memory` is a verbatim copy of the
  CLI's no-op stub — identical by diff, no new build-time network or process
  execution.
