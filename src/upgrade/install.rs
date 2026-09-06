use std::ffi::OsString;
use std::fs::{self, File, Metadata, OpenOptions};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use anyhow::{Context, Result, ensure};
use semver::Version;
use tempfile::TempPath;

use super::process;

pub(super) struct Destination {
    pub path: PathBuf,
    identity: Identity,
    parent_identity: (u64, u64),
}

#[derive(PartialEq, Eq)]
struct Identity {
    dev: u64,
    ino: u64,
    len: u64,
    modified: (i64, i64),
    changed: (i64, i64),
    mode: u32,
}

impl Identity {
    fn read(path: &Path) -> Result<Self> {
        let meta = fs::symlink_metadata(path)
            .with_context(|| format!("cannot inspect {}", path.display()))?;
        ensure!(
            meta.file_type().is_file(),
            "destination is no longer a regular file: {}",
            path.display()
        );
        Ok(Self {
            dev: meta.dev(),
            ino: meta.ino(),
            len: meta.len(),
            modified: (meta.mtime(), meta.mtime_nsec()),
            changed: (meta.ctime(), meta.ctime_nsec()),
            mode: meta.mode(),
        })
    }
}

fn directory_identity(meta: Metadata) -> (u64, u64) {
    (meta.dev(), meta.ino())
}

impl Destination {
    pub fn capture(path: PathBuf) -> Result<Self> {
        let identity = Identity::read(&path)?;
        let parent_identity = directory_identity(fs::metadata(
            path.parent().context("executable has no parent directory")?,
        )?);
        Ok(Self { path, identity, parent_identity })
    }

    pub fn lock(&self) -> Result<fd_lock::RwLock<File>> {
        let mut name = OsString::from(".");
        name.push(self.path.file_name().unwrap());
        name.push(".tmup-upgrade.lock");
        let path = self.path.parent().unwrap().join(name);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&path)
            .with_context(|| format!("cannot open upgrade lock {}", path.display()))?;
        ensure!(file.metadata()?.is_file(), "upgrade lock is not a regular file");
        Ok(fd_lock::RwLock::new(file))
    }

    pub fn check_unchanged(&self) -> Result<()> {
        ensure!(
            Identity::read(&self.path)? == self.identity,
            "executable destination changed during upgrade: {}",
            self.path.display()
        );
        ensure!(
            directory_identity(fs::metadata(self.path.parent().unwrap())?) == self.parent_identity,
            "executable parent directory changed during upgrade"
        );
        Ok(())
    }

    pub fn copy_candidate(&self, prepared: &Path) -> Result<TempPath> {
        let mut source = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(prepared)
            .context("cannot open prepared tmup binary")?;
        let meta = source.metadata()?;
        ensure!(
            meta.is_file() && meta.mode() & 0o111 != 0,
            "prepared tmup must be a regular file with executable permission bits"
        );
        let mut candidate = tempfile::Builder::new()
            .prefix(".tmup-upgrade-")
            .suffix(".tmp")
            .tempfile_in(self.path.parent().unwrap())
            .context("cannot create candidate beside executable")?;
        let result = (|| {
            std::io::copy(&mut source, &mut candidate)?;
            candidate.as_file().set_permissions(fs::Permissions::from_mode(meta.mode() & 0o777))?;
            candidate.as_file().sync_all()?;
            Ok(())
        })();
        if let Err(error) = result {
            let path = candidate.path().to_owned();
            return match candidate.close() {
                Ok(()) => Err(error),
                Err(cleanup) => Err(error
                    .context(format!("candidate cleanup failed at {}: {cleanup}", path.display()))),
            };
        }
        // into_temp_path closes the writable descriptor before execution (ETXTBSY).
        Ok(candidate.into_temp_path())
    }

    pub fn verify_candidate(
        &self,
        path: &Path,
        selected: &Version,
        workspace: &Path,
        timeout: Duration,
    ) -> Result<()> {
        let output = process::run(
            Command::new(path).arg("--version"),
            workspace,
            timeout,
            "candidate version check",
        )?
        .require_success("candidate version check")?;
        ensure!(
            output.stdout == format!("tmup {selected}\n"),
            "candidate version mismatch: expected tmup {selected}, got {:?}",
            output.stdout.trim()
        );
        Ok(())
    }

    pub fn publish(&self, candidate: &mut Option<TempPath>) -> Result<()> {
        self.check_unchanged()?;
        match candidate.take().context("missing upgrade candidate")?.persist(&self.path) {
            Ok(()) => Ok(()),
            Err(error) => {
                *candidate = Some(error.path);
                Err(error.error).with_context(|| format!("cannot replace {}", self.path.display()))
            }
        }
    }
}
