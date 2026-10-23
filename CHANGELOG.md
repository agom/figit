# Changelog

## 0.1.0-alpha.2 - 2026-10-23

### Improved

- Full-width diffs below side-by-side file lists when the graph is hidden; historical commits show Changes and Commit Details above the diff.
- Pane maximization with `z`, reverse focus navigation with Shift+Tab, and contextual shortcut hints.
- Neutral selection backgrounds that preserve colored file-type icons, change counts, graph lanes, and rounded branch pills.
- Light and dark terminal presentation, with bold and underlined selection for `NO_COLOR`.
- File-list readability, repository status headers, and visible commit failures that retain the entered message.
- Startup profiling now measures from process start and prints after the terminal is restored.

### Fixed

- Conflict previews interfering with external editor results, premature writes, and missing-side deletion resolutions.
- Stale diffs after repeated external saves and inconsistent graph/details after external history rewrites.
- Scrolled file-list mouse targeting, popup input isolation, and fixup-selection cancellation.
- Unstaging files before the first commit.
- Unified diff source ordering and cursor positioning in wrapped reword descriptions.
