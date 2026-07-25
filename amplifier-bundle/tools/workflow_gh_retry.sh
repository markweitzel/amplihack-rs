#!/usr/bin/env bash
# workflow_gh_retry.sh — sourceable rate-limit-aware retry helpers for the
# default-workflow gh/az helper scripts (issue #1009).
#
# Motivation: when the GitHub GraphQL/core quota is exhausted (0/5000, resets up
# to ~1 hour later), a plain 3x fast-backoff retry keeps hitting the still-empty
# quota and the workflow step fails closed. These helpers let callers:
#   * classify a failure as auth (never retry), rate-limit (wait for reset), or
#     generic transient (short fast backoff),
#   * read the authoritative reset time via `gh api rate_limit` (that endpoint
#     itself does NOT consume the core/graphql budget), honouring Retry-After /
#     X-RateLimit-Reset headers when present,
#   * wait adaptively until just past the observed reset window (NOT an arbitrary
#     fixed cap — a liveness safety clamp only guards against a bogus far-future
#     reset), and
#   * fall back to REST (core budget) for read-only PR-existence lookups when
#     GraphQL is exhausted but core still has budget.
#
# This file only DEFINES functions and MUST be sourced. It intentionally avoids
# top-level side effects (no `set -euo pipefail`, no execution) so it inherits
# the caller's shell options. Guard against double-sourcing.
#
# No forbidden `timeout` wrappers around gh live here: rate-limit waits are
# liveness-bounded by the reset window, not by an arbitrary wall-clock step
# timeout.

# Guard against double-sourcing. This file is meant to be sourced; `return`
# succeeds in that context. (If ever executed directly it is a harmless no-op.)
if [ -z "${WORKFLOW_GH_RETRY_LIB_SOURCED:-}" ]; then
  WORKFLOW_GH_RETRY_LIB_SOURCED=1
else
  # shellcheck disable=SC2317  # reached only on a second source of this lib
  return 0 2>/dev/null || true
fi

# Tunables (env-overridable). Defaults are conservative and adaptive.
: "${WORKFLOW_GH_TRANSIENT_ATTEMPTS:=3}"            # generic 5xx/network attempts
: "${WORKFLOW_GH_RATE_LIMIT_ATTEMPTS:=6}"           # wait-for-reset attempts
: "${WORKFLOW_GH_RESET_SLACK_SECONDS:=5}"           # sleep a little past reset
# Liveness safety clamp: guards ONLY against an obviously bogus far-future reset.
# Defaults to GitHub's published max window (3600s) + slack. It is not the driver
# of normal waits, which are governed by the observed reset window below.
: "${WORKFLOW_GH_MAX_RESET_WAIT_SECONDS:=3900}"
# Overridable so tests can stub the sleep (e.g. WORKFLOW_GH_SLEEP_CMD=true).
: "${WORKFLOW_GH_SLEEP_CMD:=sleep}"

# --- Classifiers (operate on a stderr FILE) --------------------------------

# Permanent authentication failure — MUST NOT be retried. 401 / Bad credentials
# only; a 403 is treated as rate-limit only when accompanied by rate-limit
# wording (see wf_gh_is_rate_limit_error).
wf_gh_is_auth_error() {
  [ -s "$1" ] && grep -Eiq '(^|[^0-9])401([^0-9]|$)|HTTP 401|401 Unauthorized|Bad credentials|requires authentication|must authenticate|authentication failed|not logged into|gh auth login' "$1"
}

# Rate-limit / secondary-rate-limit / abuse — wait for the reset window.
wf_gh_is_rate_limit_error() {
  [ -s "$1" ] && grep -Eiq 'rate limit|api rate limit exceeded|secondary rate limit|abuse detection|you have exceeded|(^|[^0-9])429([^0-9]|$)|HTTP 429|x-ratelimit-remaining:[[:space:]]*0|retry-after' "$1"
}

# Generic transient (5xx / network / timeout). Kept in sync with the callers'
# local is_transient_gh_error regex. Note: rate-limit is checked BEFORE this in
# every caller loop, so the "rate limit" alternate here never steals the
# wait-for-reset path.
wf_gh_is_transient_error() {
  [ -s "$1" ] && grep -Eiq 'HTTP 5[0-9][0-9]|(^|[^0-9])(502|503|504)([^0-9]|$)|rate limit|timed out|timeout|temporar|connection reset|connection refused|TLS handshake|network|server error' "$1"
}

# --- Rate-limit budget / reset introspection -------------------------------

