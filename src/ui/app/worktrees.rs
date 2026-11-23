use super::*;
use crate::backend::worktrees::{self, Source, Worktree};

pub(super) enum Dialog {
    List {
        trees: Vec<Worktree>,
        selected: usize,
    },
    Create {
        source: Source,
        branch: String,
        path: String,
        editing_branch: bool,
    },
}

impl App {
    pub(super) fn show_worktrees(&mut self) {
        let Some(root) = self.repository_path.as_deref() else {
            return;
        };
        match worktrees::list(root) {
            Ok(trees) => {
                let selected = trees.iter().position(|t| t.path == root).unwrap_or(0);
                self.worktrees = Some(Dialog::List { trees, selected });
            }
            Err(e) => self.show_git_error(e.to_string()),
        }
    }

    pub(super) fn offer_occupied_worktree(&mut self, branch: &str) -> bool {
        let Some(root) = self.repository_path.as_deref() else {
            return false;
        };
        if self.graph.head_name.as_deref() == Some(branch) {
            return false;
        }
        if let Ok(trees) = worktrees::list(root) {
            if let Some(selected) = trees
                .iter()
                .position(|t| t.branch.as_deref() == Some(branch) && t.path != root)
            {
                self.worktrees = Some(Dialog::List { trees, selected });
                return true;
            }
        }
        false
    }

    pub(super) fn worktree_from_graph(&mut self) {
        let Some(id) = self.selected_commit_id() else {
            return;
        };
        let source = if let Some(name) = self.selected_local_branch() {
            if self.offer_occupied_worktree(&name) {
                return;
            }
            // A second checkout at the current tip needs its own branch or a detached HEAD.
            if self.graph.head_name.as_deref() == Some(&name) {
                Source::Commit(id)
            } else {
                Source::Local(name)
            }
        } else if let Some(name) = self.selected_remote_branch() {
            Source::Remote(name)
        } else {
            Source::Commit(id)
        };
        self.worktree_form(source);
    }

    fn worktree_form(&mut self, source: Source) {
        let Some(root) = self.repository_path.as_deref() else {
            return;
        };
        let branch = match &source {
            Source::Remote(name) => name
                .split_once('/')
                .map_or(name.as_str(), |(_, b)| b)
                .to_owned(),
            _ => String::new(),
        };
        let suffix = match &source {
            Source::Local(name) => name.replace('/', "-"),
            Source::Remote(_) => branch.replace('/', "-"),
            Source::Commit(id) => id.chars().take(8).collect(),
        };
        let path = root
            .with_file_name(format!(
                "{}-{suffix}",
                root.file_name().unwrap_or_default().to_string_lossy()
            ))
            .display()
            .to_string();
        let editing_branch = !matches!(source, Source::Local(_));
        self.worktrees = Some(Dialog::Create {
            source,
            branch,
            path,
            editing_branch,
        });
    }

    fn switch_worktree(&mut self, path: PathBuf) {
        if self.git_busy() {
            return;
        }
        let opened = (|| -> anyhow::Result<_> {
            let path = path.canonicalize()?;
            let backend = crate::backend::open(&path)?;
            anyhow::ensure!(
                backend.working_directory().canonicalize()? == path,
                "Worktree directory has moved"
            );
            let trees = worktrees::list(&path)?;
            let original = self.repository_path.as_ref().unwrap().canonicalize()?;
            anyhow::ensure!(
                trees.iter().any(|t| t.path == original)
                    && trees.iter().any(|t| t.path == path && !t.unavailable),
                "Worktree registration has changed"
            );
            let label = path
                .ancestors()
                .skip(1)
                .filter_map(|parent| path.strip_prefix(parent).ok())
                .find(|suffix| trees.iter().filter(|t| t.path.ends_with(suffix)).count() == 1)
                .unwrap_or(&path)
                .display()
                .to_string();
            Ok((backend, label))
        })();
        let (backend, label) = match opened {
            Ok(result) => result,
            Err(error) => {
                self.show_git_error(format!(
                    "Cannot open worktree at {}: {error}",
                    path.display()
                ));
                return;
            }
        };
        self.stop_auto_fetch();
        let mut next = Self::loading(backend);
        next.worktree_label = Some(label);
        next.bg = self.bg;
        next.terminal_palette = self.terminal_palette;
        next.no_color = self.no_color;
        next.plain_icons = self.plain_icons;
        next.panel_hints_visible = self.panel_hints_visible;
        next.git_gate = self.git_gate.clone();
        // Application updates belong to the process, not to a checkout.
        std::mem::swap(&mut next.update_rx, &mut self.update_rx);
        std::mem::swap(&mut next.update_tx, &mut self.update_tx);
        next.update_status = self.update_status.clone();
        next.update_status_since = self.update_status_since;
        next.update_check_deadline = self.update_check_deadline;
        next.diff_loader = Some(diff_loader::DiffLoader::new());
        next.refresh.deadline = Some(Instant::now());
        next.auto_fetch.deadline = Some(Instant::now() + Duration::from_secs(3));
        next.start_repository_watcher();
        next.redraw = true;
        *self = next;
    }

