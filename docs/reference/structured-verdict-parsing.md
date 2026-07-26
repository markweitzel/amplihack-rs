# Structured Verdict Parsing

## Overview

Amplihack recipes gate on a small, fixed vocabulary of **verdicts** emitted by
verifier agents (for example `step-08c-work-verifier` in
[`amplifier-bundle/recipes/workflow-tdd.yaml`](../../amplifier-bundle/recipes/workflow-tdd.yaml)).
LLM verifiers rarely emit the canonical token verbatim — they produce synonyms
like `APPROVED`, `SUCCESS`, or `INCONCLUSIVE`. The
`amplihack orch helper normalise-verdict` helper collapses those synonyms into
exactly three canonical tokens so recipe bash steps can gate deterministically:

| Canonical verdict       | Terminal meaning                                              |
| ----------------------- | ------------------------------------------------------------ |
| `WORK_VERIFIED`         | Verifier approved — real artifacts address the task.         |
| `HOLLOW_SUCCESS`        | Step claimed done but no concrete artifacts were found. Fatal. |
| `INSUFFICIENT_EVIDENCE` | Verdict was unclear, absent, or unparseable. Non-fatal; warn and continue. |

This reference specifies the parsing contract. For the task-oriented pipeline,
see
[Parse Agent Verdicts with the orch Helper](../howto/parse-agent-verdicts-with-orch-helper.md).

## Equality, not containment (R2)

> **`normalise-verdict` uses case-insensitive EXACT-TOKEN equality, never
> substring matching.**

This is design invariant **R2 (Equality not containment)** and it is
authoritative. Verdict labels contain negation-adjacent collisions, so
`str::contains` (bash `[[ "$V" == *VERIFIED* ]]` or Python `in`) **fails open**:

| Raw token      | Substring match (WRONG) | Exact-token match (CORRECT) |
| -------------- | ----------------------- | --------------------------- |
| `UNVERIFIED`   | `WORK_VERIFIED` ❌       | `INSUFFICIENT_EVIDENCE` ✅   |
| `NOT_APPROVED` | `WORK_VERIFIED` ❌       | `INSUFFICIENT_EVIDENCE` ✅   |
| `NOT_ACHIEVED` | `WORK_VERIFIED` ❌       | `INSUFFICIENT_EVIDENCE` ✅   |

`UNVERIFIED` contains the substring `VERIFIED`; a containment check would
approve a step the verifier explicitly rejected. Exact-token equality is the
only safe rule.

> **Do not "mirror `normalise_type`."** The step task-type helper
> ([`normalise_type`](../reference/orch-run-command.md)) uses substring
> matching safely, because task-type keywords (`invest`, `research`, `command`)
> have no negation-adjacent collisions. Verdict labels do. R2 supersedes any
> earlier requirement wording that said "substring" or "mirror
> `normalise_type`."

## Canonical mapping

The mapping is grounded in the bash `case` synonym gate in
[`workflow-tdd.yaml` lines 265–268](../../amplifier-bundle/recipes/workflow-tdd.yaml).
Bash `case` globs without wildcards (`VERIFIED|SUCCESS|APPROVED|PASS|PASSED)`)
are **exact-token alternations**, confirming equality semantics:

| Canonical verdict       | Exact input tokens (case-insensitive)                         |
| ----------------------- | ------------------------------------------------------------- |
| `WORK_VERIFIED`         | `VERIFIED`, `SUCCESS`, `APPROVED`, `PASS`, `PASSED`           |
| `HOLLOW_SUCCESS`        | `FAILED`, `NO_WORK`, `EMPTY`, `NO_ARTIFACTS`                  |
| `INSUFFICIENT_EVIDENCE` | `INCONCLUSIVE`, `UNKNOWN`, `UNCLEAR`, `PARTIAL`               |

### Additive exact tokens

Beyond the YAML gate, `normalise-verdict` also accepts, as **additive whole
tokens** (still exact equality — never substrings):

- The canonical tokens themselves (`WORK_VERIFIED`, `HOLLOW_SUCCESS`,
  `INSUFFICIENT_EVIDENCE`) — idempotent re-normalisation.