# Echo the epoch reset for a resource ("core"|"graphql"|"search"|"auto").
# "auto" picks the soonest reset among exhausted resources, else the soonest of
# core/graphql. Uses `gh api rate_limit`, which does NOT consume any budget.
# Prints nothing and returns non-zero when the reset cannot be read.
wf_gh_rate_limit_reset() {
  local resource="${1:-auto}" json
  # "stderr" means: do not consult GitHub's rate_limit endpoint (e.g. AzDO
  # context); the caller relies solely on response headers via
  # wf_gh_reset_from_stderr.
  [ "$resource" = "stderr" ] && return 1
  command -v gh >/dev/null 2>&1 || return 1
  command -v jq >/dev/null 2>&1 || return 1
  json="$(gh api rate_limit 2>/dev/null)" || return 1
  [ -n "$json" ] || return 1
  if [ "$resource" = "auto" ]; then
    printf '%s' "$json" | jq -r '
      ( [ .resources | to_entries[] | select(.value.remaining == 0) | .value.reset ] | min ) //
      ( [ .resources.core.reset, .resources.graphql.reset ] | map(select(. != null)) | min ) //
      empty' 2>/dev/null
  else
    printf '%s' "$json" | jq -r --arg r "$resource" '.resources[$r].reset // empty' 2>/dev/null
  fi
}

# Return 0 iff the REST core budget still has remaining requests.
wf_gh_core_has_budget() {
  local json rem
  command -v gh >/dev/null 2>&1 || return 1
  command -v jq >/dev/null 2>&1 || return 1
  json="$(gh api rate_limit 2>/dev/null)" || return 1
  rem="$(printf '%s' "$json" | jq -r '.resources.core.remaining // 0' 2>/dev/null)" || return 1
  case "$rem" in ''|*[!0-9]*) return 1 ;; esac
  [ "$rem" -gt 0 ]
}

# Parse an epoch reset from a saved gh response/stderr file via Retry-After
# (delta seconds) or X-RateLimit-Reset (epoch). Prints nothing on miss.
wf_gh_reset_from_stderr() {
  local file="$1" xr ra now
  [ -n "$file" ] && [ -s "$file" ] || return 1
  xr="$(grep -oiE 'x-ratelimit-reset:[[:space:]]*[0-9]+' "$file" 2>/dev/null | grep -oE '[0-9]+' | tail -1 || true)"
  if [ -n "$xr" ]; then printf '%s' "$xr"; return 0; fi
  ra="$(grep -oiE 'retry-after:[[:space:]]*[0-9]+' "$file" 2>/dev/null | grep -oE '[0-9]+' | tail -1 || true)"
  if [ -n "$ra" ]; then now="$(date +%s)"; printf '%s' "$((now + ra))"; return 0; fi
  return 1
}

# Adaptive wait for a rate-limit reset. Governed by the observed reset window;
# clamped only by the liveness safety ceiling. Always logs what it is doing.
# Returns 0 after sleeping when an authoritative reset window was obtained.
# Returns non-zero WITHOUT sleeping when the reset window is unobservable (both
# `gh api rate_limit` and response headers gave nothing) so the caller can fall
# through instead of blind-waiting an arbitrary fixed amount.
# Usage: wf_gh_wait_for_rate_limit <label> [resource] [stderr_file] [attempt]
wf_gh_wait_for_rate_limit() {
  local label="$1" resource="${2:-auto}" stderr_file="${3:-}" attempt="${4:-1}"
  local now reset wait slack max
  slack="${WORKFLOW_GH_RESET_SLACK_SECONDS}"
  max="${WORKFLOW_GH_MAX_RESET_WAIT_SECONDS}"
  now="$(date +%s)"
  reset="$(wf_gh_rate_limit_reset "$resource" || true)"
  if [ -z "$reset" ] && [ -n "$stderr_file" ]; then
    reset="$(wf_gh_reset_from_stderr "$stderr_file" || true)"
  fi
  # No authoritative reset window observable -> do NOT blind-wait an arbitrary
  # fixed amount (repo policy: no arbitrary fixed caps). Signal the caller to
  # fall through to its normal fail/backoff path.
  case "$reset" in
    ''|*[!0-9]*)
      echo "WARNING: ${label}: GitHub rate limit hit but the authoritative reset window is unavailable (gh api rate_limit and response headers gave no reset); not waiting blindly (attempt ${attempt})" >&2
      return 1
      ;;
  esac
  if [ "$reset" -gt "$now" ]; then
    wait="$(( reset - now + slack ))"
  else
    # Reset already elapsed; a brief settle wait lets the window roll over.
    wait="$slack"
  fi
  if [ "$wait" -gt "$max" ]; then
    echo "WARNING: ${label}: reported rate-limit reset ${wait}s in the future exceeds safety clamp ${max}s; waiting ${max}s then re-checking (attempt ${attempt})" >&2
    wait="$max"
  fi
  [ "$wait" -lt 1 ] && wait=1
  echo "WARNING: ${label}: GitHub rate limit hit; waiting ${wait}s for quota reset before retry (attempt ${attempt})" >&2
  "${WORKFLOW_GH_SLEEP_CMD}" "$wait"
  return 0
}

