use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use tmup::config_mode::ConfigMode;

use super::{Continuation, InvocationContext, ResolvedTpmIdentity};

const RECORD_VERSION: u32 = 1;
const SESSIONS_DIRECTORY: &str = "init-sessions";
const BOOTSTRAP_RECORD: &str = "bootstrap.json";
const CLAIMED_BOOTSTRAP_RECORD: &str = ".bootstrap.claimed.json";
const TEMP_BOOTSTRAP_RECORD: &str = ".bootstrap.json.tmp";
const SESSION_CREATE_ATTEMPTS: u64 = 32;

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(super) struct PublishedBootstrap {
    session_dir: PathBuf,
    record_path: PathBuf,
}

impl PublishedBootstrap {
    pub(super) fn record_path(&self) -> &Path {
        &self.record_path
    }

    pub(super) fn cleanup(self) -> Result<()> {
        cleanup_session_directory(&self.session_dir)
    }
}

#[derive(Debug)]
pub(super) struct ClaimedBootstrap {
    context: InvocationContext,
    session_dir: PathBuf,
}

impl ClaimedBootstrap {
    #[cfg(test)]
    pub(super) fn context(&self) -> &InvocationContext {
        &self.context
    }

    pub(super) fn into_parts(self) -> (InvocationContext, SessionOwner) {
        (self.context, SessionOwner { session_dir: self.session_dir })
    }

    #[cfg(test)]
    fn cleanup(self) -> Result<()> {
        self.into_parts().1.cleanup()
    }
}

#[derive(Debug)]
pub(super) struct SessionOwner {
    session_dir: PathBuf,
}

