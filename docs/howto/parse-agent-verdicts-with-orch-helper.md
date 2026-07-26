# Parse Agent Verdicts with the orch Helper

How to turn raw verifier-agent output into a single canonical verdict token
using the `amplihack orch helper` pipeline — so your recipe bash steps can gate
deterministically without `jq` or brittle string matching.

For the full contract and vocabulary, see
[Structured Verdict Parsing](../reference/structured-verdict-parsing.md).

## Before you start

- `amplihack` is installed (run `amplihack --version` to confirm).
- You have raw verifier output on stdin or in a variable (`$VERDICT_RAW`).
- You know the JSON field that carries the verdict (typically `verdict`).

## The canonical pipeline

Chain three `orch helper` subcommands. Each reads stdin and writes stdout, so
they compose with plain pipes:

```sh
VERDICT=$(printf '%s' "$VERDICT_RAW" \
  | amplihack orch helper extract-json \
  | amplihack orch helper extract-field --field verdict \
  | amplihack orch helper normalise-verdict)
```

1. **`extract-json`** — recovers the first complete JSON object from mixed
   agent output (```json fences, untagged fences, or a balanced-brace scan).
   Prints `{}` if nothing parseable is found.
2. **`extract-field --field verdict`** — reads the JSON object and prints the
   `verdict` value as a bare string (no `jq` dependency). Prints the
   `--default` value (empty by default) when the field is absent.
3. **`normalise-verdict`** — collapses synonyms into one canonical token:
   `WORK_VERIFIED`, `HOLLOW_SUCCESS`, or `INSUFFICIENT_EVIDENCE`.

`$VERDICT` now holds exactly one of the three canonical tokens.

## Gate on the verdict with `==` equality

Compare the canonical token with **exact equality**, never substring matching:

```sh
case "$VERDICT" in
  WORK_VERIFIED)
    echo "INFO: verifier approved." >&2
    ;;
  HOLLOW_SUCCESS)
    echo "ERROR: step claimed done but produced no artifacts." >&2
    exit 1
    ;;
  INSUFFICIENT_EVIDENCE)
    echo "WARN: unclear verdict — continuing with a loud warning." >&2
    ;;
esac
```

> **Do not** write `if [[ "$VERDICT" == *VERIFIED* ]]`. A substring test matches
> `UNVERIFIED` and `NOT_APPROVED` as approvals — it fails open. This is why
> `normalise-verdict` uses exact-token equality (invariant R2); your gate must
> too. See
> [Equality, not containment](../reference/structured-verdict-parsing.md#equality-not-containment-r2).

## Handling synonyms

You do not need to enumerate synonyms yourself — `normalise-verdict` maps them:

```sh
$ printf '{"verdict":"APPROVED"}' \
    | amplihack orch helper extract-field --field verdict \
    | amplihack orch helper normalise-verdict
WORK_VERIFIED

$ printf '{"verdict":"INCONCLUSIVE"}' \
    | amplihack orch helper extract-field --field verdict \
    | amplihack orch helper normalise-verdict
INSUFFICIENT_EVIDENCE

# Fail-open guard: UNVERIFIED never becomes WORK_VERIFIED
$ printf '{"verdict":"UNVERIFIED"}' \
    | amplihack orch helper extract-field --field verdict \
    | amplihack orch helper normalise-verdict
INSUFFICIENT_EVIDENCE
```

## Empty or missing verdicts

If the field is missing, empty, or the whole payload is unparseable, the
pipeline degrades safely to `INSUFFICIENT_EVIDENCE` (non-fatal):

```sh
$ printf 'no json at all' \
    | amplihack orch helper extract-json \
    | amplihack orch helper extract-field --field verdict \
    | amplihack orch helper normalise-verdict
INSUFFICIENT_EVIDENCE
```

`normalise-verdict` never emits `NEEDS_ATTENTION`. If you need the
`NEEDS_ATTENTION` marker, it comes from the checkpoint marker path, not this
pipeline — see
[Workflow terminal state](../reference/workflow-terminal-state.md).

## Fatal prose verdicts

The JSON pipeline is not the only signal. An **independent** recipe gate treats
an explicit failure verdict as **fatal** — whether it appears as the prose line
`VERDICT: FAILED` **or** as a JSON verdict containing `FAILED`/`NOT_VERIFIED`.
That gate (the failure-token grep in
[`workflow-pr-review.yaml` line 32](../../amplifier-bundle/recipes/workflow-pr-review.yaml),
inside `step-17a-testing-evidence-gate`) fires regardless of whether this
pipeline ran, and is never softened to `INSUFFICIENT_EVIDENCE`. Keep any
existing `VERDICT: FAILED` check alongside the pipeline; the two paths are
complementary. See
[A2 in the reference](../reference/structured-verdict-parsing.md#a2-prose-verdict-failed-remains-a-fatal-token-962).

## Machine-readable output (opt-in)

For tooling that wants structured output, add the **opt-in** `--json` flag to
`normalise-verdict`. It is additive — the default remains byte-exact text:

```sh
$ printf '{"verdict":"APPROVED"}' \
    | amplihack orch helper extract-field --field verdict \
    | amplihack orch helper normalise-verdict --json
{"verdict":"WORK_VERIFIED","raw":"APPROVED","matched":true}
```

> **Security: the `raw` field is untrusted.** The `raw` value echoes verbatim
> LLM output. `normalise-verdict` JSON-escapes it, but downstream tooling must
> treat it as untrusted: parse it with a real JSON parser, never `eval` it, and
> never interpolate it unquoted into a shell command. Gate only on the
> `verdict` field (one of the three canonical tokens), not on `raw`. In text
> mode the helper emits only the canonical token, so raw input never reaches
> your terminal.

## Related

- [Structured Verdict Parsing](../reference/structured-verdict-parsing.md) — the normative contract and vocabulary.
- [`amplihack orch run` reference](../reference/orch-run-command.md) — the orch helper surface.
- [Recipe Executor Environment](../reference/recipe-executor-environment.md) — env contract for recipe bash steps.
- [Run a Recipe End-to-End](../howto/run-a-recipe.md) — running recipes that consume verdicts.
