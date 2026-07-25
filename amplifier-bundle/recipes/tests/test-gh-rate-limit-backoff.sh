#!/usr/bin/env bash
# test-gh-rate-limit-backoff.sh — focused unit tests for the rate-limit-aware
# retry helper library (amplifier-bundle/tools/workflow_gh_retry.sh, issue #1009).
#
# Motivation: when the GitHub GraphQL/core quota is exhausted (0/5000, resets up
# to ~1 hour later), a plain 3x fast-backoff retry keeps hitting the still-empty
# quota and the workflow step fails closed. These helpers must instead read the
# authoritative reset window and wait for it, prefer a REST (core budget) fallback
# for read-only PR-existence lookups, never retry permanent auth errors, and keep
# the fast 3-attempt behaviour for generic 5xx/network transients.
#
# Covered contracts:
#   (a) a rate-limit stderr triggers a wait-for-reset (mock `gh api rate_limit`)
#       and, when no reset window is observable, does NOT wait blindly.
#   (b) a REST (core budget) fallback serves PR-existence when GraphQL is
#       exhausted but core still has budget.
#   (c) a permanent auth error is NEVER retried.
#   (d) a generic 5xx/network transient keeps the short-backoff classification
#       (routed away from the auth/rate-limit paths).
#
# Usage: bash amplifier-bundle/recipes/tests/test-gh-rate-limit-backoff.sh
# Exit codes: 0 = pass, 1 = test failure, 2 = harness error.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
LIB="${SCRIPT_DIR}/../../tools/workflow_gh_retry.sh"
[ -f "$LIB" ] || { echo "HARNESS ERROR: library not found at $LIB" >&2; exit 2; }

command -v jq >/dev/null 2>&1 || { echo "HARNESS ERROR: jq is required" >&2; exit 2; }

FAILURES=0
pass() { printf 'PASS: %s\n' "$1"; }
fail() { printf 'FAIL: %s\n' "$1" >&2; FAILURES=$((FAILURES + 1)); }

WORKDIR="$(mktemp -d)"
trap 'rm -rf "$WORKDIR"' EXIT

