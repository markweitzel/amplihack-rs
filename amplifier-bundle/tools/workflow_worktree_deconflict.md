# Worktree Branch Deconfliction

**Non-destructive helper that keeps two concurrently-running recipes from
fighting over the same branch name.**

`workflow_worktree_deconflict.sh` is a self-contained shell brick used by the
worktree/branch setup steps of `workflow-worktree.yaml` (step
`step-04-setup-worktree`) and mirrored as guidance in
`consensus-issue-worktree.yaml` (step `step3-setup-worktree`). It inspects the
live `git worktree` topology and, when the branch a recipe wants is already
checked out by a **different** recipe's worktree, hands back a fresh, distinct,
provably-free branch name instead of destroying the other recipe's work.

- [Why this exists](#why-this-exists)
- [What it guarantees](#what-it-guarantees)
- [Command-line usage](#command-line-usage)
- [Contract / API](#contract--api)
- [Ownership classification](#ownership-classification)
- [Deconfliction algorithm](#deconfliction-algorithm)
- [Configuration](#configuration)
- [Integration in the recipes](#integration-in-the-recipes)
- [Examples](#examples)
- [Preserved behaviors](#preserved-behaviors)
- [Security model](#security-model)
- [Testing](#testing)

---

## Why this exists

`smart-orchestrator` derives a branch name deterministically from the target
issue number and a slug of the task description
(`feat/issue-<N>-<slug>`). When two orchestrator runs start from the **same
issue number and the same task-description file**, they derive the **identical
branch slug**. Each run computes its own worktree path
(`<repo>/worktrees/<branch>`).

Previously, the setup step's idempotency state machine assumed that a branch it
found was one of its *own* prior runs. In the "State-2" path (branch exists,
worktree missing at *this* recipe's path, and the branch has commits ahead of
base) it ran:

```bash
git branch -D "${BRANCH_NAME}"
```

If that branch was, at that very moment, checked out by a **foreign** worktree
belonging to a concurrently-running recipe at a *different* path, git refused:

```
error: cannot delete branch 'feat/issue-200-fix-bug' used by worktree at
'/repo/worktrees/feat/issue-200-fix-bug'
```

...and the whole recipe aborted. The existing worktree-existence probe only
matched *this* recipe's exact path via `grep -Fx`, so a foreign worktree owning
the branch elsewhere was invisible to the state machine.

A foreign-owned branch breaks the State-2 path at **two** points, both resolved
by renaming this run's branch upstream of the state machine:

- when the branch is **ahead of base**, `git branch -D` fails as shown above; and
- even when it is **clean**, the fallback `git worktree add "${WORKTREE_PATH}"
  "${BRANCH_NAME}"` (attach without `-b`) fails with `fatal: '<branch>' is
  already used by worktree at '<foreign path>'` — git will not check out one
  branch in two worktrees.

This helper detects that foreign ownership **before** any destructive or reuse
decision is made, and resolves it by *renaming this run's branch* rather than
by destroying the other run's branch or worktree.

## What it guarantees

- **Never destructive to foreign work.** The helper contains **zero**
  destructive commands — no `git branch -D`, no `git worktree remove`, no
  `rm -rf`, no `reset --hard`, no `checkout -f`. It only reads git state and
  prints a branch name.
- **Foreign-owned branches are left untouched.** When the desired branch is
  checked out by any worktree at a path other than the intended one, the
  original branch and its worktree are neither deleted nor reused.
- **The chosen name is provably free.** Every returned name passes
  `git check-ref-format --branch`, has **no** local ref (`refs/heads/*`), **no**
  matching remote ref (`refs/remotes/origin/*`), and is **not** checked out by
  **any** worktree.
- **Bounded and loud.** At most `5` candidate suffixes are tried (a hard ceiling
  that `AMPLIHACK_DECONFLICT_MAX_RETRIES` may only *lower*, never raise); on
  exhaustion the helper exits non-zero with a diagnostic rather than looping or
  silently degrading.
- **Same-path resume is preserved.** If the branch is checked out at *this*
  recipe's own intended path, that is a genuine resume — the helper returns the
  branch unchanged so the recipe's State-1 reuse path (`created=false`) still
  runs.

## Command-line usage

```bash
workflow_worktree_deconflict.sh resolve <candidate_branch> <intended_worktree_path> [repo_path]
```

| Argument                  | Meaning                                                              |
| ------------------------- | ------------------------------------------------------------------- |
| `candidate_branch`        | The branch name the recipe derived (e.g. `feat/issue-200-fix-bug`). |
| `intended_worktree_path`  | The path this recipe intends to attach the worktree to.             |
| `repo_path` (optional)    | Repository root. Defaults to the current working directory.         |

`stdout` carries **only** the resolved branch name (one line, no
decoration) so it is safe to capture with command substitution. All human
diagnostics go to `stderr`.

```bash
NEW_BRANCH="$(bash workflow_worktree_deconflict.sh resolve \
  "feat/issue-200-fix-bug" \
  "/repo/worktrees/feat/issue-200-fix-bug")"
```

### Exit codes

| Code | Meaning                                                                            |
| ---- | ---------------------------------------------------------------------------------- |
| `0`  | A usable branch name was resolved and printed to stdout.                            |
| `1`  | Bad arguments, or all bounded retries (`≤ 5`) were exhausted without a free name.   |

## Contract / API

The helper exposes a single logical operation, `resolve`, with this contract:

**Input:** a candidate branch, the intended worktree path, and (optionally) the
repo root.

**Output (stdout):** exactly one branch name that is guaranteed to satisfy all
freshness invariants at the moment of return. It is either:

1. the **unchanged** candidate — when the branch is absent, or is checked out at
   the intended path (genuine same-path resume), or exists but is **not**
   checked out by any worktree (a normal State-2 branch this recipe may safely
   manage); or
2. a **new** deconflicted name — when the candidate is foreign-owned.

**Side effects:** none. The helper only reads git state.

The caller is responsible for recomputing `WORKTREE_PATH` from the returned
branch and proceeding through its normal create path.

## Ownership classification

The helper parses `git worktree list --porcelain` (the stable, machine-readable
form — never the human `git worktree list` output) and pairs each
`worktree <path>` record with its `branch refs/heads/<name>` record. Both the
live worktree path and the intended path are normalized (symlinks and `..`
resolved, `pwd -P`-style, matching the existing issue #858 caller-checkout
logic) before comparison.

| Situation                                                        | Classification | Action                                  |
| ---------------------------------------------------------------- | -------------- | --------------------------------------- |
| Branch is checked out by no worktree                             | **candidate**  | return unchanged                        |
| Branch is checked out at the **intended** path (after normalize) | **same-path**  | return unchanged (State-1 resume)       |
| Branch is checked out at a **different** normalized path         | **foreign**    | deconflict → return a new distinct name |

## Deconfliction algorithm

When a branch is classified **foreign**, the helper generates a new name with a
bounded, deterministic-enough loop:

1. Build a short unique suffix from `date +%s` and the process id, then sanitize
   it through `tr -cd 'a-z0-9-'` so it can never introduce shell or ref
   metacharacters.
2. Form a candidate: `<candidate_branch>-<suffix>`, then cap the total length at
   `DECONFLICT_MAX_BRANCH_LEN` (80) by truncating only the **base** — the
   uniqueness-bearing suffix is preserved intact and any resulting trailing
   hyphen is stripped.
3. Validate the candidate with `git check-ref-format --branch`.
4. Reject the candidate unless it is **free** on every axis:
   - no `refs/heads/<name>` (local branch),
   - no `refs/remotes/origin/<name>` (remote branch),
   - not checked out by **any** worktree.
5. On the first candidate that passes all checks, print it to stdout and exit
   `0`.
6. Repeat up to the configured bound (default and hard ceiling **5**; see
   [Configuration](#configuration)). If every attempt is exhausted, print a
   diagnostic to stderr and exit `1` (fail loud — never loop unbounded, never
   fall back to a colliding name).

Because every pass re-validates freshness, the astronomically-rare case of two
runs generating the same suffix in the same second is caught on the next
iteration.

## Configuration

The helper is intentionally low-configuration. Its behavior is governed by a
single optional environment variable, plus one internal constant:

| Variable                            | Purpose                                             | Default |
| ----------------------------------- | --------------------------------------------------- | ------- |
| `AMPLIHACK_DECONFLICT_MAX_RETRIES`  | Upper bound on suffix retries. Validated `^[0-9]+$` **and clamped to a hard ceiling of `5`** so the override can only *lower* the bound, never weaken the DoS guarantee. Any invalid value falls back to the default. | `5`     |

The internal `readonly DECONFLICT_MAX_BRANCH_LEN` (80) caps the length of a
renamed branch on the deconfliction path — see the [algorithm](#deconfliction-algorithm),
step 2. It is a constant, not an environment knob.

Resolution of the helper path in the recipe follows the same **best-effort
ladder** modeled on `workflow_worktree_sweep.sh` (issue #829/#840 precedent). As
wired in `workflow-worktree.yaml`, the call site probes, in order,
`$AMPLIHACK_HOME`, `$REPO_PATH`, and the current directory for
`amplifier-bundle/tools/workflow_worktree_deconflict.sh` (see the snippet under
[Integration in the recipes](#integration-in-the-recipes)). A **missing** helper
is a graceful no-op guarded by `-f`, and the recipe proceeds with its legacy
state machine. A helper that **runs but exits non-zero** (retries exhausted /
misconfig) fails loud (`exit 1`) instead of falling back to the conflicting name.

## Integration in the recipes

### `workflow-worktree.yaml` — executable path (primary fix)

The call site sits immediately after `WORKTREE_PATH` is computed
(~L292) and **before** the idempotency state machine's
`BRANCH_EXISTS`/`WORKTREE_EXISTS` probes and the State-2 `git branch -D`
(~L308–334):

```bash
# --- Foreign-worktree deconfliction (concurrency-safety) ---
# Before the idempotency state machine touches this branch, ask the helper
# whether the branch is owned by a FOREIGN worktree at a different path. If so
# it returns a NEW distinct branch name; otherwise it returns the candidate
# unchanged (preserving same-path State-1 resume). A missing helper is a
# graceful no-op (#829/#840 precedent); a helper that runs but FAILS (retries
# exhausted / misconfig) aborts loud rather than reusing the conflicting name.
DECONFLICT_HELPER="${AMPLIHACK_HOME:-${REPO_PATH:-$(pwd)}}/amplifier-bundle/tools/workflow_worktree_deconflict.sh"
[ -f "$DECONFLICT_HELPER" ] || DECONFLICT_HELPER="${REPO_PATH:-$(pwd)}/amplifier-bundle/tools/workflow_worktree_deconflict.sh"
[ -f "$DECONFLICT_HELPER" ] || DECONFLICT_HELPER="$(pwd)/amplifier-bundle/tools/workflow_worktree_deconflict.sh"
if [ -f "$DECONFLICT_HELPER" ]; then
  # Branch on the helper's EXIT CODE and let its stderr surface — never
  # '2>/dev/null || echo "$BRANCH_NAME"', which would silently reuse the
  # conflicting name and re-abort at the State-2 `git branch -D`.
  if ! RESOLVED_BRANCH="$(bash "$DECONFLICT_HELPER" resolve "$BRANCH_NAME" "$WORKTREE_PATH" "$REPO_PATH")"; then
    echo "ERROR: worktree branch deconfliction failed for '${BRANCH_NAME}' ..." >&2
    exit 1
  fi
  if [ -n "$RESOLVED_BRANCH" ] && [ "$RESOLVED_BRANCH" != "$BRANCH_NAME" ]; then
    echo "INFO: branch '${BRANCH_NAME}' is owned by a foreign worktree — deconflicting to '${RESOLVED_BRANCH}'." >&2
    BRANCH_NAME="$RESOLVED_BRANCH"
    WORKTREE_PATH="${REPO_PATH}/worktrees/${BRANCH_NAME}"
  fi
fi
```

Because `BRANCH_NAME` is reassigned before the state machine runs and before the
JSON is printed, the final (possibly deconflicted) name flows automatically into
the step's structured output:

```json
{
  "worktree_path": "/repo/worktrees/feat/issue-200-fix-bug-9f3c1a2b",
  "branch_name": "feat/issue-200-fix-bug-9f3c1a2b",
  "base_ref": "origin/main",
  "base_branch": "main",
  "runtime_root": "/tmp/amplihack-runtime/feat/issue-200-fix-bug-9f3c1a2b",
  "created": true
}
```

Downstream steps (PR creation, runtime-artifact laddering, etc.) consume
`branch_name` from this object, so they target the deconflicted branch with no
further changes.

### `consensus-issue-worktree.yaml` — mirror (guidance)

The consensus setup step is an `agent:` step (`amplihack:worktree-manager`)
whose bash lives inside a natural-language `prompt:` block. The equivalent
deconfliction guidance and helper-invocation snippet are inserted into that
prompt **before** its own `git branch -D` (~L286). This mirror is verified by a
**static parity assertion** (grep-based) rather than a behavioral test, because
the recipe-runner does not execute prompt bash directly.

## Examples

### Two concurrent runs, same slug

```console
$ # Run A already holds feat/issue-200-fix-bug at /repo/worktrees/feat/issue-200-fix-bug
$ bash workflow_worktree_deconflict.sh resolve \
    feat/issue-200-fix-bug /repo-B/worktrees/feat/issue-200-fix-bug /repo-B
INFO: branch 'feat/issue-200-fix-bug' is owned by a foreign worktree at
      /repo/worktrees/feat/issue-200-fix-bug (intended: /repo-B/worktrees/...) — deconflicting.
feat/issue-200-fix-bug-9f3c1a2b
```

Run B proceeds on `feat/issue-200-fix-bug-9f3c1a2b`; Run A is untouched and never
sees `git branch -D`.

### Genuine same-path resume (no rename)

```console
$ # This recipe's own worktree already holds the branch at the intended path
$ bash workflow_worktree_deconflict.sh resolve \
    feat/issue-200-fix-bug /repo/worktrees/feat/issue-200-fix-bug /repo
feat/issue-200-fix-bug
```

Unchanged output → the recipe's State-1 reuse path runs and emits
`created=false`.

### Branch exists but no worktree owns it (normal State-2)

```console
$ bash workflow_worktree_deconflict.sh resolve \
    feat/issue-200-fix-bug /repo/worktrees/feat/issue-200-fix-bug /repo
feat/issue-200-fix-bug
```

Unchanged output → the recipe safely manages its own leftover branch exactly as
before.

## Preserved behaviors

The deconfliction gate is purely additive. Every existing behavior remains
intact:

- **State-1 same-path resume** — reuse with `created=false`.
- **Issue #200 dirty-branch reset** — a dirty/ahead branch owned by *this*
  recipe is still reset to base.
- **Issue #342 `existing_branch` / `PR_NUMBER` targeting** — explicit
  branch/PR targeting is honored.
- **Issue #858 caller-checkout refusal** — refusing to hijack a branch the
  caller has checked out.
- **Issue #3023 empty-slug hardened fallback** — `feat/task-unnamed-<ts>`.
- **The sanitized slug pipeline** — `tr -cd 'a-z0-9-'`, `git check-ref-format`,
  and the hardcoded `feat/` fallback prefix.

## Security model

- **Command injection:** all expansions are quoted; there is no `eval` or
  backtick re-execution; porcelain output is parsed as data via `awk`; and every
  candidate must clear `git check-ref-format --branch` before use.
- **Non-destructive invariant (hard):** the helper contains no branch/worktree
  deletion, no `rm`, no `reset --hard`, and no `checkout -f`. This is enforced
  by a static test assertion.
- **Path spoofing / traversal:** both the intended and live worktree paths are
  `pwd -P`-normalized (symlinks and `..` resolved) before equality comparison,
  so a symlinked path cannot masquerade as same-path.
- **Denial of service:** retries are bounded (`≤ 5`) and the helper fails loud
  on exhaustion — it never loops unbounded.
- **No network / auth / secret surface:** read-only local git introspection
  only; no fetch/push/pull and no credential handling.
- **Data minimization:** stdout carries the resolved branch name only;
  worktree-list detail stays on stderr.
- The helper is **shellcheck-gated in CI** to catch unquoted-expansion and
  injection classes automatically.

## Testing

Behavioral and static coverage lives in
`amplifier-bundle/recipes/tests/test-foreign-worktree-deconflict.sh`, wired as a
named step in `.github/workflows/ci.yml` alongside the other per-recipe tests
(and a shellcheck step for the helper). It follows the issue #840 harness
template (`set -euo pipefail`, `extract_step()` awk, `mktemp -d` fixtures,
`trap cleanup EXIT`, pass/fail counters).

It asserts:

1. **Foreign ownership → new branch.** With a foreign worktree holding
   `BRANCH_NAME` at a different path, the step selects a **new**, valid branch,
   succeeds, and **never** calls `git branch -D` on the foreign branch.
2. **Same-path resume still reuses.** A genuine same-path worktree yields
   `created=false` and no new branch.
3. **Consensus parity.** The `consensus-issue-worktree.yaml` prompt carries the
   equivalent deconfliction guidance.
4. **Static invariants.** Non-destructive command absence, porcelain +
   `check-ref-format` usage, bounded-retry, call-site ordering before the state
   machine, and the `< 400`-line budget on `workflow-worktree.yaml`.