    pub(super) fn worktree_key(&mut self, key: KeyEvent) {
        if key.code == KeyCode::Esc {
            self.worktrees = None;
            return;
        }
        let Some(dialog) = self.worktrees.as_mut() else {
            return;
        };
        match dialog {
            Dialog::List { trees, selected } => match key.code {
                KeyCode::Up | KeyCode::Char('k') => *selected = selected.saturating_sub(1),
                KeyCode::Down | KeyCode::Char('j') => {
                    *selected = (*selected + 1).min(trees.len().saturating_sub(1))
                }
                KeyCode::Char('n') => {
                    if let Some(id) = self.graph.head_id.clone() {
                        self.worktree_form(Source::Commit(id));
                    } else {
                        self.worktrees = None;
                        self.show_git_error(
                            "Create a first commit before adding a worktree.".into(),
                        );
                    }
                }
                KeyCode::Enter => {
                    if let Some(tree) = trees.get(*selected) {
                        let path = tree.path.clone();
                        let unavailable = tree.unavailable;
                        self.worktrees = None;
                        if unavailable {
                            self.show_git_error(format!(
                                "Worktree unavailable: {}",
                                path.display()
                            ));
                        } else if self.repository_path.as_ref() != Some(&path) {
                            self.switch_worktree(path);
                        }
                    }
                }
                _ => {}
            },
            Dialog::Create {
                source,
                branch,
                path,
                editing_branch,
            } => {
                let field = if *editing_branch {
                    &mut *branch
                } else {
                    &mut *path
                };
                match key.code {
                    KeyCode::Tab | KeyCode::BackTab if !matches!(source, Source::Local(_)) => {
                        *editing_branch = !*editing_branch
                    }
                    KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        field.clear()
                    }
                    KeyCode::Char(c)
                        if !key
                            .modifiers
                            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
                    {
                        field.push(c)
                    }
                    KeyCode::Backspace => {
                        field.pop();
                    }
                    KeyCode::Enter if !path.is_empty() => {
                        let Some(Dialog::Create {
                            source,
                            branch,
                            path,
                            ..
                        }) = self.worktrees.take()
                        else {
                            return;
                        };
                        let target = self.repository_path.as_ref().unwrap().join(path);
                        self.start_local_job(
                            "Create worktree",
                            move |backend| {
                                worktrees::create(
                                    backend.working_directory(),
                                    &source,
                                    &branch,
                                    &target,
                                )?;
                                Ok(target)
                            },
                            |app, job| match job.result {
                                Ok(target) => app.switch_worktree(target),
                                Err(e) => app.show_git_error(e),
                            },
                        );
                    }
                    _ => {}
                }
            }
        }
    }

    pub(super) fn render_worktrees(&self, f: &mut Frame) {
        let Some(dialog) = &self.worktrees else {
            return;
        };
        let area = f.area();
        let w = area.width.saturating_sub(4).min(100);
        let h = match dialog {
            Dialog::List { trees, .. } => trees.len().saturating_add(3).min(20) as u16,
            Dialog::Create { .. } => 8,
        }
        .min(area.height);
        let rect = Rect::new((area.width - w) / 2, (area.height - h) / 2, w, h);
        let (title, footer, lines) = match dialog {
            Dialog::List { trees, selected } => {
                let count = h.saturating_sub(2) as usize;
                let start = (selected + 1).saturating_sub(count);
                let lines = trees
                    .iter()
                    .enumerate()
                    .skip(start)
                    .take(count)
                    .map(|(i, t)| {
                        let current = self.repository_path.as_ref() == Some(&t.path);
                        let name = t
                            .branch
                            .clone()
                            .unwrap_or_else(|| format!("detached {}", t.head));
                        Line::styled(
                            format!(
                                "{} {}{}{}  {}",
                                if i == *selected { "▸" } else { " " },
                                name,
                                if current { " · current" } else { "" },
                                if t.unavailable { " · unavailable" } else { "" },
                                t.path.display()
                            ),
                            if i == *selected {
                                Style::default().add_modifier(Modifier::REVERSED)
                            } else {
                                Style::default()
                            },
                        )
                    })
                    .collect();
                ("Worktrees", "Enter open · n new · Esc close", lines)
            }
            Dialog::Create {
                source,
                branch,
                path,
                editing_branch,
            } => {
                let choice = match source {
                    Source::Local(b) => format!("Existing branch: {b}"),
                    _ => format!(
                        "New branch: {}{}",
                        if *editing_branch { "▸ " } else { "" },
                        if branch.is_empty() {
                            "(detached)"
                        } else {
                            branch
                        }
                    ),
                };
                (
                    "New worktree",
                    "Tab field · Ctrl+U clear · Enter create · Esc cancel",
                    vec![
                        Line::from(format!("From: {}", source.name())),
                        Line::from(choice),
                        Line::from(format!(
                            "Directory: {}{path}",
                            if *editing_branch { "" } else { "▸ " }
                        )),
                    ],
                )
            }
        };
        f.render_widget(Clear, rect);
        let mut content = Paragraph::new(lines);
        if matches!(dialog, Dialog::Create { .. }) {
            content = content.wrap(Wrap { trim: false });
        }
        f.render_widget(
            content.block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(title)
                    .title_bottom(footer),
            ),
            rect,
        );
    }
}
