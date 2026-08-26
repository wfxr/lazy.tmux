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

use super::{InvocationContext, ResolvedTpmIdentity};

const RECORD_VERSION: u32 = 1;
const SESSIONS_DIRECTORY: &str = "init-sessions";
const BOOTSTRAP_RECORD: &str = "bootstrap.json";
const CLAIMED_BOOTSTRAP_RECORD: &str = ".bootstrap.claimed.json";
const TEMP_BOOTSTRAP_RECORD: &str = ".bootstrap.json.tmp";
const UI_CHILD_RECORD: &str = "ui-child.json";
const CLAIMED_UI_CHILD_RECORD: &str = ".ui-child.claimed.json";
const TEMP_UI_CHILD_RECORD: &str = ".ui-child.json.tmp";
const CHILD_RESULT_RECORD: &str = "child-result.json";
const CLAIMED_CHILD_RESULT_RECORD: &str = ".child-result.claimed.json";
const TEMP_CHILD_RESULT_RECORD: &str = ".child-result.json.tmp";
const SESSION_CREATE_ATTEMPTS: u64 = 32;

static NEXT_SESSION_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub(super) struct PublishedBootstrap {
    session_dir: PathBuf,
    record_path: PathBuf,
}

#[derive(Debug)]
pub(super) struct PublishedUiChild<'session> {
    session: SessionOwnership<'session>,
    record_path: PathBuf,
    result_path: PathBuf,
}

