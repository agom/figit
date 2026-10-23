# Changelog

## 0.1.0-alpha.4 - 2026-10-23

### Fixed

- Automatically fall back to background polling every five seconds when filesystem watcher setup fails, including when Linux file watch limits are reached.
- Catch changes missed during failed watcher setup and continue detecting external file and index changes without overlapping refresh workers or interrupting commit editing.
- Show a concise polling status while keeping refresh and action failures visible.

## 0.1.0-alpha.3 - 2026-10-23

### Improved

- Render the initial screen before registering filesystem watchers, keeping startup responsive in large repositories on Linux.
- Avoid redundant recursive watches and traversal of symlinked directories; refresh once registration completes to catch changes made during setup.
- Use vertical tracking arrows and Nerd Font tag and ahead/behind indicators in branch pills.

### Fixed

- Unreadable branch-pill indicators and text after dimming, with explicit contrasting foreground colors across terminal themes.
- Local changes failing to refresh after external staging and unstaging.
- New untracked directories appearing as a single folder instead of individually selectable files.
- Read-only status checks unnecessarily updating the Git index.
- Watcher setup errors disappearing after successful repository refreshes.

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
