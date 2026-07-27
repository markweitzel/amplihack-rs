# Merge Flow Reference

> [Home](../index.md) > Reference > Merge Flow

`main` is protected by 7 required status checks (see [CI Pipeline](ci-pipeline.md)):
`Lint & Format`, `Test`, `Install Smoke Test`, and the four `Build <target>` legs.

## The live-lock this flow avoids

`main` used to also require branches to be **up to date** before merging (the
"strict" policy). On a fast-moving `main` that created a **merge live-lock**:
every time `main` advanced, an open PR went `BEHIND`, had to rebase and re-run
the full ~35-minute matrix, and was frequently reset again before it could reach
`green + up-to-date` — PRs survived by racing a quiet window, not by converging
(issue #1050).

## Why not a GitHub merge queue?

GitHub's merge queue solves exactly this problem, but it is **not available for
this repository**. Merge queue is offered only for repositories owned by an
**organization**; `rysweet/amplihack-rs` is owned by a personal user account, so
the REST API rejects a `merge_queue` ruleset rule outright regardless of plan or
visibility. See
[Managing a merge queue](https://docs.github.com/en/repositories/configuring-branches-and-merges-in-your-repository/configuring-pull-request-merges/managing-a-merge-queue)
("available for public repositories … owned by an organization").

If this repository is ever transferred to an organization, enabling the queue is
straightforward: add a repository ruleset with a `merge_queue` rule and set the
classic strict policy off (the two cannot both be on). The CI workflow already
triggers on the `merge_group` event, so no workflow change would be needed.

## The available fix: drop the up-to-date requirement

Since a queue is not an option here, the live-lock is removed at its root by
turning the strict up-to-date policy **off** while keeping all 7 checks as gates:

```bash
gh api --method PATCH repos/OWNER/REPO/branches/main/protection/required_status_checks \
  -F strict=false
```

PRs still cannot merge until every required check is green, but they no longer
have to be rebased onto the very latest `main` first. That is what ends the
live-lock.

**Trade-off (be honest about it):** with strict off, a PR can merge without the
absolute latest `main` underneath it. Two PRs that each pass their own checks can
still interact badly once both are on `main` (a semantic conflict no single PR's
checks caught). A merge queue would prevent that by testing the combined result;
without one, the mitigation is low merge concurrency plus a green post-merge
`main` build. For this repository's PR volume that trade is acceptable and, unlike
the live-lock, it is rare and self-evident when it happens.

## Merge flow for contributors and agents

Do **not** hand-run merge watchers or `gh pr merge --admin` to race a quiet
`main`. Once a PR is green and ready, let auto-merge land it:

```bash
# Merges automatically as soon as the required checks pass and the PR is mergeable.
gh pr merge <number> --squash --auto
```

`--auto` does not require a merge queue. With the up-to-date requirement off, a
green PR becomes mergeable and lands without a manual rebase or a watched merge
window.

## Rollback

If turning off the up-to-date requirement causes problems, restore it:

```bash
gh api --method PATCH repos/OWNER/REPO/branches/main/protection/required_status_checks \
  -F strict=true
```
