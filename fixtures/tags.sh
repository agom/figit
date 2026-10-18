#!/usr/bin/env bash
# Build a repository with lightweight and annotated tags, plus a detached HEAD.
set -euo pipefail

DIR="${1:?usage: $0 <dir>}"
rm -rf "$DIR"
mkdir -p "$DIR"

git -C "$DIR" init -q -b main
git -C "$DIR" config user.email test@example.com
git -C "$DIR" config user.name "Test User"

for i in 1 2 3; do
  echo "commit $i" >"$DIR/file.txt"
  git -C "$DIR" add file.txt
  git -C "$DIR" commit -q -m "commit $i"
done

git -C "$DIR" tag lightweight-tag
git -C "$DIR" tag -a annotated-tag -m "annotated tag message"

# detach HEAD at the tagged commit
git -C "$DIR" checkout -q --detach

echo "fixture: tags repo (detached HEAD) in $DIR"