impl SessionOwner {
    pub(super) fn cleanup(self) -> Result<()> {
        cleanup_session_directory(&self.session_dir)
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ContinuationRecord {
    version: u32,
    continuation: RecordedContinuation,
}

#[derive(Debug, Deserialize)]
struct VersionEnvelope {
    version: u32,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
enum RecordedContinuation {
    Bootstrap { context: RecordedContext },
    UiChild { context: RecordedContext },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecordedContext {
    config_mode: RecordedConfigMode,
    config_path: PathBuf,
    tpm_identity: RecordedTpmIdentity,
    data_root: PathBuf,
    state_root: PathBuf,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum RecordedConfigMode {
    Pure,
    Mixed,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RecordedTpmIdentity {
    Disabled,
    Path { path: PathBuf },
    Absent,
}

impl From<&InvocationContext> for RecordedContext {
    fn from(context: &InvocationContext) -> Self {
        let config_mode = match context.config_mode {
            ConfigMode::Pure => RecordedConfigMode::Pure,
            ConfigMode::Mixed => RecordedConfigMode::Mixed,
        };
        let tpm_identity = match &context.tpm_identity {
            ResolvedTpmIdentity::Disabled => RecordedTpmIdentity::Disabled,
            ResolvedTpmIdentity::Path(path) => RecordedTpmIdentity::Path { path: path.clone() },
            ResolvedTpmIdentity::Absent => RecordedTpmIdentity::Absent,
        };
        Self {
            config_mode,
            config_path: context.config_path.clone(),
            tpm_identity,
            data_root: context.data_root.clone(),
            state_root: context.state_root.clone(),
        }
    }
}

impl From<RecordedContext> for InvocationContext {
    fn from(context: RecordedContext) -> Self {
        let config_mode = match context.config_mode {
            RecordedConfigMode::Pure => ConfigMode::Pure,
            RecordedConfigMode::Mixed => ConfigMode::Mixed,
        };
        let tpm_identity = match context.tpm_identity {
            RecordedTpmIdentity::Disabled => ResolvedTpmIdentity::Disabled,
            RecordedTpmIdentity::Path { path } => ResolvedTpmIdentity::Path(path),
            RecordedTpmIdentity::Absent => ResolvedTpmIdentity::Absent,
        };
        Self::new(
            config_mode,
            context.config_path,
            tpm_identity,
            context.data_root,
            context.state_root,
        )
    }
}

impl ContinuationRecord {
    fn from_continuation(continuation: &Continuation) -> Self {
        let continuation = match continuation {
            Continuation::DeferredBootstrap(context) => {
                RecordedContinuation::Bootstrap { context: context.into() }
            }
            Continuation::HostedChild(context) => {
                RecordedContinuation::UiChild { context: context.into() }
            }
        };
        Self { version: RECORD_VERSION, continuation }
    }
}

pub(super) fn publish_bootstrap(context: &InvocationContext) -> Result<PublishedBootstrap> {
    validate_context_paths(context)?;
    let session_dir = create_session_directory(&context.state_root)?;
    let publication = publish_in_session(&session_dir, context);
    if publication.is_err() {
        let _ = fs::remove_dir_all(&session_dir);
    }
    publication
}

pub(super) fn claim_bootstrap(record_path: &Path) -> Result<ClaimedBootstrap> {
    let session_dir = validate_record_path_shape(record_path)?;
    let claimed_path = session_dir.join(CLAIMED_BOOTSTRAP_RECORD);
    fs::rename(record_path, &claimed_path).with_context(|| {
        format!(
            "failed to claim bootstrap continuation record {} (it may be missing or already consumed)",
            record_path.display()
        )
    })?;

    let bytes = fs::read(&claimed_path).with_context(|| {
        format!("failed to read claimed continuation record: {}", claimed_path.display())
    })?;
    let envelope: VersionEnvelope = serde_json::from_slice(&bytes)
        .with_context(|| format!("malformed continuation record: {}", claimed_path.display()))?;
    ensure!(
        envelope.version == RECORD_VERSION,
        "unsupported continuation record version {} (expected {})",
        envelope.version,
        RECORD_VERSION
    );
    let record: ContinuationRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("malformed continuation record: {}", claimed_path.display()))?;
    let context = match record.continuation {
        RecordedContinuation::Bootstrap { context } => InvocationContext::from(context),
        RecordedContinuation::UiChild { .. } => {
            anyhow::bail!("bootstrap resume path contains a UI-child continuation record")
        }
    };
    validate_context_paths(&context)?;
    ensure!(
        session_dir.parent() == Some(context.state_root.join(SESSIONS_DIRECTORY).as_path()),
        "bootstrap continuation record does not belong to its recorded state root"
    );
    fs::remove_file(&claimed_path).with_context(|| {
        format!("failed to consume continuation record: {}", claimed_path.display())
    })?;

    Ok(ClaimedBootstrap { context, session_dir })
}

fn publish_in_session(
    session_dir: &Path,
    context: &InvocationContext,
) -> Result<PublishedBootstrap> {
    let record =
        ContinuationRecord::from_continuation(&Continuation::DeferredBootstrap(context.clone()));
    let json =
        serde_json::to_vec_pretty(&record).context("failed to serialize continuation record")?;
    let temp_path = session_dir.join(TEMP_BOOTSTRAP_RECORD);
    let record_path = session_dir.join(BOOTSTRAP_RECORD);

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&temp_path).with_context(|| {
        format!("failed to create continuation record: {}", temp_path.display())
    })?;
    file.write_all(&json)?;
    file.write_all(b"\n")?;
    drop(file);
    fs::rename(&temp_path, &record_path).with_context(|| {
        format!(
            "failed to publish continuation record: {} -> {}",
            temp_path.display(),
            record_path.display()
        )
    })?;

    Ok(PublishedBootstrap { session_dir: session_dir.to_path_buf(), record_path })
}

fn create_session_directory(state_root: &Path) -> Result<PathBuf> {
    ensure!(state_root.is_absolute(), "Init Session state root must be absolute");
    let sessions_root = state_root.join(SESSIONS_DIRECTORY);
    fs::create_dir_all(&sessions_root).with_context(|| {
        format!("failed to create Init Session root: {}", sessions_root.display())
    })?;
    #[cfg(unix)]
    fs::set_permissions(&sessions_root, fs::Permissions::from_mode(0o700)).with_context(|| {
        format!("failed to secure Init Session root: {}", sessions_root.display())
    })?;

    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_nanos();
    for _ in 0..SESSION_CREATE_ATTEMPTS {
        let sequence = NEXT_SESSION_ID.fetch_add(1, Ordering::Relaxed);
        let session_dir = sessions_root
            .join(format!("session-{}-{timestamp:032x}-{sequence:016x}", std::process::id()));
        let mut builder = DirBuilder::new();
        #[cfg(unix)]
        builder.mode(0o700);
        match builder.create(&session_dir) {
            Ok(()) => return Ok(session_dir),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to create Init Session directory: {}", session_dir.display())
                });
            }
        }
    }
    anyhow::bail!(
        "failed to create a unique Init Session directory under {}",
        sessions_root.display()
    )
}

fn validate_record_path_shape(record_path: &Path) -> Result<PathBuf> {
    ensure!(record_path.is_absolute(), "--resume requires an absolute continuation record path");
    ensure!(
        record_path.file_name().and_then(|name| name.to_str()) == Some(BOOTSTRAP_RECORD),
        "--resume path must name {BOOTSTRAP_RECORD}"
    );
    let session_dir =
        record_path.parent().context("continuation record has no session directory")?;
    ensure!(
        session_dir
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("session-")),
        "continuation record is not inside an Init Session directory"
    );
    ensure!(
        session_dir.parent().and_then(Path::file_name).and_then(|name| name.to_str())
            == Some(SESSIONS_DIRECTORY),
        "continuation record is not under the Init Session root"
    );
    Ok(session_dir.to_path_buf())
}

fn validate_context_paths(context: &InvocationContext) -> Result<()> {
    for (name, path) in [
        ("config path", &context.config_path),
        ("data root", &context.data_root),
        ("state root", &context.state_root),
    ] {
        ensure!(path.is_absolute(), "Init Session {name} must be absolute: {}", path.display());
    }
    if let ResolvedTpmIdentity::Path(path) = &context.tpm_identity {
        ensure!(path.is_absolute(), "Init Session TPM path must be absolute: {}", path.display());
    }
    Ok(())
}

