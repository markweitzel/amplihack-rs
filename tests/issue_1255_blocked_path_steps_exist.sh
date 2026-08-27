#!/usr/bin/env bash
# Issue #1255 — the dev-orchestrator skill's BLOCKED-path section must name
# steps that actually exist.
#
# SKILL.md documented the depth-limited fallback as two steps:
#
#     1. announce-depth-limited
#     2. execute-single-fallback-blocked
#
# Neither id exists. There is no announcement step at all — the classification
# in `derive-recursion-guard` prints the reason to stderr and *is* the
# announcement — and the fallback is two steps, `-development` and
# `-investigation`, chosen by the workstream's classification.
#
# A reader following that section goes looking for steps that were never there,
# and concludes the fallback is missing rather than misnamed. That is the same
# failure as #1348, where the docs described three CLI subcommands that did not
# exist.
#
# Deliberately scoped to this one section rather than every backticked token in
# every skill. A repo-wide heuristic flags 24 skill names, recipe names and
# model identifiers that share the same kebab shape as a step id; a check that
# needs two dozen exceptions is not a check.

set -uo pipefail

SKILL="amplifier-bundle/skills/dev-orchestrator/SKILL.md"
RECIPES="amplifier-bundle/recipes"
[ -f "$SKILL" ] || { echo "missing $SKILL (run from repo root)"; exit 1; }

fails=0
pass() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; fails=$((fails + 1)); }

# Every step id defined by any recipe.
ids="$(grep -rhoE '^ *- id: "[^"]+"' "$RECIPES"/*.yaml | sed 's/.*id: "//; s/"$//' | sort -u)"
[ -n "$ids" ] || { echo "  FAIL  parsed no step ids from $RECIPES — this check would pass vacuously"; exit 1; }

# The numbered list under the BLOCKED-path heading, up to the next blank-line
# paragraph that starts a new sentence block.
section="$(awk '/\*\*BLOCKED path \(recursion guard\)\*\*/{f=1} f{print} f && /^Both live in/{exit}' "$SKILL")"
if [ -z "$section" ]; then
  echo "  FAIL  could not locate the BLOCKED-path section in $SKILL — check would pass vacuously"
  exit 1
fi

# Backticked tokens in that section that look like step ids.
tokens="$(grep -oE '`[a-z0-9]+(-[a-z0-9]+)+`' <<<"$section" | tr -d '`' | sort -u)"
if [ -z "$tokens" ]; then
  fail "the BLOCKED-path section names no steps at all"
fi

checked=0
while IFS= read -r tok; do
  [ -n "$tok" ] || continue
  checked=$((checked + 1))
  if grep -qxF "$tok" <<<"$ids"; then
    pass "BLOCKED-path step '$tok' exists in a recipe"
  else
    fail "BLOCKED-path section names '$tok', which is not a step id in any recipe under $RECIPES"
  fi
done <<<"$tokens"

[ "$checked" -gt 0 ] || fail "no step names checked — the section may have been restructured"

# The two ids that actually implement the fallback must be documented, or the
# section is accurate-but-useless.
for required in execute-single-fallback-blocked-development execute-single-fallback-blocked-investigation; do
  grep -q "$required" <<<"$section" \
    && pass "section documents '$required'" \
    || fail "section does not mention '$required', which implements the fallback"
done

# And the id that never existed must not come back.
if grep -q 'announce-depth-limited' "$SKILL"; then
  fail "SKILL.md still references 'announce-depth-limited', which is not a step in any recipe"
else
  pass "the non-existent 'announce-depth-limited' step is not referenced"
fi

echo
if [ "$fails" -gt 0 ]; then
  echo "issue-1255: $fails check(s) failed"
  exit 1
fi
echo "issue-1255: all checks passed ($checked step name(s) verified)"
