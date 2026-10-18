use super::*;

/// Parse a Git size value such as `512m`, `1g`, or `1024` into bytes.
pub(super) fn parse_git_size(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let digits_end = value
        .find(|character: char| !character.is_ascii_digit())
        .unwrap_or(value.len());
    let base: u64 = value[..digits_end].parse().ok()?;
    let suffix = value[digits_end..].trim().to_ascii_lowercase();
    let suffix = suffix.strip_suffix('b').unwrap_or(&suffix);
    let multiplier: u64 = match suffix {
        "" => 1,
        "k" => 1024,
        "m" => 1024 * 1024,
        "g" => 1024 * 1024 * 1024,
        _ => return None,
    };
    base.checked_mul(multiplier)
}

pub(super) fn human_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = 1024.0 * 1024.0;
    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let bytes = bytes as f64;
    if bytes >= GIB {
        format!("{:.1} GiB", bytes / GIB)
    } else if bytes >= MIB {
        format!("{:.1} MiB", bytes / MIB)
    } else {
        format!("{:.0} KiB", (bytes / KIB).max(1.0))
    }
}

/// A stand-in "diff" rendered as metadata lines when a file is too large to
/// display. Leading `\` marks a note, which the diff parser keeps out of hunks.
pub(super) fn too_large_diff_notice(path: &GitPath, limit: u64) -> String {
    format!(
        "diff --git a/{path} b/{path}\n\
         \\ File is larger than the {} diff limit; content not loaded.\n\
         \\ Open it in an editor to review. Whole-file staging still works.\n",
        human_size(limit)
    )
}

pub(super) fn load_commit(repo: &gix::Repository, id: &gix::ObjectId) -> Result<Commit> {
    let commit = repo
        .find_commit(*id)
        .with_context(|| format!("find commit {id}"))?;
    let author = commit.author()?;
    let committer = commit.committer()?;
    let message = commit.message()?;
    Ok(Commit {
        id: id.to_string(),
        parents: commit
            .parent_ids()
            .map(|parent| parent.to_string())
            .collect(),
        summary: message.summary().to_string(),
        message: message
            .body()
            .map(|body| body.as_ref().to_string())
            .unwrap_or_default(),
        author_name: author.name.to_string(),
        author_email: author.email.to_string(),
        timestamp: author.time()?.seconds,
        committer_timestamp: committer.time()?.seconds,
        timezone_offset: author.time()?.offset,
        committer_timezone_offset: committer.time()?.offset,
    })
}

