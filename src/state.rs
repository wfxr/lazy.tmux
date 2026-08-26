use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

/// All filesystem paths used by tmup.
#[derive(Debug, Clone)]
pub struct Paths {
    /// Root for plugin checkouts: {data}/plugins/
    pub plugin_root: PathBuf,
    /// Staging area for in-progress installs: {data}/.staging/
    pub staging_root: PathBuf,
    /// Lock file for serializing write operations: {state}/operations.lock
    pub lock_path: PathBuf,
    /// Build failure markers: {state}/failures/
    pub failures_root: PathBuf,
    /// Operation log files: {state}/logs/
    pub logs_root: PathBuf,
    /// Active config file path
    pub config_path: PathBuf,
    /// Active lock file (usually next to the active config file)
    pub lockfile_path: PathBuf,
    /// Persistent cache roots for remote plugins: {data}/.repos/
    pub repo_cache_root: PathBuf,
}

impl Paths {
    /// Resolve paths from the XDG base directories of the current user.
    pub fn resolve() -> Result<Self> {
        Self::resolve_with_config_path(None)
    }

    /// Resolve paths from the XDG base directories, optionally overriding the config path.
    pub fn resolve_with_config_path(config_path: Option<PathBuf>) -> Result<Self> {
        let home_dir = resolve_home_dir().ok();
        let data_dir = xdg_dir("XDG_DATA_HOME", ".local/share", home_dir.as_deref())?.join("tmup");
        let state_dir =
            xdg_dir("XDG_STATE_HOME", ".local/state", home_dir.as_deref())?.join("tmup");
        let config_path = match config_path {
            Some(path) => path,
            None => tmux_config_dir(home_dir.as_deref())?.join("tmup.kdl"),
        };
        let lockfile_path =
            config_path.parent().context("config path has no parent directory")?.join("tmup.lock");

        Ok(Self {
            plugin_root: data_dir.join("plugins"),
            staging_root: data_dir.join(".staging"),
            lock_path: state_dir.join("operations.lock"),
            failures_root: state_dir.join("failures"),
            logs_root: state_dir.join("logs"),
            config_path,
            lockfile_path,
            repo_cache_root: data_dir.join(".repos"),
        })
    }

    /// Create a Paths for testing with explicit roots.
    pub fn for_test(data_root: impl Into<PathBuf>, state_root: impl Into<PathBuf>) -> Self {
        let data = data_root.into();
        let state = state_root.into();
        Self {
            plugin_root: data.join("plugins"),
            staging_root: data.join(".staging"),
            lock_path: state.join("operations.lock"),
            failures_root: state.join("failures"),
            logs_root: state.join("logs"),
            config_path: state.join("tmup.kdl"),
            lockfile_path: state.join("tmup.lock"),
            repo_cache_root: data.join(".repos"),
        }
    }

    /// Reconstruct Paths from explicit roots passed by the init parent process.
    pub fn from_runtime_roots(
        data_root: PathBuf,
        state_root: PathBuf,
        config_path: PathBuf,
    ) -> Result<Self> {
        let lockfile_path =
            config_path.parent().context("config path has no parent directory")?.join("tmup.lock");
        Ok(Self {
            plugin_root: data_root.join("plugins"),
            staging_root: data_root.join(".staging"),
            lock_path: state_root.join("operations.lock"),
            failures_root: state_root.join("failures"),
            logs_root: state_root.join("logs"),
            config_path,
            lockfile_path,
            repo_cache_root: data_root.join(".repos"),
        })
    }

    /// Override the active config path and retarget the derived lockfile path.
    pub fn set_config_path(&mut self, config_path: PathBuf) -> Result<()> {
        let config_dir = config_path.parent().with_context(|| {
            format!("config path has no parent directory: {}", config_path.display())
        })?;
        self.lockfile_path = config_dir.join("tmup.lock");
        self.config_path = config_path;
        Ok(())
    }

    /// Clone the current path set while retargeting the active config and lockfile pair.
    pub fn with_config_path(&self, config_path: PathBuf) -> Result<Self> {
        let mut paths = self.clone();
        paths.set_config_path(config_path)?;
        Ok(paths)
    }

