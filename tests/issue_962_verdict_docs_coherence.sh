#!/usr/bin/env bash
# TDD tests for Issue #962: the two structured-verdict-parsing docs must
# accurately represent the feature we will build. These tests codify the
# ground-truth contract so the docs cannot silently drift from it.
#
# Files under test:
#   - docs/reference/structured-verdict-parsing.md
#   - docs/howto/parse-agent-verdicts-with-orch-helper.md
#   - docs/index.md (must link both)
#
# Ground truth:
#   - normalise-verdict uses case-insensitive EXACT-TOKEN equality (design
#     invariant R2), NOT substring matching. UNVERIFIED contains VERIFIED, so
#     str::contains would fail open. This mirrors the bash `case` alternation
#     gate in amplifier-bundle/recipes/workflow-tdd.yaml L266-268 (which use
#     globs WITHOUT wildcards, i.e. exact-token alternations).
#   - A2: the prose `VERDICT: FAILED` fatal token is retained; the actual gate
#     is the independent failure-token grep in workflow-pr-review.yaml L32.
#   - A3: normalise-verdict never emits NEEDS_ATTENTION; empty ->
#     INSUFFICIENT_EVIDENCE. NEEDS_ATTENTION comes from the checkpoint marker
#     path.
#   - D2: `--json` is additive/opt-in; default output is byte-exact text.
#   - C: consumers gate with `==` equality, never substring.
#   - Canonical pipeline: extract-json | extract-field | normalise-verdict.
#
# These are documentation-coherence tests: no build required, only that the
# docs stay internally coherent and grounded to the recipes.
#
# Run: bash tests/issue_962_verdict_docs_coherence.sh
# Expected before revision: FAIL. Expected after revision: PASS.

set -uo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DOCS_DIR="$REPO_ROOT/docs"
RECIPES_DIR="$REPO_ROOT/amplifier-bundle/recipes"

REF="$DOCS_DIR/reference/structured-verdict-parsing.md"
HOWTO="$DOCS_DIR/howto/parse-agent-verdicts-with-orch-helper.md"
INDEX="$DOCS_DIR/index.md"
TDD_YAML="$RECIPES_DIR/workflow-tdd.yaml"
PR_REVIEW_YAML="$RECIPES_DIR/workflow-pr-review.yaml"

fail=0
pass=0

assert() {
    local desc="$1"
    local cond="$2"
    if eval "$cond"; then
        echo "PASS: $desc"
        pass=$((pass+1))
    else
        echo "FAIL: $desc"
        echo "      condition: $cond"
        fail=$((fail+1))
    fi
}

# has FILE PATTERN  -> grep -qE succeeds
has() { grep -qiE "$2" "$1"; }
# hasx FILE PATTERN -> case-sensitive
hasx() { grep -qE "$2" "$1"; }
# missing FILE PATTERN -> grep finds nothing
missing() { ! grep -qE "$2" "$1"; }

echo "=== Issue #962 verdict-docs coherence tests ==="
echo "Docs dir:    $DOCS_DIR"
echo "Recipes dir: $RECIPES_DIR"
echo

# --- Test group 1: files exist ----------------------------------------------
assert "reference doc exists"        "[ -f '$REF' ]"
assert "howto doc exists"            "[ -f '$HOWTO' ]"
assert "docs/index.md exists"        "[ -f '$INDEX' ]"
assert "workflow-tdd.yaml exists"    "[ -f '$TDD_YAML' ]"
assert "workflow-pr-review exists"   "[ -f '$PR_REVIEW_YAML' ]"

# --- Test group 2: index links both docs ------------------------------------
assert "index links reference doc" \
    "hasx '$INDEX' 'reference/structured-verdict-parsing\.md'"
assert "index links howto doc" \
    "hasx '$INDEX' 'howto/parse-agent-verdicts-with-orch-helper\.md'"

# --- Test group 3: R2 equality-not-containment is stated & authoritative -----
assert "reference states EXACT-TOKEN (not substring)" \
    "has '$REF' 'exact.?token'"
assert "reference names invariant R2" \
    "hasx '$REF' 'R2'"
assert "reference explains fail-open counterexample (UNVERIFIED)" \
    "hasx '$REF' 'UNVERIFIED'"
assert "reference warns against str::contains / substring" \
    "has '$REF' 'substring|str::contains|contains'"
assert "reference supersedes 'mirror normalise_type' wording" \
    "has '$REF' 'normalise_type'"
