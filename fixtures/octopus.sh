#!/usr/bin/env bash
# Build a repository containing an octopus merge (a commit with 3 parents).
set -euo pipefail

DIR="${1:?usage: $0 <dir>}"
rm -rf "$DIR"
mkdir -p "$DIR"

git -C "$DIR" init -q -b main
git -C "$DIR" config user.email test@example.com
git -C "$DIR" config user.name "Test User"

# base on main
echo base >"$DIR/base.txt"
git -C "$DIR" add base.txt
git -C "$DIR" commit -q -m "base"

# three branches, each forked from main/base and touching its own file
for b in a b c; do
  git -C "$DIR" checkout -q main
  git -C "$DIR" checkout -q -b "$b"
  echo "$b change" >"$DIR/$b.txt"
  git -C "$DIR" add "$b.txt"
  git -C "$DIR" commit -q -m "$b change"
done

# back to main, octopus-merge all three
git -C "$DIR" checkout -q main
git -C "$DIR" merge -q --no-ff a b c -m "octopus merge a b c"

echo "fixture: octopus repo in $DIR"