    /// Ensure all required directories exist.
    pub fn ensure_dirs(&self) -> Result<()> {
        fs::create_dir_all(&self.plugin_root)?;
        fs::create_dir_all(&self.staging_root)?;
        fs::create_dir_all(&self.failures_root)?;
        fs::create_dir_all(&self.logs_root)?;
        fs::create_dir_all(&self.repo_cache_root)?;
        if let Some(parent) = self.lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        Ok(())
    }

    /// The data root is the parent of `plugin_root`.
    pub fn data_root(&self) -> &Path {
        self.plugin_root.parent().expect("plugin_root must have a parent")
    }

    /// The state root is the parent of `failures_root`.
    pub fn state_root(&self) -> &Path {
        self.failures_root.parent().expect("failures_root must have a parent")
    }

    /// Get the install directory for a remote plugin by id.
    pub fn plugin_dir(&self, id: &str) -> PathBuf {
        self.plugin_root.join(checked_plugin_id(id))
    }

    /// Get the cache directory for a remote plugin id.
    pub fn repo_cache_dir(&self, id: &str) -> PathBuf {
        self.repo_cache_root.join(format!("{}.git", checked_plugin_id(id)))
    }

    /// Create a staging directory for a plugin operation.
    pub fn staging_dir(&self, id: &str) -> PathBuf {
        let id = checked_plugin_id(id);
        let hash = &build_command_hash(id)[..12];
        let pid = std::process::id();
        self.staging_root.join(format!("{hash}-{pid}"))
    }
}

/// Validate that a plugin id contains no unsafe path segments.
pub(crate) fn validate_plugin_id(id: &str) -> Result<()> {
    ensure!(!id.is_empty(), "unsafe plugin id: empty");
    for segment in id.split('/') {
        ensure!(
            !segment.is_empty() && segment != "." && segment != ".." && !segment.contains('\\'),
            "unsafe plugin id segment: {segment:?}"
        );
    }
    Ok(())
}

pub(crate) fn resolve_home_dir() -> Result<PathBuf> {
    resolve_home_dir_from_env(std::env::var("HOME").ok().as_deref())
}

fn resolve_home_dir_from_env(home: Option<&str>) -> Result<PathBuf> {
    let home =
        home.filter(|value| !value.is_empty()).context("HOME must be set to an absolute path")?;
    let path = PathBuf::from(home);
    ensure!(path.is_absolute(), "HOME must be set to an absolute path");
    Ok(path)
}

fn xdg_dir(var: &str, fallback_suffix: &str, home: Option<&Path>) -> Result<PathBuf> {
    match std::env::var(var).ok().map(PathBuf::from) {
        Some(path) if path.is_absolute() => Ok(path),
        _ => Ok(home.context("HOME must be set to an absolute path")?.join(fallback_suffix)),
    }
}

fn tmux_config_dir(home: Option<&Path>) -> Result<PathBuf> {
    Ok(xdg_dir("XDG_CONFIG_HOME", ".config", home)?.join("tmux"))
}

fn checked_plugin_id(id: &str) -> &str {
    validate_plugin_id(id).expect("plugin ids must be validated before path construction");
    id
}

fn map_try_write_result<T>(result: std::io::Result<T>) -> std::io::Result<Option<T>> {
    match result {
        Ok(guard) => Ok(Some(guard)),
        Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(err) => Err(err),
    }
}

/// SHA-256 hash of a string, returned as hex.
pub fn build_command_hash(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    base16ct::lower::encode_string(&hasher.finalize())
}

/// Key for identifying a known build failure, used to suppress auto-retry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FailureKey {
    /// Plugin id the failure belongs to.
    pub plugin_id: String,
    /// Git commit hash at the time of failure.
    pub commit: String,
    /// Hash of the build command that failed.
    pub build_hash: String,
}

impl FailureKey {
    /// Construct a `FailureKey` from its three string components.
    pub fn new(plugin_id: &str, commit: &str, build_hash: &str) -> Self {
        Self { plugin_id: plugin_id.into(), commit: commit.into(), build_hash: build_hash.into() }
    }

    /// Derive the filename for persisting this failure marker.
    pub fn filename(&self) -> String {
        let combined = format!("{}:{}:{}", self.plugin_id, self.commit, self.build_hash);
        let hash = build_command_hash(&combined);
        format!("{}.json", &hash[..16])
    }
}

