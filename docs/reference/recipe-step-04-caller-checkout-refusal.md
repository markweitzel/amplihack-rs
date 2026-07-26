# step-04-setup-worktree: Caller-Checkout Reuse Refusal (Issue #858)

`step-04-setup-worktree` creates an isolated git worktree for each workflow
run. When a workflow is invoked with an **existing branch** (via
`existing_branch` or `pr_number`), the step now **refuses fail-closed** to
reuse the caller's own checkout as the task worktree. This prevents a running
workflow from mutating — or leaking uncommitted state into — the directory the
orchestrator is itself operating from.

**Added in:** PR #1037 (consolidates issue #858)
**Supersedes:** PR #1036 (closed as superseded)
**Affects:** `amplifier-bundle/recipes/workflow-worktree.yaml` (only).

`consensus-issue-worktree.yaml` runs a structurally similar existing-branch
path but is **not** modified by this change; a future parity port is discussed
under [Cross-Recipe Parity](#cross-recipe-parity-consensus-issue-worktreeyaml).

---

## Contents

- [Quick Start](#quick-start)
- [Why the Refusal Exists](#why-the-refusal-exists)
- [The Three Refusal Gates](#the-three-refusal-gates)
- [Ordering Invariant](#ordering-invariant)
- [JSON Output Contract](#json-output-contract)
- [Preserved Behavior (#342 / #642)](#preserved-behavior-342--642)
- [Cross-Recipe Parity (consensus-issue-worktree.yaml)](#cross-recipe-parity-consensus-issue-worktreeyaml)
- [Diagnostics & Remediation](#diagnostics--remediation)
- [Security Properties](#security-properties)
- [Verification & Tests](#verification--tests)
- [Related Documentation](#related-documentation)

---

## Quick Start

No configuration is required. The refusal gates are always active on the
existing-branch path (`existing_branch` / `pr_number`). Legitimate reuse of a
pre-registered task worktree, and creation of a fresh worktree for a new task,
continue to work unchanged.

```bash
# Existing-branch workflow — step-04 refuses to reuse the caller's checkout,
# but still reuses/attaches a dedicated task worktree.
amplihack recipe run workflow-worktree \
  -c existing_branch="feat/issue-858-consolidate" \
  -c repo_path="$(pwd)"
```

When the step detects that the only candidate worktree for the target branch is
the **caller's own checkout**, it stops with exit code `1`, emits **no stdout**,
and prints an issue-#858 diagnostic to stderr.

---

## Why the Refusal Exists

Before issue #858, when the caller invoked the workflow while already sitting on
the target branch, `step-04-setup-worktree` would resolve `WORKTREE_PATH` to
`REPO_PATH` (the caller's checkout) and hand it to downstream steps. Downstream
agents then committed, reset, and pushed inside the caller's working directory.

This produced three failure modes:

1. **State leakage** — uncommitted caller changes (staged, unstaged, and
   untracked files) were swept into the workflow's commits.
2. **Concurrent mutation** — the orchestrator and the workflow raced on the
   same working tree, corrupting index state.
3. **Stale-registration confusion** — a pruned-but-registered worktree pointed
   at a path that no longer existed, and the step silently proceeded.

The fix is to **refuse** rather than reuse the caller's checkout. Refusal is
fail-closed: the step exits non-zero before emitting any JSON, so no downstream
step ever receives a worktree path that aliases the caller.

---

## The Three Refusal Gates

The gates run on the existing-branch path, immediately after `BRANCH_NAME`
validation (`git check-ref-format --branch`) and the best-effort fetch, and
**before** any worktree reuse, attach, or JSON emission.

| Gate | Trigger | Action |
| ---- | ------- | ------ |
| **A — Stale registration** | The branch's registered worktree path is inaccessible (pruned out-of-band, deleted directory still registered). | `exit 1`; advise `git worktree prune`. |
| **B — Caller reuse by canonical path** | The registered worktree's canonical path equals the caller's canonical `REPO_PATH`. | `exit 1`; refuse caller-checkout reuse. |
| **C — Caller reuse by HEAD** | The caller's checked-out `HEAD` branch equals the target `BRANCH_NAME` (caller is *on* the branch). | `exit 1`; refuse caller-checkout reuse. |

> **Behavior change:** Prior to #858, Gate C's condition
> (`HEAD_BRANCH == BRANCH_NAME`) was a **reuse** path that set
> `WORKTREE_PATH="$REPO_PATH"` and returned success. It is now a **refusal**.

### Gate B canonicalization

Gate B canonicalizes **both** the registered worktree path and `REPO_PATH`
(via `realpath -e`) before comparing. This defeats bypasses via symlinks, `..`
segments, and trailing slashes. If canonicalization of either side fails
(empty result), the paths are treated as **not equal** — the gate does not
fall through to reuse, but the remaining gates and downstream attach logic
still apply. The canonical-match case always refuses.

---

## Ordering Invariant

All three gates execute **before** the `printf` that emits the JSON contract.
No gate emits partial JSON. This guarantees the fail-closed property: a refusal
never discloses a `worktree_path` on stdout.

```
BRANCH_NAME validated (check-ref-format)
        │
        ▼
best-effort fetch origin/<branch>
        │
        ▼
resolve EXISTING_WT (git worktree list --porcelain)
        │
        ▼
┌───────────────────────────────────────────────┐
│ Gate A  stale registration     → exit 1        │
│ Gate B  caller path match       → exit 1        │
│ Gate C  caller HEAD match        → exit 1        │
└───────────────────────────────────────────────┘
        │  (no caller-reuse condition matched)
        ▼
#342 reuse / #642 stale-dir handling / attach
        │
        ▼
printf JSON contract  →  stdout  →  exit 0
```

---

## JSON Output Contract

The stdout contract is **unchanged**. On the success path the step prints
exactly one JSON object:

```json
{"worktree_path": "…", "branch_name": "…", "base_ref": "", "base_branch": "", "created": false}
```

| Field | Type | Notes |
| ----- | ---- | ----- |
| `worktree_path` | string | Absolute path to the task worktree (never the caller's checkout). JSON-escaped. |
| `branch_name` | string | Branch created or reused. JSON-escaped. |
| `base_ref` | string | Empty on the existing-branch path. |
| `base_branch` | string | Empty on the existing-branch path. |
| `created` | bool | `false` when reusing an existing branch. |

On any refusal gate the step emits **zero bytes** on stdout and exits `1`.

---

## Preserved Behavior (#342 / #642)

The refusal gates nest **ahead of**, and do not disturb, the pre-existing
existing-branch logic:

- **#342 — existing-branch reuse:** a dedicated, previously-registered task
  worktree for the branch (one that is *not* the caller's checkout) is still
  reused, emitting `created=false`.
- **#642 — stale-directory handling:** a leftover worktree directory at
  `REPO_PATH/worktrees/<branch>` is reused if its `HEAD` matches, or removed
  and re-attached if it does not.
- The best-effort push/upstream steps on the existing-branch path (recipe
  lines 177–178) are unchanged.

> **Scope note:** `AMPLIHACK_RUNTIME_ROOT` provisioning lives on the
> **new-branch** path (recipe line ~334), which the existing-branch path never
> reaches — it `exit 0`s at line 188 after emitting JSON. The refusal gates
> therefore do not interact with runtime-root setup at all.

The gates only refuse the specific case where the **caller's own checkout** is
the reuse candidate. Legitimate task worktrees are never over-refused.

---

## Diagnostics & Remediation

Refusal diagnostics are scoped to issue #858, the reason, and a remediation
hint. They never print tokens, home paths, or environment dumps.

| Gate | Example stderr (illustrative) | Remediation |
| ---- | ----------------------------- | ----------- |
| A | `ERROR (issue #858): registered worktree for 'feat/x' is inaccessible; run 'git worktree prune'.` | `git worktree prune`, then re-run. |
| B | `ERROR (issue #858): refusing to reuse caller checkout as task worktree (path match).` | Invoke the workflow from outside the target branch's checkout. |
| C | `ERROR (issue #858): refusing to reuse caller checkout as task worktree (HEAD on '<branch>').` | Check out a different branch (e.g. the default branch) before invoking. |

---

## Cross-Recipe Parity (consensus-issue-worktree.yaml)

> **Status: not yet ported.** This change lands the refusal in
> `workflow-worktree.yaml` only. `consensus-issue-worktree.yaml` is left
> unchanged because no parity guard currently asserts recipe equivalence, and
> the explicit scope of issue #858's consolidation is the default-workflow
> recipe. The mapping below is retained as a design note for a future port.

`consensus-issue-worktree.yaml` runs the same existing-branch resolution and
would share the fail-closed refusal contract if ported. The mapping is **not**
line-identical because the consensus recipe roots worktrees under
`$WORKTREE_DIR/$BRANCH_NAME` (not `$REPO_PATH/worktrees/<branch>`):

| workflow-worktree gate | consensus-issue-worktree equivalent |
| ---------------------- | ----------------------------------- |
| Gate A (stale registration) | Applied to `EXISTING_WT` resolved at consensus lines 138–141. |
| Gate B (caller path match) | Compare canonical `EXISTING_WT` against canonical `$(pwd)` — the consensus recipe uses `$(pwd)` as the caller checkout. |
| Gate C (caller HEAD match) | Converts the caller-reuse branch at consensus lines 144–145 — `WORKTREE_PATH="$(pwd)"` — into an `exit 1` refusal. This is the highest-impact parity change: today that branch **reuses** the caller checkout. |

The `{branch_name, worktree_path, commands_executed, setup_complete}` JSON that
the consensus recipe emits at lines 154–155 must not be printed on any refusal
path — same zero-stdout, `exit 1` invariant as workflow-worktree.

> **Do not regress the bootstrap base_ref.** The `BASE_REF="HEAD"` assignments
> at consensus lines 171, 175, and 204 are **intentional bootstrap fallbacks**
> for repos with no `origin` remote or no `origin/main` branch. The normal
> (remote-present) path already resolves `BASE_REF="origin/main"` at lines 168
> and 218. The refusal port must leave these bootstrap fallbacks intact — do
> not "fix" them to a remote ref, or bootstrap/offline runs will break.

---

## Security Properties

| ID | Property |
| -- | -------- |
| SR-6 | **Core #858 property.** Refusals emit zero stdout and `exit 1` before JSON emission — no partial disclosure, fail-closed. |
| SR-2 | Gates run strictly downstream of `git check-ref-format --branch`; no pre-validation branch-name use. |
| SR-3 | All shell expansions are quoted (`[ "$X" = "$Y" ]`, `git -C "$WORKTREE_PATH"`) to prevent word-splitting and glob injection on worktree paths. |
| SR-4 | Gate B canonicalizes both sides (`realpath -e`); a failed canonicalization is treated as not-equal to block symlink / `..` bypass. |
| SR-7 | `json_escape` is applied to `worktree_path` and `branch_name` to prevent JSON injection. |
| RK-S4 | **Refuse, don't remediate.** No destructive `git worktree remove` / `rm` runs inside the refusal gates (avoids TOCTOU weaponization); Gate A only *advises* `git worktree prune`. |

---

## Verification & Tests

Coverage lives in a dedicated, network-free integration test that drives
`workflow-worktree.yaml` against bare local-origin fixtures. Integration tests
live at the repository root under `tests/integration/` and are registered as
`[[test]]` targets in `bins/amplihack/Cargo.toml` via a `../../tests/integration/`
relative path (matching the existing `existing_branch_context` target):

- Test file: `tests/integration/workflow_worktree_caller_refusal_test.rs`
- `[[test]]` target: `name = "workflow_worktree_caller_refusal"`,
  `path = "../../tests/integration/workflow_worktree_caller_refusal_test.rs"`.

It asserts, for each gate:

- exit code `1`,
- **empty** stdout (no JSON leaked),
- stderr contains `#858`,
- **no** worktree is created or mutated as a side effect.

It also asserts the non-refusal cases still work: dirty caller-branch commits
and untracked files do **not** leak into a freshly created task worktree, and a
legitimate existing task worktree is reused fail-open.

Existing companion suite that must continue to pass:

- `existing_branch_context` (issue #342 reuse) —
  `tests/integration/existing_branch_context_test.rs`.

### Running the tests

Isolate the build target directory (per `docs/artifact-guard.md`) so test
builds do not pollute the repo tree:

```bash
export CARGO_TARGET_DIR=.amplihack/cache/cargo-target
cargo test -p amplihack --locked \
  --test workflow_worktree_caller_refusal \
  --test existing_branch_context
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```

---

## Related Documentation

- [step-04 Re-Prune After Orphan Cleanup](recipe-step-04-worktree-reattach-prune.md) — worktree reattach/prune behavior.
- [worktree_setup Context Propagation](worktree-setup-propagation.md) — how the JSON contract propagates downstream.
- [step-03 Idempotency Guards](recipe-step-03-idempotency.md) — issue-creation deduplication.
- [Troubleshoot Worktree](../howto/troubleshoot-worktree.md) — general worktree debugging.
- [Worktree Support](../concepts/worktree-support.md) — feature overview.