fn cleanup_session_directory(session_dir: &Path) -> Result<()> {
    fs::remove_dir_all(session_dir).with_context(|| {
        format!("failed to clean Init Session directory: {}", session_dir.display())
    })
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;

    fn context(root: &Path) -> InvocationContext {
        InvocationContext::new(
            ConfigMode::Mixed,
            root.join("config/tmux/tmup.kdl"),
            ResolvedTpmIdentity::Absent,
            root.join("data/tmup"),
            root.join("state/tmup"),
        )
    }

    #[test]
    fn bootstrap_publication_is_atomic_and_private() {
        let dir = tempdir().unwrap();
        let published = publish_bootstrap(&context(dir.path())).unwrap();

        assert!(published.record_path().is_file());
        assert!(!published.session_dir.join(TEMP_BOOTSTRAP_RECORD).exists());
        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(published.record_path()).unwrap()).unwrap();
        assert_eq!(value["version"], RECORD_VERSION);
        assert_eq!(value["continuation"]["role"], "bootstrap");
        assert_eq!(value["continuation"]["context"]["config_mode"], "mixed");
        assert_eq!(value["continuation"]["context"]["tpm_identity"]["kind"], "absent");
        assert!(value["continuation"]["context"].get("config").is_none());
        assert!(value["continuation"]["context"].get("lock").is_none());

        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&published.session_dir).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(published.record_path()).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn continuation_model_can_represent_a_future_ui_child() {
        let dir = tempdir().unwrap();
        let record =
            ContinuationRecord::from_continuation(&Continuation::HostedChild(context(dir.path())));

        let value = serde_json::to_value(record).unwrap();

        assert_eq!(value["continuation"]["role"], "ui_child");
        assert!(value["continuation"]["context"].get("config").is_none());
        assert!(value["continuation"]["context"].get("lock").is_none());
    }

    #[test]
    fn bootstrap_record_has_a_single_owner_after_claim() {
        let dir = tempdir().unwrap();
        let expected_context = context(dir.path());
        let published = publish_bootstrap(&expected_context).unwrap();
        let record_path = published.record_path().to_path_buf();
        let session_dir = published.session_dir.clone();

        let claimed = claim_bootstrap(&record_path).unwrap();

        assert_eq!(claimed.context(), &expected_context);
        assert!(!record_path.exists());
        assert!(claim_bootstrap(&record_path).is_err(), "a consumed record must not be claimable");
        claimed.cleanup().unwrap();
        assert!(!session_dir.exists(), "the owner must clean only its session directory");
    }

    #[test]
    fn missing_corrupt_and_unsupported_records_are_operation_errors() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());

        let missing = context
            .state_root
            .join(SESSIONS_DIRECTORY)
            .join("session-missing")
            .join(BOOTSTRAP_RECORD);
        let error = claim_bootstrap(&missing).unwrap_err();
        assert!(error.to_string().contains("missing or already consumed"), "{error:#}");

        let corrupt = publish_bootstrap(&context).unwrap();
        let corrupt_path = corrupt.record_path().to_path_buf();
        let corrupt_claimed_path = corrupt.session_dir.join(CLAIMED_BOOTSTRAP_RECORD);
        fs::write(&corrupt_path, b"not json").unwrap();
        let error = claim_bootstrap(&corrupt_path).unwrap_err();
        assert!(error.to_string().contains("malformed continuation record"), "{error:#}");
        assert!(!corrupt_path.exists());
        assert!(corrupt_claimed_path.is_file(), "validation must happen after atomic claim");

        let unsupported = publish_bootstrap(&context).unwrap();
        let unsupported_path = unsupported.record_path().to_path_buf();
        let unsupported_claimed_path = unsupported.session_dir.join(CLAIMED_BOOTSTRAP_RECORD);
        fs::write(&unsupported_path, br#"{"version":2}"#).unwrap();
        let error = claim_bootstrap(&unsupported_path).unwrap_err();
        assert!(
            error.to_string().contains("unsupported continuation record version 2"),
            "{error:#}"
        );
        assert!(!unsupported_path.exists());
        assert!(
            unsupported_claimed_path.is_file(),
            "version checks must happen after atomic claim"
        );
    }

    #[test]
    fn concurrent_publications_use_isolated_session_directories() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        let publications = std::thread::scope(|scope| {
            let handles = (0..16)
                .map(|_| {
                    let context = context.clone();
                    scope.spawn(move || publish_bootstrap(&context).unwrap())
                })
                .collect::<Vec<_>>();
            handles.into_iter().map(|handle| handle.join().unwrap()).collect::<Vec<_>>()
        });

        let mut session_dirs = publications
            .iter()
            .map(|publication| publication.session_dir.clone())
            .collect::<Vec<_>>();
        session_dirs.sort();
        session_dirs.dedup();

        assert_eq!(session_dirs.len(), publications.len());
        assert!(publications.iter().all(|publication| publication.record_path().is_file()));
    }
}