/// A persisted record of a build failure.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FailureMarker {
    /// Plugin id the failure belongs to.
    pub plugin_id: String,
    /// Git commit hash at the time of failure.
    pub commit: String,
    /// Hash of the build command that failed.
    pub build_hash: String,
    /// Full build command string that was executed.
    pub build_command: String,
    /// Timestamp when the failure was recorded.
    pub failed_at: String,
    /// Truncated stderr output from the failed build.
    pub stderr_summary: String,
}

impl FailureMarker {
    /// Derive the `FailureKey` that uniquely identifies this marker.
    pub fn key(&self) -> FailureKey {
        FailureKey::new(&self.plugin_id, &self.commit, &self.build_hash)
    }
}

/// Write a failure marker to disk.
pub fn write_failure_marker(failures_root: &Path, marker: &FailureMarker) -> Result<()> {
    fs::create_dir_all(failures_root)?;
    let key = marker.key();
    let path = failures_root.join(key.filename());
    let json = serde_json::to_string_pretty(marker)?;
    fs::write(&path, json)?;
    Ok(())
}

/// Read all failure markers from disk.
pub fn read_failure_markers(failures_root: &Path) -> Result<Vec<FailureMarker>> {
    let mut markers = Vec::new();
    if !failures_root.exists() {
        return Ok(markers);
    }
    for entry in fs::read_dir(failures_root)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json") {
            match fs::read_to_string(&path) {
                Ok(content) => {
                    if let Ok(marker) = serde_json::from_str::<FailureMarker>(&content) {
                        markers.push(marker);
                    }
                }
                Err(_) => continue,
            }
        }
    }
    Ok(markers)
}

/// Remove failure markers matching a specific plugin id.
pub fn clear_failure_markers(failures_root: &Path, plugin_id: &str) -> Result<()> {
    if !failures_root.exists() {
        return Ok(());
    }
    for entry in fs::read_dir(failures_root)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) == Some("json")
            && let Ok(content) = fs::read_to_string(&path)
            && let Ok(marker) = serde_json::from_str::<FailureMarker>(&content)
            && marker.plugin_id == plugin_id
        {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

/// Check if a specific failure key is known.
pub fn has_failure_marker(failures_root: &Path, key: &FailureKey) -> Result<bool> {
    let path = failures_root.join(key.filename());
    Ok(path.exists())
}

/// Current wall-clock timestamp for failure markers.
pub fn timestamp_now() -> String {
    let now =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    format!("{}s-since-epoch", now.as_secs())
}

/// Operation lock using fd-lock for cross-process mutual exclusion.
///
/// Uses `flock(LOCK_EX)` under the hood. The lock is released when the
/// guard is dropped (which closes the file descriptor).
pub struct OperationLock;

impl OperationLock {
    fn open_lock_file(lock_path: &Path) -> Result<fs::File> {
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)
            .with_context(|| format!("failed to open lock file: {}", lock_path.display()))
    }

    /// Acquire the exclusive write lock, blocking until available.
    pub fn acquire(lock_path: &Path) -> Result<OperationLockGuard> {
        let file = Self::open_lock_file(lock_path)?;
        let mut lock = fd_lock::RwLock::new(file);
        let guard = lock.write().map_err(|e| anyhow::anyhow!("failed to acquire lock: {e}"))?;
        // Safety rationale: `fd_lock::RwLockWriteGuard` calls `LOCK_UN` on
        // drop, which would release the flock immediately.  We want the lock
        // held until `OperationLockGuard` (which owns the `RwLock<File>`) is
        // dropped — at that point the fd is closed and the OS releases the
        // flock.  `forget` is safe here because `RwLockWriteGuard` borrows the
        // same fd owned by `RwLock`; no separate resources are leaked.
        std::mem::forget(guard);
        Ok(OperationLockGuard { _lock: lock })
    }

    /// Try to acquire the exclusive write lock. Returns None if already held.
    pub fn try_acquire(lock_path: &Path) -> Result<Option<OperationLockGuard>> {
        let file = Self::open_lock_file(lock_path)?;
        let mut lock = fd_lock::RwLock::new(file);
        {
            let Some(guard) = map_try_write_result(lock.try_write())
                .map_err(|e| anyhow::anyhow!("failed to acquire lock: {e}"))?
            else {
                return Ok(None);
            };
            // Scope the borrow so we can move `lock` afterwards.
            {
                // See `acquire` for why `forget` is correct here.
                std::mem::forget(guard);
            }
        }
        Ok(Some(OperationLockGuard { _lock: lock }))
    }
}

