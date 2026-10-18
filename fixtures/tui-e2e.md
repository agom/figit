# TUI end-to-end fixture

The PTY integration test and manual testing use the same repository builder, `tui-e2e.sh`.
It creates one tracked file with two separated hunks and one untracked file so hunk
staging and discard workflows are ready to exercise as soon as the app opens.
Space stages the selected file by default. Press `h` to enter hunk mode (`[` / `]`,
then Space), or `l` to enter line mode (`j` / `k`, then Space). Press `F` to stage the
whole file from either partial mode. The same controls work in reverse from Staged.
File rows show the current section's hunk count over the staged-plus-unstaged total
immediately before the `+/-` line stats, for example `1/3` and `2/3`.
The baseline history has two merged feature branches and a divergent `release/dolor`
branch, so the Graph view includes several commits and merge nodes. The mainline also
has a commit that changes two files, which is useful for testing edit-and-split workflows.
Those files are independent of the preceding command-line commit, so the planner can
reorder the two commits without creating an artificial patch conflict. Two subsequent
commits both change `src/deploy.rs`, providing a pair whose reordering produces a realistic code conflict.
The Rust example also has a rollout policy that grows from absolute to percentage
thresholds and then adds a health-stability window; reordering those latter commits
conflicts in `src/rollout.rs` (and can also expose the dependent call-site edit).

To create or reset the local `test_repo/` checkout to its known base commit and launch the app:

```sh
bash fixtures/tui-e2e.sh
cargo run -- test_repo
```

The working-tree change in `a-stage.txt` has two separated hunks for staging and
unstaging tests. Use `s` to open the stash manager; `u` toggles whether untracked
files are included when saving a stash.

The builder creates and tags the baseline commit as `tui-e2e-base`. On later runs, it
resets only repositories carrying its `figit.fixture=tui-e2e` marker, removes the
known test-created branch, and restores the staged/unstaged files. It refuses to reset
unmarked repositories. The local `test_repo/` directory is ignored by the outer
repository because it contains its own `.git`. The PTY test uses a temporary repository
created by the same builder, so test runs never mutate the manual checkout.
