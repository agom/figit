# Contributing to figit

Thanks for wanting to help! See here you can find some useful info
on how to contribute.

## Use of AI

There is no formal AI policy. LLM-assisted contributions are welcome, and
you don't need to disclose them, but please keep it tasteful and stand behind
what you submit: it still has to pass CI, and you should be able to explain the
change in review.

## Prerequisites

- Rust (stable) — `rustup` is recommended.
- `git` on `PATH` (figit shells out to it for mutations and remotes; the tests
  need it too).
- Optional: a [Nerd Font](https://www.nerdfonts.com/) for the icons the TUI
  draws, and `rsvg-convert` (librsvg) if you regenerate the README screenshots.

## Build and run

```sh
cargo build
cargo run -- path/to/repo      # or: cargo run -- . for the current repo
```

`figit --help` lists the CLI flags; the README documents the key map.

## Repository layout

```
src/
├── core/      models (Commit, RefLabel, GraphModel) — serde, NO terminal deps
├── backend/   GitBackend trait + GixBackend (gix reads, git CLI writes)
├── graph/     lane assignment + glyph layout (pure, unit-tested)
└── ui/        ratatui renderer, event loop, workflows
site/          figit.dev static site, deployed by Pages
```

Two rules keep the module boundaries useful:

- **Backend policy.** Reads (graph, refs, commit objects) go through `gix`;
  mutations and remotes go through the `git` CLI so authentication is delegated
  to Git and the risky write path stays battle-tested. `gix` types must not leak
  past the `GitBackend` trait.
- **`core` stays terminal-free** and serde-serializable, keyed by OID, so the
  client logic stays reusable and the crate stays split-ready.

## Design guidelines

- **Safety first:** confirm before anything hard to reverse; `Esc` always exits
  transient modes.
- **Conform to git:** honor git config and repository state; don't store app
  state inside git.
- **Discoverable:** disable with a reason rather than hiding actions; keep
  muscle memory intact.
- **Conservative:** be sparing with new config options and dependencies.
- **Explain the why:** comment the reasoning, not the mechanics.

## Before you push

CI runs three checks; make them pass locally:

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
```

Use `cargo fmt --all` to apply formatting, and commit `Cargo.lock` when
dependencies change (CI builds with `--locked`). The PTY tests each spawn a real
terminal and a `git` process, so the harness caps concurrency
(`.cargo/config.toml`) and CI serializes them with `--test-threads=1`.

## Website

`figit.dev` is the static site in `site/`. `.github/workflows/pages.yml` deploys
it when `site/` changes and after each successful release, filling the version
badge from the latest release tag. Preview it locally with
`python3 -m http.server -d site`.

## Tests

- **Unit tests** live next to the code (for example `src/graph/layout.rs` and
  `src/ui/app/tests.rs`).
- **Integration tests** live in `tests/`:
  - `graph_model.rs` — builds a fixture repo and asserts the graph model.
  - `rebase_actions.rs` — drives the rebase planner against fixtures.
  - `tui_pty.rs` — drives the real binary in a PTY and asserts the screen.
- **Fixtures first.** Prefer building test repositories with shell scripts under
  `fixtures/` over setting them up inline. Several suites already do this
  (`graph_model.rs` runs `fixtures/*.sh`; `tui_pty.rs` and `rebase_actions.rs`
  use `fixtures/tui-e2e.sh`), though some still construct small repos inline.
  When adding an end-to-end scenario, add a script under `fixtures/` and invoke
  it from the test (see `tests/graph_model.rs` for an example).
- **Ignored tests** cover measurements and asset generation. Run a specific one
  by name (see [Screenshots](#screenshots) for an example):
  ```sh
  cargo test -- --ignored --nocapture <test-name>
  ```

### Manual testing

See `fixtures/tui-e2e.md` for the shared manual test repository (the same
fixture backs the PTY tests).

### Screenshots

The README images are generated from the real TUI:

```sh
cargo test --test tui_pty -- --ignored --nocapture generate_readme_screenshots
```

This requires `rsvg-convert` (librsvg) and a Nerd Font, plus a `test_repo/`
checkout (see `fixtures/tui-e2e.md` for how to build one). It rewrites
`assets/screenshot*.png`.

## Commits and pull requests

- Write [Conventional Commits](https://www.conventionalcommits.org/)-style
  messages — `feat:`, `fix:`, `docs:`, `test:`, `refactor:`, `perf:`, `chore:`
  — with an imperative summary.
- Keep changes focused and history linear and reviewable; prefer a tidy series
  of commits over one large drop.
- In a pull request, describe the behavior change and how you tested it, and
  link any related issues.

## Reporting

- **Bugs and features:** open an issue with the figit version, your OS and
  terminal, and steps to reproduce. A fixture script or a minimal repository
  helps a lot.
- **Security:** report privately through GitHub's security advisories rather
  than a public issue.

By contributing you agree that your contributions are licensed under the
project's [MIT License](LICENSE).
