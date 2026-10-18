#!/usr/bin/env bash
# Build a repository with a linear history of N commits.
set -euo pipefail

DIR="${1:?usage: $0 <dir>}"
N="${2:-5}"
rm -rf "$DIR"
mkdir -p "$DIR"

git -C "$DIR" init -q -b main
git -C "$DIR" config user.email test@example.com
git -C "$DIR" config user.name "Test User"

for i in $(seq 1 "$N"); do
  echo "commit $i" >"$DIR/file.txt"
  git -C "$DIR" add file.txt
  git -C "$DIR" commit -q -m "commit $i"
done

echo "fixture: linear repo with $N commits in $DIR"