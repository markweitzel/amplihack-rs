# Bug Fix #1110 — realign the stale gadugi `merge-validations` characterization scenario

> **Issue:** [#1110](https://github.com/rysweet/amplihack-rs/issues/1110)

---

## Summary

The gadugi characterization scenario `issue-820-merge-validations-mixed-output`
had drifted away from the shipped `merge-validations` behavior and was failing
against the current contract. This fix **re-characterizes the scenario driver**
so it once again asserts the behavior the recipe actually ships today.

This is a **test-drift fix only**. No shipped/production code changed. The single
edited file is the self-asserting scenario driver
`tests/gadugi/run-merge-validations-scenario.sh`. The shipped logic
(`amplifier-bundle/recipes/quality-audit-cycle.yaml`), the byte-exact harness
(`tests/gadugi/run-merge-validations.sh`), and the scenario YAML
(`tests/gadugi/scenarios/issue-820-merge-validations-mixed-output.yaml`) are
unchanged.

## The two drifts

The driver asserted two behaviors that the shipped `merge-validations` step no
longer exhibits:

| # | Drift | Old (stale) driver expectation | Current shipped behavior |
| --- | --- | --- | --- |
| 1 | Per-unparseable-validator diagnostic wording | grep for an earlier diagnostic substring no longer emitted by the step | grep for `output unparseable; counting zero votes from it` |
| 2 | All validators unparseable | exit `0`, `confirmed_count == 0` (graceful degrade) | **FATAL**: exit `1`, no JSON on stdout, `FATAL: all validators produced unparseable output; cannot merge any verdicts` on stderr |

### Drift 1 — diagnostic wording

When a validator's output cannot be parsed, the step emits a single-line
`WARNING` to stderr:

```text
[merge-validations] WARNING: validator <label> output unparseable; counting zero votes from it. Raw output preserved at: <path>
```

The old driver grepped for an earlier diagnostic substring that the shipped
`merge-validations` step no longer emits. (The exact retired wording is not
recoverable from the current recipe body — the pre-drift diagnostic message has
been fully replaced, so only the historical fact of the change is documented
here, not the literal old string.) The driver now greps the current, shipped
substring `output unparseable; counting zero votes from it`, which is emitted
verbatim by the recipe.

### Drift 2 — all-unparseable is now FATAL

When **every** validator is unparseable (`parsed_count == 0` and
`unparseable_count >= 1`), the step now **fails closed** rather than silently
"merging" zero verdicts. It:

- prints `[merge-validations] FATAL: all validators produced unparseable output;
  cannot merge any verdicts. Raw outputs preserved at: <paths>` to stderr,
- preserves each validator's raw output as a per-cycle artifact,
- writes **no** JSON to stdout, and
- exits `1`.

This fail-closed gate is the intended hardening documented in
[bugfix #899](./bugfix-899-merge-validations-unparseable-abstention.md); the
scenario is now locked onto it.

## Scenario contract (after this fix)

The driver runs three cases against the real recipe body via
`tests/gadugi/run-merge-validations.sh` and prints `ALL_CASES_PASSED` (exit `0`)
when all hold.

### Case 1 — mixed output (fenced + bare + log-only)

`v1` wraps its verdict in a ```` ```json ```` fence, `v2` is bare JSON, and `v3`
is log-only garbage. Two validators parse, so the merge proceeds.

- exit `0`;
- no `jq` `Bad JSON` leak on stderr (PASS line `no jq 'Bad JSON' on mixed output`
  — asserted verbatim by the scenario YAML);
- `confirmed_count == 1`, first verdict `confirmed`;
- the log-only validator triggers the current unparseable diagnostic
  (`output unparseable; counting zero votes from it`).

### Case 2 — all bare JSON

All three validators emit bare JSON objects. Unchanged: exit `0`,
`confirmed_count == 1` for finding 7.

### Case 3 — all validators unparseable (now FATAL)

Three garbage inputs (`g1`/`g2`/`g3`), passed with `OUTPUT_DIR = out3` and
`cycle = 1`:

- exit `1` (FATAL);
- stderr contains `FATAL: all validators produced unparseable output; cannot
  merge any verdicts`;
- no `jq` `Bad JSON` leak;
- the raw output artifact is preserved at
  `out3/cycle_1/validator_v1_raw.txt`.

The obsolete `confirmed_count == 0` assertion is removed — the FATAL path writes
no JSON to stdout, so a vote-count check is meaningless.

## Validation

```bash
# Authoritative check: the driver self-asserts all three cases.
bash tests/gadugi/run-merge-validations-scenario.sh; echo "rc=$?"
# → prints ALL_CASES_PASSED and rc=0

# Syntax check.
bash -n tests/gadugi/run-merge-validations-scenario.sh

# Optional, if the gadugi-test runner is installed.
gadugi-test run -s issue-820-merge-validations-mixed-output
# → 0 failures
```

If the `gadugi-test` runner is not installed, the direct `bash` run is the
authoritative check.

## Scope

Test re-characterization only. The scenario driver was updated to describe the
current shipped `merge-validations` contract; the voting math, verdict
extraction, validator classification, the `jq` merge, diagnostics, and the
fail-closed gate are all unchanged shipped behavior. The scenario YAML's
`assertions` (`ALL_CASES_PASSED`, exit `0`, `no jq 'Bad JSON' on mixed output`)
remain valid and were not modified.