- `NEEDS_ATTENTION` → `INSUFFICIENT_EVIDENCE`. This maps a common verifier
  synonym to the non-fatal bucket; it never produces a `NEEDS_ATTENTION`
  *output* (see [A3](#a3-no-needs_attention-output)).

Additions are safe precisely because they are matched by equality: adding
`NEEDS_ATTENTION` cannot cause `NEEDS` inside some other label to match.

### Empty and unknown inputs

- **Empty / whitespace-only input** → `INSUFFICIENT_EVIDENCE`.
- **Unknown token** → `INSUFFICIENT_EVIDENCE` (fail-safe). This mirrors the
  `*)` arm of the recipe gate: a novel LLM verdict string must never hard-fail
  a recipe that already produced real artifacts (issue #624).

## Acceptance contracts

### A2: prose `VERDICT: FAILED` remains a fatal token (#962)

Verdict parsing has two independent paths. Separately from `normalise-verdict`,
an **independent** recipe gate treats an explicit failure verdict as **fatal** —
whether it appears as the prose line `VERDICT: FAILED` **or** as a JSON verdict
containing `FAILED`/`NOT_VERIFIED` — and never softens it to
`INSUFFICIENT_EVIDENCE`. `normalise-verdict` operates only on the extracted
verdict value; this fatal-token gate is enforced by the recipe and is retained —
see the failure-token grep in
[`workflow-pr-review.yaml` line 32](../../amplifier-bundle/recipes/workflow-pr-review.yaml)
(`grep -qiE 'VERDICT:[[:space:]]*FAILED|"verdict"[[:space:]]*:[[:space:]]*"[^"]*(FAILED|NOT_VERIFIED)'`),
inside `step-17a-testing-evidence-gate`. See
[Doc-review non-fatal checkpoint](../reference/doc-review-non-fatal-checkpoint.md)
for the complementary non-fatal path.

### A3: no `NEEDS_ATTENTION` output

`normalise-verdict` emits **only** `WORK_VERIFIED`, `HOLLOW_SUCCESS`, or
`INSUFFICIENT_EVIDENCE`. It never emits `NEEDS_ATTENTION`. The `NEEDS_ATTENTION`
marker (issue #834) originates in the **checkpoint marker path**, not in verdict
normalisation. Empty input maps to `INSUFFICIENT_EVIDENCE`, not
`NEEDS_ATTENTION`. See
[Workflow terminal state](../reference/workflow-terminal-state.md) for how the
checkpoint marker path surfaces `NEEDS_ATTENTION`.

## Output format

### Default: byte-exact text

By default the helper prints exactly one canonical token followed by a single
newline — nothing else. This byte-exact contract lets recipe bash steps compare
with `[ "$V" = "WORK_VERIFIED" ]` or an exact `case`:

```sh
$ printf 'APPROVED' | amplihack orch helper normalise-verdict
WORK_VERIFIED
$ printf 'UNVERIFIED' | amplihack orch helper normalise-verdict
INSUFFICIENT_EVIDENCE
$ printf '' | amplihack orch helper normalise-verdict
INSUFFICIENT_EVIDENCE
```

### D2: `--json` is additive and opt-in

The `--json` flag is **opt-in and additive** — it does not change the default
text output. When passed, the helper prints a single JSON object with the
canonical verdict, the raw input, and whether an exact token matched:

```sh
$ printf 'APPROVED' | amplihack orch helper normalise-verdict --json
{"verdict":"WORK_VERIFIED","raw":"APPROVED","matched":true}
$ printf 'UNVERIFIED' | amplihack orch helper normalise-verdict --json
{"verdict":"INSUFFICIENT_EVIDENCE","raw":"UNVERIFIED","matched":false}
```

Recipes that do not pass `--json` continue to receive the byte-exact token.

## Trust model

`normalise-verdict` has **zero ambient authority**: it reads only stdin and
writes only stdout. It performs no environment, file, or network reads, which
eliminates path-traversal, SSRF, and injection by construction. Its output is
therefore a pure function of its input.

Two consequences for callers:

- **Fail-safe, never fail-open.** Empty, unknown, or unparseable input maps to
  `INSUFFICIENT_EVIDENCE` — never to a forged `WORK_VERIFIED`. Combined with R2
  exact-token equality, a rejected or malformed verdict can never be promoted to
  an approval.
- **`--json` `raw` is untrusted.** The `raw` field echoes verbatim LLM output.
  It is JSON-escaped by the helper, but downstream consumers must never `eval`
  it or interpolate it unquoted into a shell command. Gate on the `verdict`
  field only; treat `raw` as opaque, untrusted text.

## Consuming the verdict (C: use `==` equality)

Downstream consumers **must** compare the canonical token with equality, never
containment:

```sh
# CORRECT — exact equality
case "$VERDICT" in
  WORK_VERIFIED)         : ;;   # approved
  HOLLOW_SUCCESS)        exit 1 ;;
  INSUFFICIENT_EVIDENCE) echo "WARN: continuing" >&2 ;;
esac

# WRONG — substring match fails open on UNVERIFIED / NOT_APPROVED
if [[ "$VERDICT" == *VERIFIED* ]]; then :; fi   # do NOT do this
```

The canonical tokens are distinct under equality, so an exact `case` (or
`[ "$V" = "..." ]`) is unambiguous. A substring check reintroduces the
fail-open bug that R2 exists to prevent.

## Related

- [Parse Agent Verdicts with the orch Helper](../howto/parse-agent-verdicts-with-orch-helper.md) — the task-oriented pipeline.
- [`amplihack orch run` reference](../reference/orch-run-command.md) — the orch helper surface.
- [Recipe Executor Environment](../reference/recipe-executor-environment.md) — subprocess env contract for recipe bash steps.
- [Run a Recipe End-to-End](../howto/run-a-recipe.md) — running the recipes that consume verdicts.
