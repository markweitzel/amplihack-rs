#!/usr/bin/env bash
# Contract guard for the issue-820 gadugi merge-validations scenario (issue #1110).
#
# Issue #1110 was a *characterization drift*: the shipped merge-validations step
# was hardened, but the gadugi scenario driver still asserted the old behavior,
# so the scenario failed deterministically. The driver was retconned to the
# current contract; this guard exists to stop that drift recurring in EITHER
# direction.
#
# Two layers:
#   PART A — pins the SHIPPED run-merge-validations.sh contract by RUNNING it.
#            This is the source of truth. If shipped behavior changes, Part A
#            fails first and tells you the driver needs re-characterizing.
#   PART B — runs the scenario driver end to end and requires ALL_CASES_PASSED
#            with exit 0, so a stale driver fails loudly.
#
# Deliberately asserts BEHAVIOR only. It does not grep the driver's source text:
# such assertions pass vacuously and break on harmless rewording.
#
# Run:  bash tests/gadugi/test-merge-validations-scenario-contract.sh
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RUN="$SCRIPT_DIR/run-merge-validations.sh"
DRIVER="$SCRIPT_DIR/run-merge-validations-scenario.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

fail=0
pass() { echo "PASS: $1"; }
fl()   { echo "FAIL: $1"; fail=1; }

[ -f "$RUN" ]    || { fl "missing harness: $RUN"; echo "SCENARIO_FAILED"; exit 1; }
[ -f "$DRIVER" ] || { fl "missing driver: $DRIVER"; echo "SCENARIO_FAILED"; exit 1; }

# ---------------------------------------------------------------------------
# PART A — shipped contract (source of truth).
# ---------------------------------------------------------------------------

# A1: a single unparseable validator among parseable ones is a WARNING, not
#     fatal: the merge still succeeds and that validator contributes no votes.
cat > "$WORK/m1.txt" <<'EOF'
```json
{"validated":[{"finding_id":1,"verdict":"confirmed","new_severity":"high"}]}
```
EOF
printf '%s\n' '{"validated":[{"finding_id":1,"verdict":"confirmed","new_severity":"low"}]}' > "$WORK/m2.txt"
printf '%s\n' 'ERROR: agent timed out, no structured output' > "$WORK/m3.txt"
mout="$("$RUN" "$WORK/m1.txt" "$WORK/m2.txt" "$WORK/m3.txt" 2 1 "$WORK/mdir" 2>"$WORK/merr.txt")"
mrc=$?
[ "$mrc" = "0" ] \
  && pass "shipped: one unparseable validator is non-fatal (exit 0)" \
  || fl  "shipped: mixed output exited $mrc, expected 0"
if grep -q "output unparseable; counting zero votes from it" "$WORK/merr.txt"; then
  pass "shipped: mixed output emits the 'output unparseable' WARNING"
else
  fl "shipped: expected 'output unparseable; counting zero votes from it' WARNING not found"
fi
mcc="$(printf '%s' "$mout" | jq -r '.confirmed_count' 2>/dev/null)"
[ "$mcc" = "1" ] \
  && pass "shipped: the two parseable validators still confirm (confirmed_count=1)" \
  || fl  "shipped: confirmed_count expected 1 on mixed output, got '$mcc'"

# A2: all-unparseable is FATAL — exit 1, FATAL diagnostic, NO merged JSON on
#     stdout, and the raw validator outputs preserved for triage. The point of
#     the hardening is that a caller must never receive a "merged" verdict set
#     that was silently built from zero votes.
printf '%s\n' 'no json here, just a log line' > "$WORK/g1.txt"
printf '%s\n' 'another { stray brace only'    > "$WORK/g2.txt"
printf '%s\n' 'timed out'                      > "$WORK/g3.txt"
gout="$("$RUN" "$WORK/g1.txt" "$WORK/g2.txt" "$WORK/g3.txt" 2 1 "$WORK/gdir" 2>"$WORK/gerr.txt")"
grc=$?
[ "$grc" = "1" ] \
  && pass "shipped: all-unparseable exits 1 (FATAL, not graceful)" \
  || fl  "shipped: all-unparseable exited $grc, expected 1"
if grep -q "FATAL: all validators produced unparseable output; cannot merge any verdicts" "$WORK/gerr.txt"; then
  pass "shipped: all-unparseable prints the FATAL diagnostic"
else
  fl "shipped: FATAL diagnostic not found for all-unparseable"
fi
[ -z "$gout" ] \
  && pass "shipped: all-unparseable writes no merged JSON to stdout" \
  || fl  "shipped: all-unparseable unexpectedly emitted JSON: '$gout'"
if ls "$WORK/gdir"/cycle_*/validator_*_raw.txt >/dev/null 2>&1; then
  pass "shipped: all-unparseable preserves validator_*_raw.txt artifacts"
else
  fl "shipped: validator_*_raw.txt artifacts not preserved"
fi

# A3: jq's "Bad JSON" must never leak from either path — that was the original
#     #820 crash this whole scenario family exists to prevent.
if grep -q "Bad JSON" "$WORK/merr.txt" "$WORK/gerr.txt"; then
  fl "shipped: jq 'Bad JSON' leaked (issue #820 regression)"
else
  pass "shipped: no jq 'Bad JSON' leak on any path"
fi

# ---------------------------------------------------------------------------
# PART B — the driver must agree with the shipped contract above.
# ---------------------------------------------------------------------------
if bash "$DRIVER" >"$WORK/driver.out" 2>&1; then drc=0; else drc=$?; fi
if grep -q "ALL_CASES_PASSED" "$WORK/driver.out" && [ "$drc" = "0" ]; then
  pass "driver run: ALL_CASES_PASSED and exit 0"
else
  fl "driver run failed (exit $drc); see output below:"
  sed 's/^/    driver| /' "$WORK/driver.out"
fi

if [ "$fail" -eq 0 ]; then echo "ALL_CASES_PASSED"; exit 0; fi
echo "SCENARIO_FAILED"; exit 1