# --- Rate-limit-aware GitHub issue creation --------------------------------

# Create a GitHub issue, waiting for the authoritative rate-limit reset window on
# a transient quota exhaustion instead of aborting the run (issue #1009). Echoes
# gh's combined output (issue URL on success, error text on failure) to stdout
# and returns gh's exit status. Permanent auth errors are never retried; a rate
# limit only waits when its reset window is observable, otherwise it fails closed
# like any other error (no arbitrary fixed sleep). Mirrors the step-03 label
# fallback (create with the workflow label, then without it).
# Usage: wf_gh_issue_create <title> <body> [label]
wf_gh_issue_create() {
  local title="$1" body="$2" label="${3:-}" out rc rl_attempt=0 f label_tried=0
  local -a args
  while :; do
    rc=0
    args=(issue create --title "$title" --body "$body")
    if [ -n "$label" ] && [ "$label_tried" -eq 0 ]; then
      args+=(--label "$label")
    fi
    out="$(timeout 60 gh "${args[@]}" 2>&1)" || rc=$?
    if [ "$rc" -eq 0 ]; then printf '%s' "$out"; return 0; fi
    f="$(mktemp)"; printf '%s' "$out" > "$f"
    # Permanent auth failure — never retry, never fall back.
    if wf_gh_is_auth_error "$f"; then rm -f "$f"; printf '%s' "$out"; return "$rc"; fi
    # Rate limit — wait for the authoritative reset window when observable, then
    # retry the SAME (labelled) create instead of aborting the run.
    if wf_gh_is_rate_limit_error "$f"; then
      rl_attempt=$((rl_attempt + 1))
      if [ "$rl_attempt" -le "${WORKFLOW_GH_RATE_LIMIT_ATTEMPTS}" ] \
         && wf_gh_wait_for_rate_limit "gh issue create" auto "$f" "$rl_attempt"; then
        rm -f "$f"
        continue
      fi
      [ "$rl_attempt" -gt "${WORKFLOW_GH_RATE_LIMIT_ATTEMPTS}" ] && echo "WARNING: gh issue create: GitHub rate limit did not clear after ${rl_attempt} reset waits." >&2
      rm -f "$f"; printf '%s' "$out"; return "$rc"
    fi
    # Other failure with a label set — the label may not exist yet; retry once
    # without it (mirrors the original step-03 label fallback).
    if [ -n "$label" ] && [ "$label_tried" -eq 0 ]; then
      label_tried=1; rm -f "$f"; continue
    fi
    rm -f "$f"
    printf '%s' "$out"
    return "$rc"
  done
}

# Emit the same JSON array shape as `gh pr list --json ...` for the PRs whose
# head is <head_branch>, using the REST pulls endpoint (core budget) so a
# GraphQL-only outage does not block PR existence detection. Prints a JSON array
# on success; returns non-zero on failure. NEVER silent — callers log the
# fallback explicitly.
# Usage: wf_gh_pr_list_rest_fallback <owner/repo> <head_branch> [state]
wf_gh_pr_list_rest_fallback() {
  local repo="$1" head_branch="$2" state="${3:-all}" owner rest
  command -v gh >/dev/null 2>&1 || return 1
  command -v jq >/dev/null 2>&1 || return 1
  case "$repo" in */*) ;; *) return 1 ;; esac
  owner="${repo%%/*}"
  case "$state" in open|closed|all) ;; *) state="all" ;; esac
  rest="$(gh api --paginate "repos/${repo}/pulls?state=${state}&head=${owner}:${head_branch}&per_page=100" 2>/dev/null)" || return 1
  [ -n "$rest" ] || rest="[]"
  printf '%s' "$rest" | jq -c '
    ( if type == "array" then . else [.] end )
    | [ .[] | {
        number: .number,
        title: (.title // ""),
        body: (.body // ""),
        state: ( if .merged_at != null then "MERGED" elif .state == "closed" then "CLOSED" else "OPEN" end ),
        createdAt: (.created_at // ""),
        mergedAt: .merged_at,
        url: (.html_url // ""),
        headRefName: (.head.ref // ""),
        baseRefName: (.base.ref // ""),
        headRefOid: (.head.sha // ""),
        headRepositoryOwner: { login: (.head.repo.owner.login // .head.user.login // "") },
        headRepository: { name: (.head.repo.name // "") },
        isCrossRepository: ( (.head.repo.full_name // "") != (.base.repo.full_name // "") ),
        isDraft: (.draft // false)
      } ]' 2>/dev/null || return 1
}
