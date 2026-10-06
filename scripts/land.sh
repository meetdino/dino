#!/bin/bash
# Land the current branch on main, the one way every change lands: rebased on origin/main, checked
# with scripts/check.sh, pushed, and squash-merged through a pull request as one commit.
#
#   scripts/land.sh ["Title"]           wait for the pull request's required checks, then merge
#   scripts/land.sh --admin ["Title"]   merge once scripts/check.sh passes, without waiting for CI
#                                       (an admin's bypass of the ruleset: for when CI can't run)
#
# The title is one line; without one, the branch's commit subject, when it has a single commit.
# The squashed commit is the title, then the Co-Authored-By and Signed-off-by lines of the branch's
# commits. If main moves while CI runs, the branch is rebased and checked again, so what lands is
# what was checked. Afterwards, where scripts/install-hooks.sh ran, the main checkout follows main
# and the dino you use rebuilds (scripts/dev-rebuild.sh).
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

admin=
if [ "${1:-}" = --admin ]; then admin=1; shift; fi
title=${1:-}

branch=$(git symbolic-ref -q --short HEAD) || { echo "not on a branch" >&2; exit 1; }
if [ "$branch" = main ]; then
    echo "land a branch, not main: git switch -c NAME first" >&2
    exit 1
fi
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    echo "commit or stash your changes first" >&2
    exit 1
fi

# Wait for the pull request's checks: the required ones, or all of them where none are required.
wait_for_checks() {
    local pr=$1 i out status
    for i in $(seq 24); do   # checks take a few seconds to appear after a push
        status=0
        out=$(gh pr checks "$pr" --required 2>&1) || status=$?
        case $status in
            0) return 0 ;;
            8) gh pr checks "$pr" --required --watch --fail-fast --interval 30; return ;;
        esac
        if ! grep -q 'no required checks reported\|no checks reported' <<<"$out"; then
            echo "$out" >&2   # a required check failed
            return 1
        fi
        # Some checks but none required after a minute: there's no ruleset; wait for them all.
        if [ "$i" -ge 12 ] && ! grep -q 'no checks reported' <<<"$(gh pr checks "$pr" 2>&1)"; then
            gh pr checks "$pr" --watch --fail-fast --interval 30
            return
        fi
        sleep 5
    done
    echo "no checks started on #$pr within two minutes: is Actions enabled?" >&2
    return 1
}

trailers() { git log --format="%(trailers:key=$1,unfold,valueonly=false)" origin/main..HEAD | sed '/^$/d' | awk '!seen[tolower($0)]++'; }

for _ in 1 2 3; do
    git fetch -q origin
    if ! git rebase -q origin/main; then
        echo "the rebase on origin/main stopped at a conflict: resolve it (git rebase --continue), then run this again" >&2
        exit 1
    fi
    base=$(git rev-parse origin/main)
    commits=$(git rev-list --count origin/main..HEAD)
    if [ "$commits" = 0 ]; then
        echo "nothing to land: $branch has no commits that main doesn't" >&2
        exit 1
    fi
    if [ -z "$title" ]; then
        if [ "$commits" != 1 ]; then
            echo "$branch has $commits commits: give the squashed commit its title, scripts/land.sh \"Title\"" >&2
            exit 1
        fi
        title=$(git log -1 --format=%s)
    fi
    body=$(trailers Co-Authored-By; trailers Signed-off-by)
    if ! grep -qi '^Signed-off-by:' <<<"$body"; then
        echo "no commit on $branch is signed off: git rebase --signoff origin/main (see CONTRIBUTING.md)" >&2
        exit 1
    fi

    scripts/check.sh
    head=$(git rev-parse HEAD)
    git push -q --force-with-lease origin "HEAD:refs/heads/$branch"

    pr=$(gh pr list --head "$branch" --base main --state open --json number --jq '.[0].number // empty')
    description="$(git log --reverse --format='- %s' origin/main..HEAD)

\`scripts/check.sh\` passed on this branch rebased on $(git rev-parse --short "$base")."
    if [ -z "$pr" ]; then
        gh pr create --base main --head "$branch" --title "$title" --body "$description" >/dev/null
        pr=$(gh pr list --head "$branch" --base main --state open --json number --jq '.[0].number')
    else
        gh pr edit "$pr" --title "$title" --body "$description" >/dev/null
    fi
    echo "pull request #$pr: $title"

    if [ -z "$admin" ]; then
        wait_for_checks "$pr"
        git fetch -q origin
        if [ "$(git rev-parse origin/main)" != "$base" ]; then
            echo "main moved while CI ran: rebasing and checking again"
            continue
        fi
    fi
    gh pr merge "$pr" --squash ${admin:+--admin} --match-head-commit "$head" --subject "$title" --body "$body"
    git push -q origin --delete "$branch" 2>/dev/null || true   # unless GitHub deleted it already
    git fetch -q origin
    echo "landed as $(git log -1 --format='%h %s' origin/main)"
    scripts/dev-rebuild.sh --sync origin/main || true
    exit 0
done
echo "main kept moving while CI ran: run this again" >&2
exit 1
