#!/usr/bin/env bash
# Build a repository with two branches that are merged with a merge commit.
set -euo pipefail

DIR="${1:?usage: $0 <dir>}"
rm -rf "$DIR"
mkdir -p "$DIR"

git -C "$DIR" init -q -b main
git -C "$DIR" config user.email test@example.com
git -C "$DIR" config user.name "Test User"

# base commits on main
echo base >"$DIR/main.txt"
git -C "$DIR" add main.txt
git -C "$DIR" commit -q -m "base"
git -C "$DIR" checkout -q -b feature
git -C "$DIR" checkout -q main

echo "main one" >>"$DIR/main.txt"
git -C "$DIR" add main.txt
git -C "$DIR" commit -q -m "main one"

git -C "$DIR" checkout -q feature
echo "feature one" >>"$DIR/feature.txt"
git -C "$DIR" add feature.txt
git -C "$DIR" commit -q -m "feature one"
echo "feature two" >>"$DIR/feature.txt"
git -C "$DIR" add feature.txt
git -C "$DIR" commit -q -m "feature two"

git -C "$DIR" checkout -q main
echo "main two" >>"$DIR/main.txt"
git -C "$DIR" add main.txt
git -C "$DIR" commit -q -m "main two"

git -C "$DIR" merge -q --no-ff feature -m "merge feature"

echo "fixture: branch-merge repo in $DIR"