impl PublishedUiChild<'_> {
    #[cfg(test)]
    fn session_dir(&self) -> &Path {
        self.session.session_dir()
    }

    pub(super) fn record_path(&self) -> &Path {
        &self.record_path
    }

    pub(super) fn result_path(&self) -> &Path {
        &self.result_path
    }

    pub(super) fn child_launched(&mut self) {
        self.session.child_launched();
    }

    pub(super) fn terminal_completion_confirmed(&mut self) {
        self.session.terminal_completion_confirmed();
    }

    pub(super) fn cleanup(self) -> Result<()> {
        self.session.cleanup()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ChildResult {
    Completed,
    CompletedWithPluginFailures { failures: Vec<String> },
    OperationFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum UiChildSource {
    Direct,
    DeferredBootstrap,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum UiHost {
    Popup,
    Split { wait_channel: String },
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

#[derive(Debug)]
pub(super) struct ClaimedUiChild {
    context: InvocationContext,
    result_path: PathBuf,
}

impl ClaimedUiChild {
    pub(super) fn context(&self) -> &InvocationContext {
        &self.context
    }

    #[cfg(test)]
    pub(super) fn result_path(&self) -> &Path {
        &self.result_path
    }

    pub(super) fn publish_result(self, result: &ChildResult) -> Result<()> {
        publish_child_result(&self.result_path, result)
    }
}

#[derive(Debug)]
pub(super) enum ClaimedContinuation {
    Bootstrap(ClaimedBootstrap),
    UiChild(ClaimedUiChild),
}

impl ClaimedBootstrap {
    #[cfg(test)]
    pub(super) fn context(&self) -> &InvocationContext {
        &self.context
    }

    pub(super) fn into_parts(self) -> (InvocationContext, SessionOwner) {
        (self.context, SessionOwner::new(self.session_dir))
    }

    #[cfg(test)]
    fn cleanup(self) -> Result<()> {
        self.into_parts().1.cleanup()
    }
}

#[derive(Debug)]
pub(super) struct SessionOwner {
    session_dir: PathBuf,
    cleanup: SessionCleanup,
}

impl SessionOwner {
    fn new(session_dir: PathBuf) -> Self {
        Self { session_dir, cleanup: SessionCleanup::Safe }
    }

    fn session_dir(&self) -> &Path {
        &self.session_dir
    }

    pub(super) fn child_launched(&mut self) {
        self.cleanup = SessionCleanup::Preserve;
    }

    pub(super) fn terminal_completion_confirmed(&mut self) {
        self.cleanup = SessionCleanup::Safe;
    }

    pub(super) fn cleanup(self) -> Result<()> {
        match self.cleanup {
            SessionCleanup::Safe => cleanup_session_directory(&self.session_dir),
            SessionCleanup::Preserve => Ok(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionCleanup {
    Safe,
    Preserve,
}

#[derive(Debug)]
enum SessionOwnership<'session> {
    Owned(SessionOwner),
    Borrowed(&'session mut SessionOwner),
}

impl SessionOwnership<'_> {
    fn session_dir(&self) -> &Path {
        match self {
            Self::Owned(owner) => owner.session_dir(),
            Self::Borrowed(owner) => owner.session_dir(),
        }
    }

    fn child_launched(&mut self) {
        match self {
            Self::Owned(owner) => owner.child_launched(),
            Self::Borrowed(owner) => owner.child_launched(),
        }
    }

    fn terminal_completion_confirmed(&mut self) {
        match self {
            Self::Owned(owner) => owner.terminal_completion_confirmed(),
            Self::Borrowed(owner) => owner.terminal_completion_confirmed(),
        }
    }

    fn cleanup(self) -> Result<()> {
        match self {
            Self::Owned(owner) => owner.cleanup(),
            Self::Borrowed(_) => Ok(()),
        }
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
#[serde(deny_unknown_fields)]
struct ChildResultRecord {
    version: u32,
    result: ChildResult,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case", deny_unknown_fields)]
enum RecordedContinuation {
    Bootstrap { context: RecordedContext },
    UiChild { context: RecordedContext, source: UiChildSource, completion: RecordedUiCompletion },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum RecordedUiCompletion {
    Popup { result_name: String },
    Split { result_name: String, wait_channel: String },
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
    fn bootstrap(context: &InvocationContext) -> Self {
        Self {
            version: RECORD_VERSION,
            continuation: RecordedContinuation::Bootstrap { context: context.into() },
        }
    }

    fn ui_child(
        context: &InvocationContext,
        source: UiChildSource,
        completion: RecordedUiCompletion,
    ) -> Self {
        Self {
            version: RECORD_VERSION,
            continuation: RecordedContinuation::UiChild {
                context: context.into(),
                source,
                completion,
            },
        }
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

pub(super) fn publish_ui_child<'session>(
    context: &InvocationContext,
    source: UiChildSource,
    host: UiHost,
    session: Option<&'session mut SessionOwner>,
) -> Result<PublishedUiChild<'session>> {
    validate_context_paths(context)?;
    let completion = match host {
        UiHost::Popup => RecordedUiCompletion::Popup { result_name: CHILD_RESULT_RECORD.into() },
        UiHost::Split { wait_channel } => RecordedUiCompletion::Split {
            result_name: CHILD_RESULT_RECORD.into(),
            wait_channel: {
                ensure!(
                    !wait_channel.is_empty(),
                    "split completion wait channel must not be empty"
                );
                wait_channel
            },
        },
    };
    let session = match session {
        Some(owner) => SessionOwnership::Borrowed(owner),
        None => SessionOwnership::Owned(SessionOwner::new(create_session_directory(
            &context.state_root,
        )?)),
    };
    let session_dir = session.session_dir().to_path_buf();
    let record = ContinuationRecord::ui_child(context, source, completion);
    let record_path = match publish_record(
        &session_dir,
        TEMP_UI_CHILD_RECORD,
        UI_CHILD_RECORD,
        &record,
        "continuation record",
    ) {
        Ok(record_path) => record_path,
        Err(error) => {
            let _ = session.cleanup();
            return Err(error);
        }
    };

    let result_path = session_dir.join(CHILD_RESULT_RECORD);
    Ok(PublishedUiChild { session, record_path, result_path })
}

pub(super) fn claim_bootstrap(record_path: &Path) -> Result<ClaimedBootstrap> {
    let (session_dir, claimed_path, record) = claim_continuation_record(
        record_path,
        BOOTSTRAP_RECORD,
        CLAIMED_BOOTSTRAP_RECORD,
        "bootstrap",
    )?;
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

pub(super) fn claim(record_path: &Path) -> Result<ClaimedContinuation> {
    ensure!(record_path.is_absolute(), "--resume requires an absolute continuation record path");
    match record_path.file_name().and_then(|name| name.to_str()) {
        Some(BOOTSTRAP_RECORD) => claim_bootstrap(record_path).map(ClaimedContinuation::Bootstrap),
        Some(UI_CHILD_RECORD) => claim_ui_child(record_path).map(ClaimedContinuation::UiChild),
        _ => anyhow::bail!("--resume path must name {BOOTSTRAP_RECORD} or {UI_CHILD_RECORD}"),
    }
}

pub(super) fn claim_ui_child(record_path: &Path) -> Result<ClaimedUiChild> {
    let (session_dir, claimed_path, record) = claim_continuation_record(
        record_path,
        UI_CHILD_RECORD,
        CLAIMED_UI_CHILD_RECORD,
        "UI-child",
    )?;
    let (context, result_name) = match record.continuation {
        RecordedContinuation::UiChild { context, source: _, completion } => {
            let result_name = match completion {
                RecordedUiCompletion::Popup { result_name } => result_name,
                RecordedUiCompletion::Split { result_name, wait_channel } => {
                    ensure!(
                        !wait_channel.is_empty(),
                        "split completion wait channel must not be empty"
                    );
                    result_name
                }
            };
            (InvocationContext::from(context), result_name)
        }
        RecordedContinuation::Bootstrap { .. } => {
            anyhow::bail!("UI-child resume path contains a bootstrap continuation record")
        }
    };
    validate_context_paths(&context)?;
    ensure!(
        session_dir.parent() == Some(context.state_root.join(SESSIONS_DIRECTORY).as_path()),
        "UI-child continuation record does not belong to its recorded state root"
    );
    let result_path = validated_relative_record_path(&session_dir, &result_name)?;
    fs::remove_file(&claimed_path).with_context(|| {
        format!("failed to consume continuation record: {}", claimed_path.display())
    })?;

    Ok(ClaimedUiChild { context, result_path })
}

fn claim_continuation_record(
    record_path: &Path,
    expected_name: &str,
    claimed_name: &str,
    role: &str,
) -> Result<(PathBuf, PathBuf, ContinuationRecord)> {
    let session_dir = validate_record_path_shape(record_path, expected_name)?;
    let claimed_path = session_dir.join(claimed_name);
    fs::rename(record_path, &claimed_path).with_context(|| {
        format!(
            "failed to claim {role} continuation record {} (it may be missing or already consumed)",
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
    let record = serde_json::from_slice(&bytes)
        .with_context(|| format!("malformed continuation record: {}", claimed_path.display()))?;
    Ok((session_dir, claimed_path, record))
}

fn publish_child_result(result_path: &Path, result: &ChildResult) -> Result<()> {
    let session_dir = validate_child_result_path(result_path)?;
    validate_child_result(result)?;
    let record = ChildResultRecord { version: RECORD_VERSION, result: result.clone() };
    publish_record(
        &session_dir,
        TEMP_CHILD_RESULT_RECORD,
        CHILD_RESULT_RECORD,
        &record,
        "child result",
    )?;
    Ok(())
}

pub(super) fn consume_child_result(result_path: &Path) -> Result<ChildResult> {
    let session_dir = validate_child_result_path(result_path)?;
    let claimed_path = session_dir.join(CLAIMED_CHILD_RESULT_RECORD);
    fs::rename(result_path, &claimed_path).with_context(|| {
        format!(
            "failed to claim child result {} (it may be missing or already consumed)",
            result_path.display()
        )
    })?;
    let bytes = fs::read(&claimed_path).with_context(|| {
        format!("failed to read claimed child result: {}", claimed_path.display())
    })?;
    let envelope: VersionEnvelope = serde_json::from_slice(&bytes)
        .with_context(|| format!("malformed child result: {}", claimed_path.display()))?;
    ensure!(
        envelope.version == RECORD_VERSION,
        "unsupported child result version {} (expected {})",
        envelope.version,
        RECORD_VERSION
    );
    let record: ChildResultRecord = serde_json::from_slice(&bytes)
        .with_context(|| format!("malformed child result: {}", claimed_path.display()))?;
    validate_child_result(&record.result)?;
    fs::remove_file(&claimed_path)
        .with_context(|| format!("failed to consume child result: {}", claimed_path.display()))?;
    Ok(record.result)
}

fn publish_in_session(
    session_dir: &Path,
    context: &InvocationContext,
) -> Result<PublishedBootstrap> {
    let record = ContinuationRecord::bootstrap(context);
    let record_path = publish_record(
        session_dir,
        TEMP_BOOTSTRAP_RECORD,
        BOOTSTRAP_RECORD,
        &record,
        "continuation record",
    )?;

    Ok(PublishedBootstrap { session_dir: session_dir.to_path_buf(), record_path })
}

fn publish_record(
    session_dir: &Path,
    temp_name: &str,
    record_name: &str,
    record: &impl Serialize,
    description: &str,
) -> Result<PathBuf> {
    let json = serde_json::to_vec_pretty(record)
        .with_context(|| format!("failed to serialize {description}"))?;
    let temp_path = session_dir.join(temp_name);
    let record_path = session_dir.join(record_name);

    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options
        .open(&temp_path)
        .with_context(|| format!("failed to create {description}: {}", temp_path.display()))?;
    file.write_all(&json)?;
    file.write_all(b"\n")?;
    drop(file);
    publish_temp_no_replace(&temp_path, &record_path).with_context(|| {
        format!(
            "failed to publish {description} without replacement (the record already exists or publication failed): {} -> {}",
            temp_path.display(),
            record_path.display()
        )
    })?;
    Ok(record_path)
}

fn publish_temp_no_replace(temp_path: &Path, record_path: &Path) -> std::io::Result<()> {
    match fs::hard_link(temp_path, record_path) {
        Ok(()) => fs::remove_file(temp_path),
        Err(error) => {
            let _ = fs::remove_file(temp_path);
            Err(error)
        }
    }
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

fn validate_record_path_shape(record_path: &Path, expected_name: &str) -> Result<PathBuf> {
    validate_session_record_path(record_path, expected_name, "continuation record")
}

fn validated_relative_record_path(session_dir: &Path, name: &str) -> Result<PathBuf> {
    let path = Path::new(name);
    ensure!(
        path.components().count() == 1
            && path.file_name().and_then(|value| value.to_str()) == Some(name),
        "Init Session result name must be a relative file name"
    );
    ensure!(name == CHILD_RESULT_RECORD, "unsupported Init Session result name {name:?}");
    Ok(session_dir.join(path))
}

fn validate_child_result_path(result_path: &Path) -> Result<PathBuf> {
    validate_session_record_path(result_path, CHILD_RESULT_RECORD, "child result")
}

fn validate_session_record_path(
    record_path: &Path,
    expected_name: &str,
    description: &str,
) -> Result<PathBuf> {
    ensure!(record_path.is_absolute(), "{description} path must be absolute");
    ensure!(
        record_path.file_name().and_then(|name| name.to_str()) == Some(expected_name),
        "{description} path must name {expected_name}"
    );
    let session_dir =
        record_path.parent().with_context(|| format!("{description} has no session directory"))?;
    ensure!(
        session_dir
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("session-")),
        "{description} is not inside an Init Session directory"
    );
    ensure!(
        session_dir.parent().and_then(Path::file_name).and_then(|name| name.to_str())
            == Some(SESSIONS_DIRECTORY),
        "{description} is not under the Init Session root"
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
    ensure!(
        matches!(
            (context.config_mode, &context.tpm_identity),
            (ConfigMode::Pure, ResolvedTpmIdentity::Disabled)
                | (ConfigMode::Mixed, ResolvedTpmIdentity::Path(_) | ResolvedTpmIdentity::Absent)
        ),
        "Init Session config mode and TPM identity are inconsistent"
    );
    Ok(())
}

fn validate_child_result(result: &ChildResult) -> Result<()> {
    if let ChildResult::CompletedWithPluginFailures { failures } = result {
        ensure!(
            !failures.is_empty() && failures.iter().all(|failure| !failure.is_empty()),
            "completed-with-plugin-failures result requires named failures"
        );
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
    fn ui_child_publication_records_source_and_split_completion() {
        let dir = tempdir().unwrap();
        let published = publish_ui_child(
            &context(dir.path()),
            UiChildSource::Direct,
            UiHost::Split { wait_channel: "tmup-init-test".into() },
            None,
        )
        .unwrap();

        let value: serde_json::Value =
            serde_json::from_slice(&fs::read(published.record_path()).unwrap()).unwrap();

        assert_eq!(value["version"], RECORD_VERSION);
        assert_eq!(value["continuation"]["role"], "ui_child");
        assert_eq!(value["continuation"]["source"], "direct");
        assert_eq!(value["continuation"]["completion"]["kind"], "split");
        assert_eq!(value["continuation"]["completion"]["wait_channel"], "tmup-init-test");
        assert_eq!(value["continuation"]["completion"]["result_name"], CHILD_RESULT_RECORD);
        assert!(published.record_path().is_file());
        assert!(!published.session_dir().join(TEMP_UI_CHILD_RECORD).exists());

        #[cfg(unix)]
        assert_eq!(
            fs::metadata(published.record_path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn ui_child_record_has_a_single_consumer() {
        let dir = tempdir().unwrap();
        let expected_context = context(dir.path());
        let published = publish_ui_child(
            &expected_context,
            UiChildSource::DeferredBootstrap,
            UiHost::Split { wait_channel: "tmup-init-test".into() },
            None,
        )
        .unwrap();
        let record_path = published.record_path().to_path_buf();

        let claimed = claim_ui_child(&record_path).unwrap();

        assert_eq!(claimed.context(), &expected_context);
        assert_eq!(claimed.result_path(), record_path.parent().unwrap().join(CHILD_RESULT_RECORD));
        assert!(!record_path.exists());
        assert!(claim_ui_child(&record_path).is_err(), "a consumed record must not be claimable");
    }

    #[test]
    fn duplicate_ui_child_publication_does_not_replace_the_immutable_record() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());
        let bootstrap = publish_bootstrap(&context).unwrap();
        let bootstrap_path = bootstrap.record_path().to_path_buf();
        let (_, mut owner) = claim_bootstrap(&bootstrap_path).unwrap().into_parts();
        let first = publish_ui_child(
            &context,
            UiChildSource::DeferredBootstrap,
            UiHost::Popup,
            Some(&mut owner),
        )
        .unwrap();
        let first_path = first.record_path().to_path_buf();
        let original = fs::read(first.record_path()).unwrap();
        first.cleanup().unwrap();

        let error = publish_ui_child(
            &context,
            UiChildSource::DeferredBootstrap,
            UiHost::Split { wait_channel: "replacement".into() },
            Some(&mut owner),
        )
        .unwrap_err();

        assert!(error.to_string().contains("already exists"), "{error:#}");
        assert_eq!(fs::read(first_path).unwrap(), original);
        owner.cleanup().unwrap();
    }

    #[test]
    fn atomic_publication_never_replaces_an_existing_record() {
        let dir = tempdir().unwrap();
        let temp_path = dir.path().join("record.tmp");
        let record_path = dir.path().join("record.json");
        fs::write(&temp_path, b"replacement").unwrap();
        fs::write(&record_path, b"original").unwrap();

        let error = publish_temp_no_replace(&temp_path, &record_path).unwrap_err();

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read(&record_path).unwrap(), b"original");
    }

    #[test]
    fn concurrent_duplicate_publications_have_exactly_one_winner() {
        let dir = tempdir().unwrap();
        let session_dir = dir.path().to_path_buf();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(16));
        let results = std::thread::scope(|scope| {
            let handles = (0..16)
                .map(|index| {
                    let session_dir = session_dir.clone();
                    let barrier = barrier.clone();
                    scope.spawn(move || {
                        barrier.wait();
                        let temp_name = format!(".result-{index}.tmp");
                        let record = ChildResultRecord {
                            version: RECORD_VERSION,
                            result: ChildResult::Completed,
                        };
                        publish_record(
                            &session_dir,
                            &temp_name,
                            CHILD_RESULT_RECORD,
                            &record,
                            "child result",
                        )
                    })
                })
                .collect::<Vec<_>>();
            handles.into_iter().map(|handle| handle.join().unwrap()).collect::<Vec<_>>()
        });

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
    }

    #[test]
    fn child_results_are_atomic_semantic_and_single_consumer_records() {
        let dir = tempdir().unwrap();
        let published =
            publish_ui_child(&context(dir.path()), UiChildSource::Direct, UiHost::Popup, None)
                .unwrap();
        let result_path = published.result_path().to_path_buf();
        let claimed = claim_ui_child(published.record_path()).unwrap();

        claimed
            .publish_result(&ChildResult::CompletedWithPluginFailures {
                failures: vec!["example.com/test/plugin".into()],
            })
            .unwrap();

        assert!(result_path.is_file());
        assert!(!result_path.parent().unwrap().join(TEMP_CHILD_RESULT_RECORD).exists());
        #[cfg(unix)]
        assert_eq!(fs::metadata(&result_path).unwrap().permissions().mode() & 0o777, 0o600);
        assert_eq!(
            consume_child_result(&result_path).unwrap(),
            ChildResult::CompletedWithPluginFailures {
                failures: vec!["example.com/test/plugin".into()]
            }
        );
        assert!(!result_path.exists());
        assert!(
            consume_child_result(&result_path).is_err(),
            "a consumed child result must not be readable twice"
        );
    }

    #[test]
    fn direct_ui_session_is_preserved_until_terminal_completion_is_confirmed() {
        let dir = tempdir().unwrap();
        let mut unknown =
            publish_ui_child(&context(dir.path()), UiChildSource::Direct, UiHost::Popup, None)
                .unwrap();
        let unknown_session = unknown.session_dir().to_path_buf();
        unknown.child_launched();
        unknown.cleanup().unwrap();
        assert!(
            unknown_session.exists(),
            "a launched child with unknown completion must retain its session"
        );

        let mut confirmed =
            publish_ui_child(&context(dir.path()), UiChildSource::Direct, UiHost::Popup, None)
                .unwrap();
        let confirmed_session = confirmed.session_dir().to_path_buf();
        confirmed.child_launched();
        confirmed.terminal_completion_confirmed();
        confirmed.cleanup().unwrap();
        assert!(
            !confirmed_session.exists(),
            "a child with confirmed terminal completion must release its session"
        );
    }

    #[test]
    fn missing_corrupt_and_unsupported_child_results_are_operation_errors() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());

        let missing =
            publish_ui_child(&context, UiChildSource::Direct, UiHost::Popup, None).unwrap();
        let error = consume_child_result(missing.result_path()).unwrap_err();
        assert!(error.to_string().contains("missing or already consumed"), "{error:#}");

        let corrupt =
            publish_ui_child(&context, UiChildSource::Direct, UiHost::Popup, None).unwrap();
        fs::write(corrupt.result_path(), b"not json").unwrap();
        let claimed_path = corrupt.session_dir().join(CLAIMED_CHILD_RESULT_RECORD);
        let error = consume_child_result(corrupt.result_path()).unwrap_err();
        assert!(error.to_string().contains("malformed child result"), "{error:#}");
        assert!(claimed_path.is_file(), "validation must happen after atomic claim");

        let unsupported =
            publish_ui_child(&context, UiChildSource::Direct, UiHost::Popup, None).unwrap();
        fs::write(unsupported.result_path(), br#"{"version":2}"#).unwrap();
        let error = consume_child_result(unsupported.result_path()).unwrap_err();
        assert!(error.to_string().contains("unsupported child result version 2"), "{error:#}");
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
    fn corrupt_unsupported_and_unsafe_ui_child_records_are_operation_errors() {
        let dir = tempdir().unwrap();
        let context = context(dir.path());

        let corrupt =
            publish_ui_child(&context, UiChildSource::Direct, UiHost::Popup, None).unwrap();
        fs::write(corrupt.record_path(), b"not json").unwrap();
        let error = claim_ui_child(corrupt.record_path()).unwrap_err();
        assert!(error.to_string().contains("malformed continuation record"), "{error:#}");

        let unsupported =
            publish_ui_child(&context, UiChildSource::Direct, UiHost::Popup, None).unwrap();
        fs::write(unsupported.record_path(), br#"{"version":2}"#).unwrap();
        let error = claim_ui_child(unsupported.record_path()).unwrap_err();
        assert!(
            error.to_string().contains("unsupported continuation record version 2"),
            "{error:#}"
        );

        let unsafe_result =
            publish_ui_child(&context, UiChildSource::Direct, UiHost::Popup, None).unwrap();
        let mut value: serde_json::Value =
            serde_json::from_slice(&fs::read(unsafe_result.record_path()).unwrap()).unwrap();
        value["continuation"]["completion"]["result_name"] = "../result.json".into();
        fs::write(unsafe_result.record_path(), serde_json::to_vec(&value).unwrap()).unwrap();
        let error = claim_ui_child(unsafe_result.record_path()).unwrap_err();
        assert!(error.to_string().contains("relative file name"), "{error:#}");
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
