use super::*;
use crate::backend::ConflictCheckpoint;
use std::os::unix::fs::{symlink, PermissionsExt};

impl GixBackend {
    pub(super) fn conflict_checkpoint_impl(&self, path: &GitPath) -> Result<ConflictCheckpoint> {
        let index = git_output_with_path_checked(
            &self.workdir,
            &["ls-files", "--stage", "-z", "--"],
            path,
        )?
        .stdout;
        let head = git_output_checked(&self.workdir, &["rev-parse", "HEAD"])?;
        let file = self.workdir.join(path.to_path_buf()?);
        let metadata = match fs::symlink_metadata(&file) {
            Ok(value) => Some(value),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e.into()),
        };
        let symlink = metadata
            .as_ref()
            .is_some_and(|m| m.file_type().is_symlink());
        let working = if symlink {
            Some(
                fs::read_link(&file)?
                    .as_os_str()
                    .as_encoded_bytes()
                    .to_vec(),
            )
        } else if metadata.is_some() {
            Some(fs::read(&file)?)
        } else {
            None
        };
        let mut context = Vec::new();
        for name in [
            "HEAD",
            "MERGE_HEAD",
            "REBASE_HEAD",
            "CHERRY_PICK_HEAD",
            "REVERT_HEAD",
            "rebase-merge/head-name",
        ] {
            context.extend_from_slice(name.as_bytes());
            context.push(0);
            match fs::read(self.repo.git_dir().join(name)) {
                Ok(bytes) => context.extend(bytes),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
            context.push(0);
        }
        Ok(ConflictCheckpoint {
            context,
            path: path.clone(),
            head: String::from_utf8_lossy(&head.stdout).trim().into(),
            index,
            working,
            symlink,
            mode: metadata.map_or(0, |m| m.permissions().mode()),
        })
    }

    fn restore_checkpoint_file(&self, state: &ConflictCheckpoint) -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        let file = self.workdir.join(state.path.to_path_buf()?);
        if fs::symlink_metadata(&file).is_ok() {
            fs::remove_file(&file)?;
        }
        if let Some(bytes) = &state.working {
            if let Some(parent) = file.parent() {
                fs::create_dir_all(parent)?;
            }
            if state.symlink {
                symlink(std::ffi::OsStr::from_bytes(bytes), &file)?;
            } else {
                fs::write(&file, bytes)?;
                fs::set_permissions(&file, fs::Permissions::from_mode(state.mode))?;
            }
        }
        Ok(())
    }

    pub(super) fn restore_conflict_checkpoint_impl(
        &self,
        before: &ConflictCheckpoint,
        expected: &ConflictCheckpoint,
    ) -> Result<()> {
        anyhow::ensure!(
            before.path == expected.path
                && self.conflict_checkpoint_impl(&expected.path)? == *expected,
            "file, index or HEAD changed since resolution; undo cancelled"
        );
        let mut info = format!("0 {}\t", "0".repeat(before.head.len())).into_bytes();
        info.extend_from_slice(before.path.as_bytes());
        info.push(0);
        info.extend_from_slice(&before.index);
        self.restore_checkpoint_file(before)?;
        let result = git_output_with_input(
            &self.workdir,
            &["update-index", "-z", "--index-info"],
            &info,
        )?;
        if !result.status.success() {
            self.restore_checkpoint_file(expected)?;
            anyhow::bail!(
                "could not restore conflict index: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        Ok(())
    }
}