# --- Fake gh -----------------------------------------------------------------
# Behaviour is driven entirely by files under $WORKDIR so each test can script a
# deterministic sequence of gh responses without touching the network.
#   $WORKDIR/gh_calls              append-log of "$*" for every gh invocation
#   $WORKDIR/gh_create.seq         newline list of "RC|stdout" for issue create
#   $WORKDIR/rate_limit.json       body returned for `gh api rate_limit`
#   $WORKDIR/pulls.json            body returned for `gh api repos/.../pulls...`
#   $WORKDIR/core_remaining        remaining core budget for rate_limit stub
BIN_DIR="$WORKDIR/bin"
mkdir -p "$BIN_DIR"
cat > "$BIN_DIR/gh" <<'GH'
#!/usr/bin/env bash
printf '%s\n' "$*" >> "$WORKDIR/gh_calls"
if [ "${1:-}" = "api" ]; then
  shift
  # Skip option flags (e.g. --paginate) to reach the endpoint path.
  while [ "$#" -gt 0 ]; do
    case "${1:-}" in --*) shift ;; *) break ;; esac
  done
  case "${1:-}" in
    rate_limit)
      if [ -f "$WORKDIR/rate_limit.json" ]; then cat "$WORKDIR/rate_limit.json"; exit 0; fi
      exit 1 ;;
    repos/*pulls*)
      if [ -f "$WORKDIR/pulls.json" ]; then cat "$WORKDIR/pulls.json"; exit 0; fi
      exit 1 ;;
  esac
  exit 1
fi
if [ "${1:-}" = "issue" ] && [ "${2:-}" = "create" ]; then
  # Pop the next scripted "RC|stdout" line.
  seq_file="$WORKDIR/gh_create.seq"
  line="$(head -n1 "$seq_file" 2>/dev/null || true)"
  tail -n +2 "$seq_file" > "$seq_file.tmp" 2>/dev/null || true
  mv -f "$seq_file.tmp" "$seq_file" 2>/dev/null || true
  rc="${line%%|*}"; out="${line#*|}"
  [ -n "$out" ] && printf '%s\n' "$out"
  exit "${rc:-0}"
fi
exit 0
GH
chmod +x "$BIN_DIR/gh"
export WORKDIR
export PATH="$BIN_DIR:$PATH"

# Record sleeps instead of actually sleeping so tests stay fast and can assert a
# wait occurred and roughly how long.
cat > "$BIN_DIR/fake_sleep" <<'SLEEP'
#!/usr/bin/env bash
printf '%s\n' "${1:-0}" >> "$WORKDIR/sleeps"
exit 0
SLEEP
chmod +x "$BIN_DIR/fake_sleep"

reset_state() {
  : > "$WORKDIR/gh_calls"
  : > "$WORKDIR/sleeps"
  rm -f "$WORKDIR/rate_limit.json" "$WORKDIR/pulls.json" "$WORKDIR/gh_create.seq"
}

# Source the library under test with a stubbed sleep.
export WORKFLOW_GH_SLEEP_CMD="$BIN_DIR/fake_sleep"
export WORKFLOW_GH_RATE_LIMIT_ATTEMPTS=6
# shellcheck source=/dev/null
. "$LIB"

now_epoch() { date +%s; }

# ---------------------------------------------------------------------------
# (a) Rate-limit stderr triggers wait-for-reset; unobservable reset does NOT wait
# ---------------------------------------------------------------------------
test_rate_limit_waits_for_reset() {
  reset_state
  local now reset err
  now="$(now_epoch)"; reset="$((now + 40))"
  printf '{"resources":{"core":{"remaining":10,"reset":%s},"graphql":{"remaining":0,"reset":%s}}}\n' "$reset" "$reset" > "$WORKDIR/rate_limit.json"
  err="$WORKDIR/err"; printf 'GraphQL: API rate limit exceeded\n' > "$err"

  wf_gh_is_rate_limit_error "$err" || { fail "(a) rate-limit stderr not classified as rate limit"; return; }
  if wf_gh_wait_for_rate_limit "unit gh" auto "$err" 1; then
    local slept
    slept="$(head -n1 "$WORKDIR/sleeps" 2>/dev/null || echo 0)"
    # reset - now + slack(5) == ~45; allow a couple seconds of clock drift.
    if [ "$slept" -ge 40 ] && [ "$slept" -le 60 ]; then
      pass "(a) rate-limit waits until the observed reset window (slept=${slept}s)"
    else
      fail "(a) unexpected wait duration: slept=${slept}s (expected ~45)"
    fi
  else
    fail "(a) wait-for-reset returned non-zero despite an observable reset"
  fi
}

test_rate_limit_no_reset_does_not_blind_wait() {
  reset_state
  local err; err="$WORKDIR/err"
  printf 'secondary rate limit; please wait\n' > "$err"
  # No rate_limit.json and no Retry-After/X-RateLimit-Reset header -> unobservable.
  if wf_gh_wait_for_rate_limit "unit gh" auto "$err" 1; then
    fail "(a) wait-for-reset must return non-zero when the reset window is unobservable"
  else
    if [ -s "$WORKDIR/sleeps" ]; then
      fail "(a) wait-for-reset must NOT sleep when the reset window is unobservable"
    else
      pass "(a) unobservable reset window fails closed without a blind wait"
    fi
  fi
}

test_reset_from_header_when_api_unavailable() {
  reset_state
  local err; err="$WORKDIR/err"
  printf 'secondary rate limit\nRetry-After: 15\n' > "$err"
  # gh api rate_limit unavailable (no rate_limit.json) -> must honour Retry-After.
  if wf_gh_wait_for_rate_limit "unit gh" stderr "$err" 1; then
    local slept; slept="$(head -n1 "$WORKDIR/sleeps" 2>/dev/null || echo 0)"
    if [ "$slept" -ge 15 ] && [ "$slept" -le 25 ]; then
      pass "(a) Retry-After header drives the wait when gh api rate_limit is unavailable (slept=${slept}s)"
    else
      fail "(a) Retry-After wait duration unexpected: slept=${slept}s (expected ~20)"
    fi
  else
    fail "(a) Retry-After header should provide an observable reset window"
  fi
}

test_issue_create_waits_then_succeeds() {
  reset_state
  local now reset
  now="$(now_epoch)"; reset="$((now + 30))"
  printf '{"resources":{"core":{"remaining":0,"reset":%s},"graphql":{"remaining":0,"reset":%s}}}\n' "$reset" "$reset" > "$WORKDIR/rate_limit.json"
  # First (labelled) create attempt hits a rate limit; after the reset wait the
  # retried labelled create succeeds and returns the issue URL.
  {
    printf '1|API rate limit exceeded\n'
    printf '0|https://github.com/example-org/example-repo/issues/4242\n'
  } > "$WORKDIR/gh_create.seq"

  local out rc=0
  out="$(wf_gh_issue_create "Unit title" "Unit body" "workflow:default")" || rc=$?
  if [ "$rc" -eq 0 ] && [ "$out" = "https://github.com/example-org/example-repo/issues/4242" ]; then
    if [ -s "$WORKDIR/sleeps" ]; then
      pass "(a) wf_gh_issue_create waits for the reset then succeeds"
    else
      fail "(a) wf_gh_issue_create succeeded but never waited for the reset window"
    fi
  else
    fail "(a) wf_gh_issue_create rc=$rc out='$out' (expected success URL)"
  fi
}

# ---------------------------------------------------------------------------
# (b) REST (core budget) fallback for PR-existence when GraphQL exhausted
# ---------------------------------------------------------------------------
test_rest_fallback_pr_existence() {
  reset_state
  local now reset
  now="$(now_epoch)"; reset="$((now + 60))"
  # core has budget, graphql exhausted.
  printf '{"resources":{"core":{"remaining":4321,"reset":%s},"graphql":{"remaining":0,"reset":%s}}}\n' "$reset" "$reset" > "$WORKDIR/rate_limit.json"
  cat > "$WORKDIR/pulls.json" <<'JSON'
[
  {
    "number": 77,
    "title": "Add rate limit tolerance",
    "body": "b",
    "state": "open",
    "merged_at": null,
    "created_at": "2024-01-01T00:00:00Z",
    "html_url": "https://github.com/example-org/example-repo/pull/77",
    "draft": true,
    "head": {"ref": "feat/x", "sha": "abc123", "repo": {"name": "example-repo", "full_name": "example-org/example-repo", "owner": {"login": "example-org"}}},
    "base": {"ref": "main", "repo": {"full_name": "example-org/example-repo"}}
  }
]
JSON
  wf_gh_core_has_budget || { fail "(b) core budget should be reported as available"; return; }
  local out
  if out="$(wf_gh_pr_list_rest_fallback "example-org/example-repo" "feat/x" all)"; then
    local n state head draft cross
    n="$(printf '%s' "$out" | jq -r '.[0].number')"
    state="$(printf '%s' "$out" | jq -r '.[0].state')"
    head="$(printf '%s' "$out" | jq -r '.[0].headRefName')"
    draft="$(printf '%s' "$out" | jq -r '.[0].isDraft')"
    cross="$(printf '%s' "$out" | jq -r '.[0].isCrossRepository')"
    if [ "$n" = "77" ] && [ "$state" = "OPEN" ] && [ "$head" = "feat/x" ] && [ "$draft" = "true" ] && [ "$cross" = "false" ]; then
      pass "(b) REST fallback maps pulls to the gh pr list JSON shape"
    else
      fail "(b) REST fallback mapping wrong: number=$n state=$state head=$head draft=$draft cross=$cross"
    fi
  else
    fail "(b) REST fallback returned non-zero"
  fi
}

test_rest_fallback_merged_state() {
  reset_state
  cat > "$WORKDIR/pulls.json" <<'JSON'
[ { "number": 9, "state": "closed", "merged_at": "2024-02-02T00:00:00Z", "html_url": "u", "head": {"ref": "b", "sha": "s"}, "base": {"ref": "main"} } ]
JSON
  local out state
  out="$(wf_gh_pr_list_rest_fallback "o/r" "b" all)" || { fail "(b) REST fallback (merged) returned non-zero"; return; }
  state="$(printf '%s' "$out" | jq -r '.[0].state')"
  if [ "$state" = "MERGED" ]; then
    pass "(b) REST fallback maps merged_at to MERGED state"
  else
    fail "(b) merged state mapping wrong: $state"
  fi
}

# ---------------------------------------------------------------------------
# (c) Permanent auth error is NEVER retried
# ---------------------------------------------------------------------------
test_auth_error_not_retried() {
  reset_state
  local err; err="$WORKDIR/err"
  printf 'HTTP 401: Bad credentials\n' > "$err"
  wf_gh_is_auth_error "$err" || { fail "(c) 401/Bad credentials not classified as auth error"; return; }
  wf_gh_is_rate_limit_error "$err" && { fail "(c) auth error must NOT be classified as rate limit"; return; }

  # wf_gh_issue_create must attempt exactly once on a permanent auth failure.
  printf '1|HTTP 401: Bad credentials\n' > "$WORKDIR/gh_create.seq"
  local out rc=0
  out="$(wf_gh_issue_create "t" "b" "l")" || rc=$?
  local creates
  creates="$(grep -c '^issue create' "$WORKDIR/gh_calls" || true)"
  if [ "$rc" -ne 0 ] && [ "$creates" -eq 1 ] && [ ! -s "$WORKDIR/sleeps" ]; then
    pass "(c) auth error is surfaced without retry or wait (creates=${creates})"
  else
    fail "(c) auth handling wrong: rc=$rc creates=$creates slept=$( [ -s "$WORKDIR/sleeps" ] && echo yes || echo no ) out='$out'"
  fi
}

# ---------------------------------------------------------------------------
# (d) Generic 5xx keeps the short-backoff transient classification
# ---------------------------------------------------------------------------
test_generic_5xx_is_transient_not_ratelimit_not_auth() {
  reset_state
  local err; err="$WORKDIR/err"
  local ok=1
  for msg in "HTTP 503: Service Unavailable" "HTTP 502 Bad Gateway" "connection reset by peer"; do
    printf '%s\n' "$msg" > "$err"
    wf_gh_is_transient_error "$err" || { fail "(d) '$msg' should be transient"; ok=0; }
    wf_gh_is_rate_limit_error "$err" && { fail "(d) '$msg' must NOT be rate limit"; ok=0; }
    wf_gh_is_auth_error "$err" && { fail "(d) '$msg' must NOT be auth"; ok=0; }
  done
  [ "$ok" -eq 1 ] && pass "(d) generic 5xx/network transients route to the short-backoff path (not rate-limit, not auth)"
}

test_rate_limit_beats_transient_regex() {
  # The transient regex also matches the words "rate limit"; the caller loops and
  # this test assert the dedicated rate-limit classifier is consulted first so a
  # true rate limit takes the wait-for-reset path, not the fast 3x path.
  reset_state
  local err; err="$WORKDIR/err"
  printf 'You have exceeded a secondary rate limit\n' > "$err"
  if wf_gh_is_rate_limit_error "$err"; then
    pass "(d) secondary rate limit is classified as rate limit (checked before transient)"
  else
    fail "(d) secondary rate limit not classified as rate limit"
  fi
}

# --- Run ---------------------------------------------------------------------
test_rate_limit_waits_for_reset
test_rate_limit_no_reset_does_not_blind_wait
test_reset_from_header_when_api_unavailable
test_issue_create_waits_then_succeeds
test_rest_fallback_pr_existence
test_rest_fallback_merged_state
test_auth_error_not_retried
test_generic_5xx_is_transient_not_ratelimit_not_auth
test_rate_limit_beats_transient_regex

echo
if [ "$FAILURES" -eq 0 ]; then
  echo "PASS: gh rate-limit backoff helper contracts are covered."
  exit 0
fi
echo "FAIL: ${FAILURES} rate-limit backoff assertion(s) failed." >&2
exit 1
