---
title: Runtime Topology
---
# Layer: runtime-topology

amplihack-rs is a **CLI toolchain**, not a networked service mesh. It ships
**3 executables**:

- `amplihack` — main CLI (install, recipe run, orchestration, hooks staging)
- `amplihack-asset-resolver-bin` — asset resolver helper
- `amplihack-hooks-bin` — hooks executable (PreToolUse/PostToolUse, etc.)

These are invoked by the Copilot/Claude launcher and by the recipe runner as
subprocesses. There are no long-lived listening services in the workspace.

## Recursion and concurrency control

A recipe step can launch an agent, and that agent can launch another recipe. Left
unbounded, that fans out: on one 251 GiB host it exhausted memory four times. The
native recursion guard lives in `crates/amplihack-cli/src/commands/session_tree/`
and bounds the fan-out along three independent axes.

| control | env var | default | what it bounds |
| --- | --- | --- | --- |
| depth | `AMPLIHACK_MAX_DEPTH` | 3 | how many times a session may nest |
| width | `AMPLIHACK_MAX_SESSIONS` | 10 | concurrent sessions in one tree |
| memory floor | `AMPLIHACK_MIN_AVAILABLE_MIB` | 4096 | refuses to admit a session when `MemAvailable` is below this; `0` disables |

Each subprocess inherits `AMPLIHACK_TREE_ID`, `AMPLIHACK_SESSION_DEPTH` and
`AMPLIHACK_MAX_DEPTH`. Tree state is a JSON file per tree under
`$HOME/.amplihack/amplihack-session-trees/`. Admission and release are serialised
by two layers: an in-process mutex, because POSIX file locks are per-process
rather than per-thread, and a cross-process exclusive lock on a sidecar file.

The depth ceiling is **sealed** into the tree at first registration. The
environment may lower it but never raise it, and both the sealed and
environment values are clamped to a hard ceiling of 32, so a forged value cannot
disable the limit. This exists because agents treated a depth refusal as an
infrastructure fault and re-ran with a larger `AMPLIHACK_MAX_DEPTH` — the
observed escalation was 5 → 6 → 7 → 8 → 9.

The three controls are deliberately different in kind. The memory floor is a
capacity check evaluated against the machine's actual state at admission time,
so it adapts to the host. Depth is a fixed integer because it is a structural
invariant rather than a resource question: it is what stops unbounded recursion
when every other signal still looks healthy. Width is the one that is a plain
number today; bounding it by observed capacity rather than a constant would suit
it better, since 10 is simultaneously too many for a laptop and too few for a
64-core host.

This is **not** a security boundary. Any process running as the same user can
edit the tree state directly. It bounds accidental fan-out, not a hostile one.

![runtime-topology (mermaid)](runtime-topology-mermaid.svg)

![runtime-topology (dot)](runtime-topology-dot.svg)