/// RAII guard that holds the exclusive operation lock until dropped.
pub struct OperationLockGuard {
    _lock: fd_lock::RwLock<fs::File>,
}

#[cfg(test)]
mod tests {
    use std::io::{Error, ErrorKind, Result};
    use std::sync::{LazyLock, Mutex};

    static ENV_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

    #[test]
    fn try_write_result_maps_would_block_to_none() {
        let result: Result<()> = Err(ErrorKind::WouldBlock.into());
        assert!(super::map_try_write_result(result).unwrap().is_none());
    }

    #[test]
    fn try_write_result_preserves_real_errors() {
        let err = super::map_try_write_result::<()>(Err(Error::other("boom"))).unwrap_err();
        assert_eq!(err.kind(), ErrorKind::Other);
    }

    #[test]
    fn resolve_home_dir_from_env_rejects_missing_home() {
        assert!(super::resolve_home_dir_from_env(None).is_err());
    }

    #[test]
    fn resolve_uses_home_fallback_when_xdg_is_unset() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("home");
        std::fs::create_dir_all(&home).unwrap();

        let previous_data = std::env::var_os("XDG_DATA_HOME");
        let previous_state = std::env::var_os("XDG_STATE_HOME");
        let previous_config = std::env::var_os("XDG_CONFIG_HOME");
        let previous_home = std::env::var_os("HOME");

        remove_env_var("XDG_DATA_HOME");
        remove_env_var("XDG_STATE_HOME");
        remove_env_var("XDG_CONFIG_HOME");
        set_env_var("HOME", &home);

        let paths = super::Paths::resolve_with_config_path(None).unwrap();

        assert_eq!(paths.plugin_root, home.join(".local/share/tmup/plugins"));
        assert_eq!(paths.lock_path, home.join(".local/state/tmup/operations.lock"));
        assert_eq!(paths.config_path, home.join(".config/tmux/tmup.kdl"));

        restore_env("XDG_DATA_HOME", previous_data);
        restore_env("XDG_STATE_HOME", previous_state);
        restore_env("XDG_CONFIG_HOME", previous_config);
        restore_env("HOME", previous_home);
    }

    #[test]
    fn resolve_uses_absolute_xdg_dirs_without_home() {
        let _guard = ENV_LOCK.lock().unwrap();
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let state = dir.path().join("state");
        let config = dir.path().join("config");

        let previous_data = std::env::var_os("XDG_DATA_HOME");
        let previous_state = std::env::var_os("XDG_STATE_HOME");
        let previous_config = std::env::var_os("XDG_CONFIG_HOME");
        let previous_home = std::env::var_os("HOME");

        set_env_var("XDG_DATA_HOME", &data);
        set_env_var("XDG_STATE_HOME", &state);
        set_env_var("XDG_CONFIG_HOME", &config);
        remove_env_var("HOME");

        let paths = super::Paths::resolve_with_config_path(None).unwrap();

        assert_eq!(paths.plugin_root, data.join("tmup/plugins"));
        assert_eq!(paths.lock_path, state.join("tmup/operations.lock"));
        assert_eq!(paths.config_path, config.join("tmux/tmup.kdl"));

        restore_env("XDG_DATA_HOME", previous_data);
        restore_env("XDG_STATE_HOME", previous_state);
        restore_env("XDG_CONFIG_HOME", previous_config);
        restore_env("HOME", previous_home);
    }

    fn restore_env(key: &str, value: Option<std::ffi::OsString>) {
        match value {
            Some(value) => set_env_var(key, value),
            None => remove_env_var(key),
        }
    }

    fn set_env_var(key: &str, value: impl AsRef<std::ffi::OsStr>) {
        // Test-only environment mutation is serialized via ENV_LOCK in this module.
        unsafe { std::env::set_var(key, value) }
    }

    fn remove_env_var(key: &str) {
        // Test-only environment mutation is serialized via ENV_LOCK in this module.
        unsafe { std::env::remove_var(key) }
    }
}
