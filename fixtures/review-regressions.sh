#!/usr/bin/env bash
set -euo pipefail

repo=$1
scenario=$2
mkdir -p "$repo"
git -C "$repo" init -q -b main
git -C "$repo" config user.name 'Figit Test'
git -C "$repo" config user.email 'figit@example.com'
git -C "$repo" config commit.gpgsign false
git -C "$repo" config core.autocrlf false

if [[ "$scenario" == unborn ]]; then
    printf 'staged\n' > "$repo/file.txt"
    git -C "$repo" add file.txt
    printf 'edited after staging\n' > "$repo/file.txt"
    exit
fi

printf 'base\n' > "$repo/file.txt"
git -C "$repo" add file.txt
git -C "$repo" commit -qm base

case "$scenario" in
    content)
        for i in {1..60}; do printf 'changed line %s\n' "$i"; done > "$repo/file.txt"
        ;;
    history)
        printf 'old-history-content\n' > "$repo/file.txt"
        printf 'old-only\n' > "$repo/old-only.txt"
        git -C "$repo" add .
        git -C "$repo" commit -qm 'selected commit'
        ;;
    conflict|modify-delete|delete-modify)
        git -C "$repo" checkout -qb feature
        if [[ "$scenario" == modify-delete ]]; then
            git -C "$repo" rm -q file.txt
        else
            printf 'theirs\n' > "$repo/file.txt"
            git -C "$repo" add file.txt
        fi
        git -C "$repo" commit -qm theirs
        git -C "$repo" checkout -q main
        if [[ "$scenario" == delete-modify ]]; then
            git -C "$repo" rm -q file.txt
        else
            printf 'ours\n' > "$repo/file.txt"
            git -C "$repo" add file.txt
        fi
        git -C "$repo" commit -qm ours
        if git -C "$repo" merge --no-edit feature >/dev/null 2>&1; then
            echo 'expected a merge conflict' >&2
            exit 1
        fi
        ;;
    *) echo "unknown scenario: $scenario" >&2; exit 1 ;;
esac
