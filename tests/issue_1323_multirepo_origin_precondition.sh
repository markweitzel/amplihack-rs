#!/usr/bin/env bash
# Issue #1323 — a repository with no `origin` remote must be refused up front,
# with a message that names the real problem.
#
# `smart-orchestrator` ran an operations task from a multi-repository workspace
# root. That root IS a git repository, so the existing
# `git rev-parse --is-inside-work-tree` precondition passed and the run
# proceeded. But the code lives in nested repositories, so the root has no
# `origin`. The first fetch failed with:
#
#     fatal: origin does not appear to be a git repository
#     ERROR: no supported remote base ref found. Expected origin/HEAD, origin/master, or origin/develop.
#
# Neither line mentions the actual problem. A reader goes looking for a missing
# branch or a broken remote URL, when repo_path simply points one level too
# high. The failure also arrives late, after the run has done work.
#
# These tests build real repositories — a workspace root with nested checkouts,
# and an ordinary repo with a remote — and drive the real helper.

set -uo pipefail

HELPER="$(cd "$(dirname "$0")/.." && pwd)/amplifier-bundle/tools/workflow_worktree_root.sh"
[ -x "$HELPER" ] || { echo "missing or non-executable $HELPER"; exit 1; }

fails=0
pass() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; fails=$((fails + 1)); }

WORK="$(mktemp -d "${TMPDIR:-/tmp}/issue1323.XXXXXX")"
trap 'rm -rf "$WORK"' EXIT

export GIT_CONFIG_NOSYSTEM=1
export HOME="$WORK/fakehome"; mkdir -p "$HOME"
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@e GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@e
unset GIT_DIR GIT_WORK_TREE GIT_INDEX_FILE

# --- fixture: a multi-repo workspace root, itself a repo, with NO origin -----
WS="$WORK/workspace"
mkdir -p "$WS"
git init -q -b main "$WS"
( cd "$WS" && echo ws > README.md && git add README.md && git commit -q -m ws )
for name in service-a service-b; do
  git init -q --bare "$WORK/${name}.git"
  git init -q -b main "$WS/$name"
  ( cd "$WS/$name" && git remote add origin "$WORK/${name}.git" \
      && echo x > f.txt && git add f.txt && git commit -q -m init )
done

# --- fixture: an ordinary repo WITH an origin -------------------------------
git init -q --bare "$WORK/plain.git"
git init -q -b main "$WORK/plain"
( cd "$WORK/plain" && git remote add origin "$WORK/plain.git" \
    && echo y > f.txt && git add f.txt && git commit -q -m init )

# --- 1. the workspace root is refused --------------------------------------
out="$(bash "$HELPER" assert-origin "$WS" 2>&1)"; rc=$?
if [ $rc -ne 0 ]; then
  pass "a git repo with no 'origin' is refused (exit $rc)"
else
  fail "workspace root with no origin was ACCEPTED"
fi

# The message has to name the real problem, or it is no better than the old one.
grep -q "no 'origin' remote" <<<"$out" \
  && pass "message names the missing origin remote" \
  || fail "message does not mention the missing origin: ${out:0:160}"

grep -qi 'multi-repository workspace' <<<"$out" \
  && pass "message identifies it as a multi-repository workspace" \
  || fail "message does not identify the workspace shape: ${out:0:160}"

# Naming the candidate repositories is the difference between a diagnosis and
# a dead end — the user has to know which path to point repo_path at.
if grep -q 'service-a' <<<"$out" && grep -q 'service-b' <<<"$out"; then
  pass "message lists the nested repositories to choose from"
else
  fail "message does not list the nested repositories: ${out:0:200}"
fi

# --- 2. a normal repo with an origin still passes ---------------------------
# The costly failure is a precondition so strict it blocks real runs.
if bash "$HELPER" assert-origin "$WORK/plain" >/dev/null 2>&1; then
  pass "a repo with an 'origin' remote is accepted"
else
  fail "a valid repo with an origin was refused"
fi
for nested in service-a service-b; do
  if bash "$HELPER" assert-origin "$WS/$nested" >/dev/null 2>&1; then
    pass "nested repo '$nested' is accepted"
  else
    fail "nested repo '$nested' was refused despite having an origin"
  fi
done

# --- 3. non-repository and missing paths ------------------------------------
mkdir -p "$WORK/plain-dir"
bash "$HELPER" assert-origin "$WORK/plain-dir" >/dev/null 2>&1 \
  && fail "a non-repository directory was accepted" \
  || pass "a non-repository directory is refused"
bash "$HELPER" assert-origin "$WORK/does-not-exist" >/dev/null 2>&1 \
  && fail "a missing path was accepted" \
  || pass "a missing path is refused"

# --- 4. the #1134 subcommands still work -----------------------------------
# This helper is shared; adding a subcommand must not disturb the others.
if bash "$HELPER" root "$WORK/plain" >/dev/null 2>&1; then
  pass "'root' subcommand still works"
else
  fail "'root' subcommand broke"
fi
bash "$HELPER" not-a-real-op >/dev/null 2>&1 \
  && fail "an unknown subcommand was accepted" \
  || pass "an unknown subcommand is still rejected"

echo
if [ "$fails" -gt 0 ]; then
  echo "issue-1323: $fails check(s) failed"
  exit 1
fi
echo "issue-1323: all checks passed"