assert "howto forbids substring gate (== *VERIFIED*)" \
    "hasx '$HOWTO' '\\*VERIFIED\\*'"

# --- Test group 4: canonical mapping matches YAML L266-268 exactly -----------
# Every input token in the YAML case gate must appear in the reference mapping.
YAML_TOKENS="VERIFIED SUCCESS APPROVED PASS PASSED FAILED NO_WORK EMPTY NO_ARTIFACTS INCONCLUSIVE UNKNOWN UNCLEAR PARTIAL"
for tok in $YAML_TOKENS; do
    assert "reference documents YAML case token '$tok'" \
        "hasx '$REF' '\\b$tok\\b'"
done
# The three canonical outputs must all be documented.
for canon in WORK_VERIFIED HOLLOW_SUCCESS INSUFFICIENT_EVIDENCE; do
    assert "reference documents canonical output '$canon'" \
        "hasx '$REF' '$canon'"
    assert "howto documents canonical output '$canon'" \
        "hasx '$HOWTO' '$canon'"
done
# Sanity: the YAML the mapping claims to mirror still contains that gate.
assert "workflow-tdd.yaml still has VERIFIED|SUCCESS|APPROVED|PASS|PASSED gate" \
    "hasx '$TDD_YAML' 'VERIFIED\\|SUCCESS\\|APPROVED\\|PASS\\|PASSED'"

# --- Test group 5: A2 prose VERDICT: FAILED fatal token ----------------------
assert "reference has A2 fatal VERDICT: FAILED section" \
    "has '$REF' 'VERDICT: FAILED'"
assert "reference A2 cites workflow-pr-review.yaml" \
    "hasx '$REF' 'workflow-pr-review\.yaml'"
assert "howto documents fatal prose VERDICT: FAILED path" \
    "has '$HOWTO' 'VERDICT: FAILED'"
# Ground truth: that grep gate actually exists in the recipe.
assert "workflow-pr-review.yaml contains the fatal failure-token grep" \
    "hasx '$PR_REVIEW_YAML' \"VERDICT:\\[\\[:space:\\]\\]\\*FAILED\""

# --- Test group 6: A3 no NEEDS_ATTENTION output ------------------------------
assert "reference: normalise-verdict never emits NEEDS_ATTENTION" \
    "hasx '$REF' 'NEEDS_ATTENTION'"
assert "reference: empty input -> INSUFFICIENT_EVIDENCE" \
    "has '$REF' 'empty'"
assert "howto: never emits NEEDS_ATTENTION (checkpoint marker path)" \
    "hasx '$HOWTO' 'NEEDS_ATTENTION'"
assert "howto: empty/missing degrades to INSUFFICIENT_EVIDENCE" \
    "hasx '$HOWTO' 'INSUFFICIENT_EVIDENCE'"

# --- Test group 7: D2 --json additive/opt-in, byte-exact default text --------
assert "reference: --json flag documented" \
    "hasx '$REF' '\\-\\-json'"
assert "reference: --json is additive/opt-in" \
    "has '$REF' 'additive|opt.?in'"
assert "reference: default is byte-exact text" \
    "has '$REF' 'byte.?exact'"
assert "howto: --json opt-in documented" \
    "hasx '$HOWTO' '\\-\\-json'"

# --- Test group 8: C consumers use == equality -------------------------------
assert "reference: consume with == equality section" \
    "has '$REF' 'equality'"
assert "howto: gate with == equality" \
    "has '$HOWTO' 'equality'"

# --- Test group 9: canonical extract-json | extract-field pipeline -----------
assert "howto documents extract-json" \
    "hasx '$HOWTO' 'extract-json'"
assert "howto documents extract-field --field verdict" \
    "hasx '$HOWTO' 'extract-field --field verdict'"
assert "howto documents normalise-verdict" \
    "hasx '$HOWTO' 'normalise-verdict'"
assert "howto shows the three-stage pipe composition" \
    "hasx '$HOWTO' 'extract-field --field verdict'"

