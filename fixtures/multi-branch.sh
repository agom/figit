#!/usr/bin/env bash
# Build a large repository: many branches, merges, and enough commits to
# exercise virtualized scrolling (>= 200 commits).
set -euo pipefail

DIR="${1:?usage: $0 <dir>}"
BRANCHES="${2:-10}"
COMMITS_PER_BRANCH="${3:-15}"
rm -rf "$DIR"
mkdir -p "$DIR"

git -C "$DIR" init -q -b main
git -C "$DIR" config user.email test@example.com
git -C "$DIR" config user.name "Test User"

# a few base commits on main
for i in 1 2 3; do
  echo "base $i" >"$DIR/base.txt"
  git -C "$DIR" add base.txt
  git -C "$DIR" commit -q -m "base $i"
done

# create branches, each committing to its own file
for b in $(seq 1 "$BRANCHES"); do
  git -C "$DIR" checkout -q -b "branch-$b"
  for c in $(seq 1 "$COMMITS_PER_BRANCH"); do
    echo "branch-$b commit $c" >"$DIR/branch-$b.txt"
    git -C "$DIR" add "branch-$b.txt"
    git -C "$DIR" commit -q -m "branch $b commit $c"
  done
  # merge every other branch back into main
  if [ $((b % 2)) -eq 0 ]; then
    git -C "$DIR" checkout -q main
    git -C "$DIR" merge -q --no-ff "branch-$b" -m "merge branch-$b"
    git -C "$DIR" checkout -q "branch-$b"
  fi
done

git -C "$DIR" checkout -q main

echo "fixture: multi-branch repo with $BRANCHES branches in $DIR"