pub(super) fn run_git<const N: usize>(workdir: &Path, args: [&str; N]) -> Result<()> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workdir)
        .output()
        .with_context(|| "run git mutation")?;
    if !output.status.success() {
        anyhow::bail!(
            "git command failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(())
}

pub(super) fn git_output(workdir: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(workdir)
        .output()
        .with_context(|| format!("run git {}", args.join(" ")))
}

pub(super) fn git_output_checked(workdir: &Path, args: &[&str]) -> Result<std::process::Output> {
    let output = git_output(workdir, args)?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

pub(super) fn git_output_with_path(
    workdir: &Path,
    args: &[&str],
    path: &GitPath,
) -> Result<std::process::Output> {
    Command::new("git")
        .arg("--literal-pathspecs")
        .args(args)
        .arg(path.to_os_string()?)
        .current_dir(workdir)
        .output()
        .with_context(|| format!("run git {} with path", args.join(" ")))
}

pub(super) fn git_output_with_os_arg(
    workdir: &Path,
    args: &[&str],
    arg: std::ffi::OsString,
) -> Result<std::process::Output> {
    Command::new("git")
        .arg("--literal-pathspecs")
        .args(args)
        .arg(arg)
        .current_dir(workdir)
        .output()
        .with_context(|| format!("run git {} with path argument", args.join(" ")))
}

pub(super) fn git_output_with_input_os_arg(
    workdir: &Path,
    prefix: &[&str],
    arg: std::ffi::OsString,
    suffix: &[&str],
    input: &[u8],
) -> Result<std::process::Output> {
    let mut child = Command::new("git")
        .arg("--literal-pathspecs")
        .args(prefix)
        .arg(arg)
        .args(suffix)
        .current_dir(workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("run git {} with path argument", prefix.join(" ")))?;
    child
        .stdin
        .take()
        .context("open git stdin")?
        .write_all(input)
        .context("write git stdin")?;
    child.wait_with_output().context("wait for git command")
}

pub(super) fn git_output_with_path_checked(
    workdir: &Path,
    args: &[&str],
    path: &GitPath,
) -> Result<std::process::Output> {
    let output = git_output_with_path(workdir, args, path)?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed for {path}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output)
}

pub(super) fn run_git_with_path(workdir: &Path, args: &[&str], path: &GitPath) -> Result<()> {
    let output = git_output_with_path(workdir, args, path)?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed for {path}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

pub(super) fn revision_path(revision_prefix: &str, path: &GitPath) -> Result<std::ffi::OsString> {
    let mut spec = std::ffi::OsString::from(revision_prefix);
    spec.push(path.to_os_string()?);
    Ok(spec)
}

pub(super) fn prefixed_path_arg(prefix: &str, path: &GitPath) -> Result<std::ffi::OsString> {
    let mut arg = std::ffi::OsString::from(prefix);
    arg.push(path.to_os_string()?);
    Ok(arg)
}

pub(super) fn undo_options(workdir: &Path) -> Result<Vec<UndoInfo>> {
    let output = git_output_checked(workdir, &["reflog", "show", "--format=%H%x00%gs", "HEAD"])?;
    let reflog = String::from_utf8_lossy(&output.stdout);
    let entries = reflog
        .lines()
        .filter_map(|line| {
            let (id, message) = line.split_once('\0')?;
            (!id.is_empty()).then_some((id.to_string(), message.to_string()))
        })
        .collect::<Vec<_>>();
    let Some(current) = entries.first() else {
        return Ok(Vec::new());
    };
    let head = git_output_checked(workdir, &["rev-parse", "--verify", "HEAD"])?;
    let head = String::from_utf8_lossy(&head.stdout).trim().to_string();
    if head != current.0 {
        anyhow::bail!("HEAD has changed outside its reflog; refusing to guess what to undo");
    }

    let mut options = Vec::new();
    for depth in 1..entries.len() {
        let target = &entries[depth];
        let restore_branch = entries[..depth]
            .iter()
            .rev()
            .find_map(|(_, message)| checkout_source(message).map(str::to_string));
        let (previous_id, message) = if depth == 1 && current.1.starts_with("rebase (finish):") {
            let original =
                git_output_checked(workdir, &["rev-parse", "--verify", "ORIG_HEAD^{commit}"])?;
            let original = String::from_utf8_lossy(&original.stdout).trim().to_string();
            let original_message = entries
                .iter()
                .find(|(id, _)| id == &original)
                .map(|(_, message)| message.as_str())
                .unwrap_or("previous branch tip");
            (original, format!("pre-rebase: {original_message}"))
        } else {
            (target.0.clone(), target.1.clone())
        };
        options.push(UndoInfo {
            current_id: current.0.clone(),
            previous_id,
            depth,
            message,
            latest_message: current.1.clone(),
            restore_branch,
        });
    }
    Ok(options)
}

pub(super) fn checkout_source(message: &str) -> Option<&str> {
    message
        .strip_prefix("checkout: moving from ")?
        .rsplit_once(" to ")
        .map(|(source, _)| source)
        .filter(|source| !source.is_empty())
}

pub(super) fn has_in_progress_git_operation(workdir: &Path) -> Result<bool> {
    for marker in ["MERGE_HEAD", "CHERRY_PICK_HEAD", "REVERT_HEAD"] {
        let output = git_output_checked(workdir, &["rev-parse", "--git-path", marker])?;
        let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        if workdir.join(path).is_file() {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn extract_diff_hunk(diff: &str, hunk_index: usize) -> Result<Vec<u8>> {
    let lines = diff.split_inclusive('\n').collect::<Vec<_>>();
    let hunks = lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| line.starts_with("@@").then_some(index))
        .collect::<Vec<_>>();
    let Some(&start) = hunks.get(hunk_index) else {
        anyhow::bail!("selected change has no text hunk to stage; use file mode instead");
    };
    let end = hunks.get(hunk_index + 1).copied().unwrap_or(lines.len());
    let mut patch = String::new();
    for line in &lines[..hunks[0]] {
        patch.push_str(line);
    }
    for line in &lines[start..end] {
        patch.push_str(line);
    }
    Ok(patch.into_bytes())
}

pub(super) fn apply_cached_patch(workdir: &Path, patch: &[u8], reverse: bool) -> Result<()> {
    for check in [true, false] {
        let mut command = Command::new("git");
        command
            .args(["apply", "--cached"])
            .current_dir(workdir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if check {
            command.arg("--check");
        }
        if reverse {
            command.arg("--reverse");
        }
        let mut child = command.spawn().context("start git apply --cached")?;
        child
            .stdin
            .take()
            .context("open git apply stdin")?
            .write_all(patch)
            .context("send hunk patch to git apply")?;
        let output = child.wait_with_output().context("wait for git apply")?;
        if !output.status.success() {
            anyhow::bail!(
                "git apply {} failed: {}",
                if check { "--check" } else { "--cached" },
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
    }
    Ok(())
}

pub(super) fn git_output_with_input(
    workdir: &Path,
    args: &[&str],
    input: &[u8],
) -> Result<std::process::Output> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(workdir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("run git {}", args.join(" ")))?;
    child
        .stdin
        .take()
        .context("open git command stdin")?
        .write_all(input)
        .context("write git command input")?;
    child
        .wait_with_output()
        .context("wait for git command with input")
}

pub(super) fn split_inclusive_lines(text: &str) -> Vec<String> {
    text.split_inclusive('\n').map(str::to_string).collect()
}

pub(super) fn diff_line_content(line: &str) -> &str {
    line.strip_suffix('\n')
        .unwrap_or(line)
        .strip_suffix('\r')
        .unwrap_or_else(|| line.strip_suffix('\n').unwrap_or(line))
}

pub(super) fn index_insertion_for_target_line(
    source: &str,
    target: &str,
    target_line: usize,
) -> Result<usize> {
    let diff = TextDiff::from_lines(source, target);
    for op in diff.ops() {
        let old = op.old_range();
        let new = op.new_range();
        if !(new.start..new.end).contains(&target_line) {
            continue;
        }
        let offset = target_line - new.start;
        return match op.tag() {
            DiffTag::Insert => Ok(old.start + offset),
            DiffTag::Replace => {
                let old_len = old.end - old.start;
                if offset < old_len {
                    Ok(old.start + offset + 1)
                } else {
                    Ok(old.end + offset - old_len)
                }
            }
            DiffTag::Equal => Ok(old.start + offset),
            DiffTag::Delete => unreachable!("a delete operation has no target lines"),
        };
    }
    anyhow::bail!("selected line is not present in the target version")
}

pub(super) fn index_insertion_for_source_line(
    source: &str,
    target: &str,
    source_line: usize,
) -> Result<usize> {
    let diff = TextDiff::from_lines(source, target);
    for op in diff.ops() {
        let old = op.old_range();
        let new = op.new_range();
        if !(old.start..old.end).contains(&source_line) {
            continue;
        }
        let offset = source_line - old.start;
        return match op.tag() {
            DiffTag::Delete => Ok(new.start),
            DiffTag::Replace => Ok(new.start + offset.min(new.end - new.start)),
            DiffTag::Equal => Ok(new.start + offset),
            DiffTag::Insert => unreachable!("an insert operation has no source lines"),
        };
    }
    anyhow::bail!("selected line is not present in the source version")
}

pub(super) fn diff_file_pair(workdir: &Path, left: &Path, right: &Path) -> Result<String> {
    let output = Command::new("git")
        .args(["diff", "--no-index", "--no-ext-diff", "--unified=3", "--"])
        .arg(left)
        .arg(right)
        .current_dir(workdir)
        .output()
        .context("compare conflict file stages")?;
    if !output.status.success() && output.status.code() != Some(1) {
        anyhow::bail!(
            "could not compare conflict stages: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

pub(super) fn read_commit_message(workdir: &Path, commit: &str) -> Result<(String, String)> {
    let output = git_output_checked(workdir, &["show", "-s", "--format=%s%x00%b", commit])?;
    let message = String::from_utf8_lossy(&output.stdout);
    let (title, description) = message
        .trim_end()
        .split_once('\0')
        .unwrap_or((&message, ""));
    Ok((title.to_string(), description.trim_end().to_string()))
}

pub(super) fn resolve_commit(workdir: &Path, abbreviated: &str) -> Result<String> {
    let revision = format!("{abbreviated}^{{commit}}");
    let output = git_output_checked(workdir, &["rev-parse", "--verify", &revision])?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub(super) fn parse_rebase_todo(workdir: &Path, todo: &str) -> Result<Vec<RebasePlanBlock>> {
    let mut blocks = Vec::new();
    let mut pending = Vec::new();
    let flush = |blocks: &mut Vec<RebasePlanBlock>, pending: &mut Vec<RebasePlanEntry>| {
        if !pending.is_empty() {
            blocks.push(RebasePlanBlock::Commits(std::mem::take(pending)));
        }
    };

    for line in todo.lines() {
        let command = line.split_whitespace().next().unwrap_or_default();
        if matches!(
            command,
            "pick" | "reword" | "edit" | "squash" | "fixup" | "drop"
        ) {
            let tail = line
                .strip_prefix(command)
                .unwrap_or_default()
                .trim_start()
                .to_string();
            let abbreviated = tail.split_whitespace().next().unwrap_or_default();
            if abbreviated.is_empty() {
                anyhow::bail!("invalid commit command in generated rebase todo");
            }
            let id = resolve_commit(workdir, abbreviated)?;
            let (title, description) = read_commit_message(workdir, &id)?;
            let action = match command {
                "reword" => RebaseTodoAction::Reword,
                "edit" => RebaseTodoAction::Edit,
                "squash" => RebaseTodoAction::Squash,
                "fixup" => RebaseTodoAction::Fixup,
                "drop" => RebaseTodoAction::Drop,
                _ => RebaseTodoAction::Pick,
            };
            pending.push(RebasePlanEntry {
                id,
                subject: title.clone(),
                tail,
                action,
                title,
                description,
            });
        } else if command == "merge" {
            flush(&mut blocks, &mut pending);
            let fields: Vec<&str> = line.split_whitespace().collect();
            if fields.len() < 3 {
                anyhow::bail!("invalid merge command in generated rebase todo");
            }
            let id = resolve_commit(workdir, fields[2])?;
            let (title, description) = read_commit_message(workdir, &id)?;
            blocks.push(RebasePlanBlock::Merge(RebasePlanEntry {
                id,
                subject: title.clone(),
                tail: line.to_string(),
                action: RebaseTodoAction::Merge,
                title,
                description,
            }));
        } else {
            flush(&mut blocks, &mut pending);
            blocks.push(RebasePlanBlock::Control(line.to_string()));
        }
    }
    flush(&mut blocks, &mut pending);
    Ok(blocks)
}

pub(super) fn validate_rebase_plan(plan: &RebasePlan) -> Result<()> {
    let mut keeps_commit = false;
    for block in &plan.blocks {
        match block {
            RebasePlanBlock::Control(_) => {}
            RebasePlanBlock::Merge(_) => keeps_commit = true,
            RebasePlanBlock::Commits(entries) => {
                let mut previous_kept = false;
                for entry in entries {
                    match &entry.action {
                        RebaseTodoAction::Squash | RebaseTodoAction::Fixup if !previous_kept => {
                            anyhow::bail!(
                                "squash/fixup must follow a kept commit in the same sequence"
                            );
                        }
                        RebaseTodoAction::Reword if entry.title.trim().is_empty() => {
                            anyhow::bail!("reword commit messages cannot be empty");
                        }
                        _ => {}
                    }
                    previous_kept = !matches!(&entry.action, RebaseTodoAction::Drop);
                    keeps_commit |= previous_kept;
                }
            }
        }
    }
    if !keeps_commit {
        anyhow::bail!("the rebase plan drops every commit");
    }
    Ok(())
}

pub(super) fn format_commit_message(title: &str, description: &str) -> String {
    if description.trim().is_empty() {
        format!("{}\n", title.trim_end())
    } else {
        format!("{}\n\n{}\n", title.trim_end(), description.trim_end())
    }
}

pub(super) fn commit_author(workdir: &Path, target: &str) -> Result<(String, String, String)> {
    let author = git_output_checked(
        workdir,
        &["show", "-s", "--format=%an%x00%ae%x00%aI", target],
    )?;
    let author = String::from_utf8_lossy(&author.stdout);
    let mut fields = author.trim_end().split('\0');
    Ok((
        fields.next().unwrap_or_default().to_string(),
        fields.next().unwrap_or_default().to_string(),
        fields.next().unwrap_or_default().to_string(),
    ))
}

pub(super) fn rebase_in_progress(workdir: &Path) -> Result<bool> {
    for state in ["rebase-merge", "rebase-apply"] {
        let output = git_output_checked(workdir, &["rev-parse", "--git-path", state])?;
        let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
        let path = if path.is_absolute() {
            path
        } else {
            workdir.join(path)
        };
        if path.exists() {
            return Ok(true);
        }
    }
    Ok(false)
}

pub(super) fn rebase_state_directory(workdir: &Path) -> Result<PathBuf> {
    let output = git_output_checked(workdir, &["rev-parse", "--git-path", "rebase-merge"])?;
    let path = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    Ok(if path.is_absolute() {
        path
    } else {
        workdir.join(path)
    })
}

pub(super) fn shell_quote(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

pub(super) fn parse_status(bytes: &[u8]) -> Result<Vec<StatusEntry>> {
    let fields: Vec<&[u8]> = bytes.split(|b| *b == 0).filter(|f| !f.is_empty()).collect();
    let mut entries = Vec::new();
    let mut i = 0;
    while i < fields.len() {
        let field = fields[i];
        if field.len() < 3 {
            i += 1;
            continue;
        }
        let index = field[0] as char;
        let worktree = field[1] as char;
        let mut path = GitPath::from_bytes(field[3..].to_vec());
        // Porcelain v1 uses a second NUL field for the original path on renames.
        if (index == 'R' || worktree == 'R') && i + 1 < fields.len() {
            path = GitPath::from_bytes(fields[i + 1].to_vec());
            i += 1;
        }
        entries.push(StatusEntry {
            path,
            index,
            worktree,
        });
        i += 1;
    }
    Ok(entries)
}

pub(super) fn parse_oid(id: &str) -> Result<gix::ObjectId> {
    gix::ObjectId::from_hex(id.as_bytes())
        .map_err(|e| anyhow::anyhow!("invalid object id {id}: {e}"))
}

pub(super) fn short_ref_name(full: &gix::bstr::BStr) -> String {
    let s = full.to_string();
    s.strip_prefix("refs/heads/")
        .or_else(|| s.strip_prefix("refs/remotes/"))
        .or_else(|| s.strip_prefix("refs/tags/"))
        .unwrap_or(&s)
        .to_string()
}
