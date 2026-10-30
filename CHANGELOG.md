# Changelog

## 0.1.3 - 2026-10-30

### Improved

- Show the installed version in the footer and the available version when an update is found; check for updates at startup and hourly.
- Show aggregate added and removed line counts in staging panels and commit details.
- Preserve syntax highlighting while horizontally scrolling diffs.

## 0.1.2 - 2026-10-30

### Improved

- Check out remote-tracking branches from the graph by creating a local tracking branch.

## 0.1.1 - 2026-10-30

### Improved

- Navigate staged and unstaged hunk selections with `j`/`k` or the arrow keys, and discard individual unstaged hunks or lines from the diff.
- Return focus to the commit graph after completed graph and rebase operations.
- Refresh visible diffs after rapid external saves, including on macOS filesystems that do not reliably deliver native change events.
- Prune deleted remote-tracking branches when fetching.

### Fixed

- Preserve color assertions in PTY tests regardless of the caller's `NO_COLOR` environment.
- Keep file and line selection navigation separate in the contextual command picker.

## 0.1.0 - 2026-10-28

### Highlights

- First stable release, with prebuilt binaries for macOS and Linux on x86-64 and ARM64.
- Browse the commit graph, stage files/hunks/lines, manage branches/tags/stashes and remotes, plan interactive rebases, and resolve conflicts in the aligned three-way view.

### Improved

- Avoid idle redraws; repaint for input, background results, resize, and active operation timers.
- Run commits, rebases, and graph mutations in a serialized background worker, keeping navigation responsive during hooks and signing.
- Derive panel shortcuts and normal keyboard dispatch from the command catalog, retaining existing context-sensitive aliases.
- Organize application state and handlers by feature, with dedicated diff, conflict, graph, terminal, rebase, refresh, and update modules.
- Clarify that commit search covers loaded titles and descriptions, and document supported platforms and release verification.

### Fixed

- Pan ordinary unified and side-by-side diffs by terminal display columns, including partially visible wide glyphs and fixed side-by-side alignment.
- Omit staging commands while a selected commit's diff is still loading.

## 0.1.0-alpha.6 - 2026-10-26

### Improved

- Open a fuzzy command picker with `:` for the focused panel, with commands adapted to the current selection and repository state.
- Prioritize Fetch, Pull, Checkout, and Search in graph shortcuts, and simplify the commit search bar and navigation hints.
- Render only visible conflict rows with cached syntax highlighting, keeping large three-way merges responsive while scrolling.
- Align Ours, Theirs, and Result on one shared row grid, with synchronized scrolling, fixed line-number gutters, and active-conflict highlighting.
- Keep file lists compact during conflict resolution, and clarify conflict previews, confirmation, cancellation, and whole-file Base restoration.

### Fixed

- Jump to the selected conflict when navigating hunks, and consistently clamp keyboard, page, and mouse scrolling while preserving wheel distance.
- Apply horizontal panning to all three conflict panes, including result previews and wide characters.
- Keep local resolver commands available while inspecting historical commits, and focus the resolver when opening a repository with conflicts.
- Preserve preview safety checks for external edits, missing-file deletions, and partially resolved files.

## 0.1.0-alpha.5 - 2026-10-26

### Improved

- Show up to three contextual shortcuts in the focused panel's bottom border, with no additional content rows.
- Toggle panel shortcuts with Shift+H for the current session; hints start visible on each launch.
- Adapt shortcuts to staging, diff selection modes, historical commits, and conflicts, omitting unavailable actions and j/k navigation hints.
- Fit whole shortcut items to narrow panels while preserving right-aligned diff scope and mode labels.
- Simplify the global shortcut bar and document the panel-hint toggle in help.

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
