#!/usr/bin/env bash
set -euo pipefail

if [[ $# -gt 1 ]]; then
    echo "usage: $0 [empty-repository-directory]" >&2
    exit 2
fi

repo=${1:-test_repo}
mkdir -p "$repo"

commit_fixture() {
    local message=$1
    local date=$2
    local body=${3:-}
    if [[ -n "$body" ]]; then
        GIT_AUTHOR_DATE="$date" \
        GIT_COMMITTER_DATE="$date" \
            git -C "$repo" commit -q -m "$message" -m "$body"
    else
        GIT_AUTHOR_DATE="$date" \
        GIT_COMMITTER_DATE="$date" \
            git -C "$repo" commit -q -m "$message"
    fi
}

build_history() {
    for branch in feature/lorem feature/ipsum release/dolor; do
        if git -C "$repo" show-ref --verify --quiet "refs/heads/$branch"; then
            git -C "$repo" branch -D "$branch" >/dev/null
        fi
    done

    git -C "$repo" checkout -q --force -B main refs/tags/tui-e2e-seed
    printf 'application settings\n' > "$repo/app.conf"
    git -C "$repo" add app.conf
    commit_fixture "consectetur adipiscing" "2024-01-02T00:00:00Z"

    git -C "$repo" checkout -q -b feature/lorem
    printf 'graph view v1\n' > "$repo/graph.txt"
    git -C "$repo" add graph.txt
    commit_fixture "sed do eiusmod" "2024-01-03T00:00:00Z"
    printf 'graph view v2\nthin lanes\n' > "$repo/graph.txt"
    git -C "$repo" add graph.txt
    commit_fixture "tempor incididunt" "2024-01-04T00:00:00Z"

    git -C "$repo" checkout -q main
    printf '# Figit test repository\n' > "$repo/README.md"
    git -C "$repo" add README.md
    commit_fixture "ut labore et dolore" "2024-01-05T00:00:00Z"
    GIT_AUTHOR_DATE="2024-01-06T00:00:00Z" \
    GIT_COMMITTER_DATE="2024-01-06T00:00:00Z" \
        git -C "$repo" merge -q --no-ff -m "merge magna aliqua" feature/lorem

    git -C "$repo" checkout -q -b feature/ipsum
    printf 'stage selected files\n' > "$repo/staging.txt"
    git -C "$repo" add staging.txt
    commit_fixture "ut enim ad minim" "2024-01-07T00:00:00Z"
    printf 'stage and unstage selected files\n' > "$repo/staging.txt"
    git -C "$repo" add staging.txt
    commit_fixture "quis nostrud exercitation" "2024-01-08T00:00:00Z"

    git -C "$repo" checkout -q main
    printf 'command line startup\n' > "$repo/cli.txt"
    git -C "$repo" add cli.txt
    commit_fixture "ullamco laboris" "2024-01-09T00:00:00Z"
    printf 'Split-edit fixture notes\n' > "$repo/CHANGELOG.md"
    printf 'Deploy preview enabled\n' > "$repo/deploy.txt"
    mkdir -p "$repo/src"
    cat > "$repo/src/deploy.rs" <<'EOF'
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeployStatus {
    Ready,
    Blocked,
}

pub fn can_deploy(status: DeployStatus) -> bool {
    status == DeployStatus::Ready
}
EOF
    cat > "$repo/src/rollout.rs" <<'EOF'
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RolloutPolicy {
    pub max_unavailable: usize,
    pub minimum_healthy: usize,
}

impl RolloutPolicy {
    pub fn can_advance(&self, healthy: usize, total: usize) -> bool {
        healthy >= self.minimum_healthy
            && total.saturating_sub(healthy) <= self.max_unavailable
    }
}
EOF
    cat > "$repo/src/main.rs" <<'EOF'
mod deploy;
mod rollout;

fn main() {
    let status = deploy::DeployStatus::Ready;
    println!("deploy allowed: {}", deploy::can_deploy(status));

    let policy = rollout::RolloutPolicy {
        max_unavailable: 1,
        minimum_healthy: 2,
    };
    println!("rollout can advance: {}", policy.can_advance(3, 3));
}
EOF
    git -C "$repo" add CHANGELOG.md deploy.txt
    git -C "$repo" add src/deploy.rs src/main.rs src/rollout.rs
    commit_fixture "nisi ut aliquip" "2024-01-10T00:00:00Z"
    cat > "$repo/src/deploy.rs" <<'EOF'
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeployStatus {
    Ready,
    Preview,
    Blocked,
}

pub fn can_deploy(status: DeployStatus) -> bool {
    matches!(status, DeployStatus::Ready | DeployStatus::Preview)
}
EOF
    git -C "$repo" add src/deploy.rs
    commit_fixture "ex ea commodo" "2024-01-10T06:00:00Z"
    cat > "$repo/src/deploy.rs" <<'EOF'
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeployStatus {
    Ready,
    Preview,
    Blocked,
}

pub fn can_deploy(status: DeployStatus, approved: bool) -> bool {
    approved && matches!(status, DeployStatus::Ready | DeployStatus::Preview)
}
EOF
    cat > "$repo/src/main.rs" <<'EOF'
mod deploy;

fn main() {
    let status = deploy::DeployStatus::Ready;
    let approved = true;
    println!("deploy allowed: {}", deploy::can_deploy(status, approved));
}
EOF
    git -C "$repo" add src/deploy.rs src/main.rs
    commit_fixture "duis aute irure" "2024-01-10T12:00:00Z"
    cat > "$repo/src/rollout.rs" <<'EOF'
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RolloutPolicy {
    pub max_unavailable_percent: u8,
    pub minimum_healthy_percent: u8,
}

impl RolloutPolicy {
    pub fn can_advance(&self, healthy: usize, total: usize) -> bool {
        if total == 0 || healthy > total {
            return false;
        }

        let unavailable_percent = (total - healthy) * 100 / total;
        let healthy_percent = healthy * 100 / total;
        unavailable_percent <= usize::from(self.max_unavailable_percent)
            && healthy_percent >= usize::from(self.minimum_healthy_percent)
    }
}
EOF
    cat > "$repo/src/main.rs" <<'EOF'
mod deploy;
mod rollout;

fn main() {
    let status = deploy::DeployStatus::Ready;
    let approved = true;
    println!("deploy allowed: {}", deploy::can_deploy(status, approved));

    let policy = rollout::RolloutPolicy {
        max_unavailable_percent: 20,
        minimum_healthy_percent: 80,
    };
    println!("rollout can advance: {}", policy.can_advance(5, 5));
}
EOF
    git -C "$repo" add src/rollout.rs src/main.rs
    commit_fixture "in reprehenderit" "2024-01-10T15:00:00Z" "At vero eos et accusamus et iusto odio dignissimos ducimus qui blanditiis praesentium."
    cat > "$repo/src/rollout.rs" <<'EOF'
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RolloutPolicy {
    pub max_unavailable_percent: u8,
    pub minimum_healthy_percent: u8,
    pub minimum_healthy_seconds: u64,
}

impl RolloutPolicy {
    pub fn can_advance(&self, healthy: usize, total: usize, healthy_for_seconds: u64) -> bool {
        if total == 0 || healthy > total {
            return false;
        }

        let unavailable_percent = (total - healthy) * 100 / total;
        let healthy_percent = healthy * 100 / total;
        unavailable_percent <= usize::from(self.max_unavailable_percent)
            && healthy_percent >= usize::from(self.minimum_healthy_percent)
            && healthy_for_seconds >= self.minimum_healthy_seconds
    }
}
EOF
    cat > "$repo/src/main.rs" <<'EOF'
mod deploy;
mod rollout;

fn main() {
    let status = deploy::DeployStatus::Ready;
    let approved = true;
    println!("deploy allowed: {}", deploy::can_deploy(status, approved));

    let policy = rollout::RolloutPolicy {
        max_unavailable_percent: 20,
        minimum_healthy_percent: 80,
        minimum_healthy_seconds: 300,
    };
    println!("rollout can advance: {}", policy.can_advance(5, 5, 300));
}
EOF
    git -C "$repo" add src/rollout.rs src/main.rs
    commit_fixture "voluptate velit" "2024-01-10T18:00:00Z" "At vero eos et accusamus et iusto odio dignissimos ducimus qui blanditiis praesentium."
    GIT_AUTHOR_DATE="2024-01-10T18:00:00Z" \
    GIT_COMMITTER_DATE="2024-01-10T18:00:00Z" \
        git -C "$repo" merge -q --no-ff -m "merge esse cillum" feature/ipsum

    git -C "$repo" checkout -q -b release/dolor HEAD~4
    printf 'maintenance fixes\n' > "$repo/maintenance.txt"
    git -C "$repo" add maintenance.txt
    commit_fixture "dolore eu fugiat" "2024-01-11T00:00:00Z"
    printf 'maintenance fixes\ncompatibility updates\n' > "$repo/maintenance.txt"
    git -C "$repo" add maintenance.txt
    commit_fixture "nulla pariatur" "2024-01-12T00:00:00Z"

    git -C "$repo" checkout -q main
    git -C "$repo" tag -f tui-e2e-base >/dev/null
    git -C "$repo" config figit.fixture.version 12
}

prepare_seed() {
    git -C "$repo" checkout -q --force --detach refs/tags/tui-e2e-seed
    cat > "$repo/a-stage.txt" <<'EOF'
first base
context 1
context 2
context 3
context 4
context 5
context 6
context 7
context 8
context 9
context 10
second base
context 11
context 12
context 13
context 14
context 15
context 16
context 17
context 18
context 19
context 20
third base
EOF
    git -C "$repo" add a-stage.txt
    if ! git -C "$repo" diff --cached --quiet; then
        GIT_AUTHOR_DATE="2024-01-01T00:00:00Z" \
        GIT_COMMITTER_DATE="2024-01-01T00:00:00Z" \
            git -C "$repo" commit -q --amend --no-edit
        git -C "$repo" tag -f tui-e2e-seed HEAD >/dev/null
    fi
}

if git -C "$repo" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    marker=$(git -C "$repo" config --local --get figit.fixture || true)
    if [[ "$marker" != "tui-e2e" ]]; then
        echo "refusing to reset a repository not marked as the figit TUI fixture: $repo" >&2
        exit 1
    fi
    if ! git -C "$repo" rev-parse --verify --quiet refs/tags/tui-e2e-base >/dev/null; then
        echo "TUI fixture is missing refs/tags/tui-e2e-base: $repo" >&2
        exit 1
    fi
    # A clone does not inherit the source's identity, and CI runners may have
    # none configured, so set one before any commits below.
    git -C "$repo" config user.name "TUI Test"
    git -C "$repo" config user.email "tui-test@example.invalid"

    git -C "$repo" checkout -q --force -B main refs/tags/tui-e2e-base
    git -C "$repo" reset -q --hard refs/tags/tui-e2e-base
    if git -C "$repo" show-ref --verify --quiet refs/heads/e2e-q-branch; then
        git -C "$repo" branch -D e2e-q-branch >/dev/null
    fi
    for branch in feature/lorem feature/ipsum release/dolor; do
        if ! git -C "$repo" show-ref --verify --quiet "refs/heads/$branch" \
            && git -C "$repo" show-ref --verify --quiet "refs/remotes/origin/$branch"; then
            git -C "$repo" branch --quiet "$branch" "origin/$branch"
        fi
    done
    git -C "$repo" clean -f -- z-discard.txt >/dev/null
    version=$(git -C "$repo" config --local --get figit.fixture.version || true)
    if [[ "$version" != "12" ]]; then
        if ! git -C "$repo" show-ref --verify --quiet refs/tags/tui-e2e-seed; then
            seed=$(git -C "$repo" rev-list --max-parents=0 refs/tags/tui-e2e-base)
            git -C "$repo" tag tui-e2e-seed "$seed"
        fi
        prepare_seed
        build_history
    fi
else
    if [[ -n "$(find "$repo" -mindepth 1 -maxdepth 1 -print -quit)" ]]; then
        echo "refusing to populate non-empty directory: $repo" >&2
        exit 1
    fi

    git -C "$repo" init --initial-branch=main -q
    git -C "$repo" config user.name "TUI Test"
    git -C "$repo" config user.email "tui-test@example.invalid"
    git -C "$repo" config figit.fixture tui-e2e

    cat > "$repo/a-stage.txt" <<'EOF'
first base
context 1
context 2
context 3
context 4
context 5
context 6
context 7
context 8
context 9
context 10
second base
context 11
context 12
context 13
context 14
context 15
context 16
context 17
context 18
context 19
context 20
third base
EOF
    git -C "$repo" add .
    commit_fixture "lorem ipsum dolor" "2024-01-01T00:00:00Z"
    git -C "$repo" tag tui-e2e-seed
    build_history
fi

cat > "$repo/a-stage.txt" <<'EOF'
first changed
context 1
context 2
context 3
context 4
context 5
context 6
context 7
context 8
context 9
context 10
second base
context 11
context 12
context 13
context 14
context 15
context 16
context 17
context 18
context 19
context 20
third changed
EOF
printf 'discard me\n' > "$repo/z-discard.txt"
