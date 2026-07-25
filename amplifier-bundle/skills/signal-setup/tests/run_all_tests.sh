#!/usr/bin/env bash
# Unified test runner for the signal-setup skill.
#
# Usage:
#   bash amplifier-bundle/skills/signal-setup/tests/run_all_tests.sh
#
# Exit: 0 = all suites passed, non-zero = one or more failures.
#
# Suites:
#   1. test_skill_structure.sh  — SKILL.md/SECURITY.md contract + script-source
#                                 invariants (the four hard-won facts, gotchas,
#                                 security model, idempotency, --host contract).
#   2. test_script_behavior.sh  — the actual script under a sandboxed HOME with
#                                 mocked signal-cli/qrencode/systemd-run/az/sudo/nc
#                                 (help, validation, injection fail-closed,
#                                 idempotency-renders-no-QR, prereq failures).
#
# Intentionally omits -e so one failing suite never aborts the runner.
set -uo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

SUITES=(
  "test_skill_structure.sh"
  "test_script_behavior.sh"
)

TOTAL_FAIL=0
echo "###########################################################"
echo "#  signal-setup skill — full TDD suite"
echo "###########################################################"
for s in "${SUITES[@]}"; do
  echo ""
  echo ">>> Running $s"
  if bash "$SCRIPT_DIR/$s"; then
    echo "<<< $s: OK"
  else
    echo "<<< $s: FAILED"
    TOTAL_FAIL=$((TOTAL_FAIL + 1))
  fi
done

echo ""
echo "###########################################################"
if [[ "$TOTAL_FAIL" -eq 0 ]]; then
  echo "#  ALL SUITES PASSED"
else
  echo "#  $TOTAL_FAIL SUITE(S) FAILED"
fi
echo "###########################################################"

[[ "$TOTAL_FAIL" -gt 0 ]] && exit 1
exit 0
