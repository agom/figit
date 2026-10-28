# Releasing Figit

Figit currently ships macOS and Linux binaries for x86-64 and ARM64. The release
workflow builds all four targets, the shell installer, and checksums from a
version tag. Windows is not a supported release target.

Before publishing:

- Update `Cargo.toml`, `Cargo.lock`, and `CHANGELOG.md` together.
- Run formatting, Clippy with warnings denied, and the full serialized test suite.
- Regenerate README screenshots after visible changes and inspect the images.
- Smoke-test the candidate on Linux and macOS: startup, stage/unstage, commit,
  conflict preview/cancel/confirm, and external refresh. Include a linked worktree
  and a terminal without a Nerd Font; icons require a Nerd Font.
- Check slow or rejected hooks leave the UI responsive and preserve commit drafts.

## Isolated installation

Download the published release's `figit-installer.sh`, then install into a
throwaway directory without modifying shell configuration or the normal receipt:

```sh
FIGIT_INSTALL_DIR=/tmp/figit-smoke/bin \
FIGIT_NO_MODIFY_PATH=1 \
XDG_CONFIG_HOME=/tmp/figit-smoke/config \
sh /tmp/figit-installer.sh

/tmp/figit-smoke/bin/figit --version
/tmp/figit-smoke/bin/figit --help
```

Verify the installer receipt under `/tmp/figit-smoke/config/figit` points to the
throwaway binary directory. Launch that exact binary in a disposable repository.

## First stable release

Publish `v0.1.0` as a full release, not a prerelease. After the workflow succeeds:

- Verify all four binary archives, their checksums, and `figit-installer.sh` exist.
- Check `https://figit.dev/install` installs 0.1.0 into an isolated directory.
- With an isolated alpha.6 installation and its matching `XDG_CONFIG_HOME`, use
  `U` to check, then `U` to install. Restart and verify `--version` reports 0.1.0.
- Check the website version badge and release links.

The actual alpha-to-stable upgrade can only be checked after a stable release
exists. Do not repoint an existing alpha tag to simulate it. Future stable
installs use GitHub's latest full release; prereleases do not replace that channel.
