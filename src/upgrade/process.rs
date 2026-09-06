use std::fs::File;
use std::io::{Read, Seek};
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

pub(super) struct Output {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

impl Output {
    pub fn require_success(self, phase: &str) -> Result<Self> {
        if !self.status.success() {
            bail!("{phase} failed ({}): {}", self.status, self.stderr.trim());
        }
        Ok(self)
    }
}

struct Group {
    child: Child,
    stopped: bool,
}

impl Group {
    fn terminate(&mut self) -> std::io::Result<()> {
        // SAFETY: this is our child's own process group, established before exec.
        if self.stopped {
            return Ok(());
        }
        let result = unsafe { libc::kill(-(self.child.id() as i32), libc::SIGKILL) };
        let error = std::io::Error::last_os_error();
        let waited = self.child.wait();
        self.stopped = waited.is_ok();
        if result == -1 && error.raw_os_error() != Some(libc::ESRCH) {
            return Err(error);
        }
        waited.map(|_| ())
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        let _ = self.terminate();
    }
}

pub(super) fn run(
    command: &mut Command,
    workspace: &Path,
    timeout: Duration,
    phase: &str,
) -> Result<Output> {
    let mut stdout = tempfile::tempfile_in(workspace)?;
    let mut stderr = tempfile::tempfile_in(workspace)?;
    command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?)
        .process_group(0);
    let mut group = Group {
        child: command.spawn().with_context(|| format!("cannot start {phase}"))?,
        stopped: false,
    };
    let started = Instant::now();
    let status = loop {
        if let Some(status) =
            group.child.try_wait().with_context(|| format!("cannot monitor {phase}"))?
        {
            break status;
        }
        if started.elapsed() >= timeout {
            group.terminate().with_context(|| format!("cannot stop {phase} after timeout"))?;
            bail!("{phase} timed out");
        }
        std::thread::sleep(
            Duration::from_millis(10).min(timeout.saturating_sub(started.elapsed())),
        );
    };
    // A helper may exit while background descendants still own workspace paths.
    group.terminate().with_context(|| format!("cannot stop descendants of {phase}"))?;
    Ok(Output { status, stdout: read_output(&mut stdout)?, stderr: read_output(&mut stderr)? })
}

fn read_output(file: &mut File) -> Result<String> {
    file.rewind()?;
    let mut bytes = Vec::new();
    file.take(65537).read_to_end(&mut bytes)?;
    anyhow::ensure!(bytes.len() <= 65536, "helper output exceeded 64 KiB");
    String::from_utf8(bytes).context("helper output is not UTF-8")
}