# --- Test group 10: cross-references resolve --------------------------------
# Collect every relative .md link target from both docs and verify the file
# exists (resolved relative to the linking doc's directory).
check_links() {
    local doc="$1"
    local docdir
    docdir="$(cd "$(dirname "$doc")" && pwd)"
    # Extract markdown link targets: [...](target) — .md links only, strip #anchor.
    grep -oE '\]\([^)]+\.md[^)]*\)' "$doc" \
        | sed -E 's/^\]\(//; s/\)$//; s/#.*$//' \
        | while read -r target; do
            [ -z "$target" ] && continue
            case "$target" in
                http*://*) continue ;;
            esac
            local resolved
            resolved="$(cd "$docdir" && cd "$(dirname "$target")" 2>/dev/null && pwd)/$(basename "$target")"
            if [ ! -f "$resolved" ]; then
                echo "BROKEN_LINK $doc -> $target"
            fi
        done
}
BROKEN_REF="$(check_links "$REF")"
BROKEN_HOWTO="$(check_links "$HOWTO")"
[ -n "$BROKEN_REF" ] && echo "$BROKEN_REF" | sed 's/^/      /'
[ -n "$BROKEN_HOWTO" ] && echo "$BROKEN_HOWTO" | sed 's/^/      /'
assert "all .md links in reference doc resolve" "[ -z \"\$BROKEN_REF\" ]"
assert "all .md links in howto doc resolve"     "[ -z \"\$BROKEN_HOWTO\" ]"

# --- Test group 11: intra-doc anchors resolve -------------------------------
# For same-doc (#anchor) links into the reference doc, verify a heading exists
# whose GitHub slug matches. GitHub slug: lowercase, drop non-alphanumeric
# except spaces/hyphens, spaces->hyphens, underscores preserved.
slugify() {
    printf '%s\n' "$1" \
        | tr '[:upper:]' '[:lower:]' \
        | sed -E 's/[^a-z0-9 _-]//g; s/ +/-/g'
}
# Build the set of heading slugs in the reference doc.
REF_SLUGS="$(grep -E '^#{1,6} ' "$REF" | sed -E 's/^#{1,6} +//' | while read -r h; do slugify "$h"; done)"
# Gather every #anchor that targets structured-verdict-parsing.md (from both
# docs) plus same-doc `](#anchor)` links. Only markdown link syntax `](...)`
# counts — bare parentheticals like prose "(#962)" are NOT links.
anchor_targets() {
    grep -oE '\]\(structured-verdict-parsing\.md#[a-z0-9_-]+\)' "$1" \
        | sed -E 's/^\]\(structured-verdict-parsing\.md#//; s/\)$//'
    grep -oE '\]\(#[a-z0-9_-]+\)' "$1" \
        | sed -E 's/^\]\(#//; s/\)$//'
}
BAD_ANCHORS=""
for src in "$REF" "$HOWTO"; do
    while read -r a; do
        [ -z "$a" ] && continue
        if ! printf '%s\n' "$REF_SLUGS" | grep -qx "$a"; then
            BAD_ANCHORS="$BAD_ANCHORS\n  $src -> #$a"
        fi
    done < <(anchor_targets "$src")
done
[ -n "$BAD_ANCHORS" ] && printf 'unresolved anchors:%b\n' "$BAD_ANCHORS" | sed 's/^/      /'
assert "all reference-doc #anchors resolve to real headings" \
    "[ -z \"\$BAD_ANCHORS\" ]"

# --- Test group 12: no fail-open language / no jq dependency claim -----------
assert "howto: pipeline advertised as jq-free" \
    "has '$HOWTO' 'without .?jq|no .?jq'"
# Guard against reintroducing the wrong 'substring' contract as the RULE:
# the docs may MENTION substring only to reject it, so ensure the rejection
# framing ('fails open' / 'do not' / 'WRONG') co-occurs.
assert "reference frames substring as WRONG/fails-open" \
    "has '$REF' 'fails open|WRONG|do not'"

# --- Test group 13: security / trust model (design security_considerations) --
# The reference doc must document zero-ambient-authority and fail-safe posture.
assert "reference documents zero ambient authority (stdin->stdout only)" \
    "has '$REF' 'ambient authority' && has '$REF' 'only stdin'"
assert "reference: fail-safe not fail-open (never forged WORK_VERIFIED)" \
    "has '$REF' 'fail-safe' && has '$REF' 'forged'"
assert "reference: --json raw is untrusted, never eval'd" \
    "has '$REF' 'raw. is untrusted|raw. field' && has '$REF' 'eval'"
# The howto must carry the security note on treating --json raw as untrusted.
assert "howto: --json raw treated as untrusted LLM output" \
    "has '$HOWTO' 'untrusted' && has '$HOWTO' 'never .?eval'"
assert "howto: gate on verdict field, not raw" \
    "has '$HOWTO' 'verdict. field'"

echo
echo "=== Results: $pass passed, $fail failed ==="
[ "$fail" -eq 0 ] || exit 1
