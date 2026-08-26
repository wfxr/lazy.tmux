use std::fmt;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::ValueEnum;

use crate::config::{Condition, ParsedConfig, PluginDeclaration};
use crate::model::Config;
use crate::state::Paths;
use crate::{config, config_tpm};

mod condition;

/// Supported configuration loading modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "lower")]
pub enum ConfigMode {
    /// Load only tmup.kdl.
    Pure,
    /// Load both config sources and merge them.
    Mixed,
}

/// Policy for handling the primary tmup.kdl during config load.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TmupConfigPolicy {
    /// Never write tmup.kdl; use an in-memory default when it is missing.
    ReadOnly,
    /// Create the default tmup.kdl on disk when it is missing.
    CreateIfMissing,
}

/// Policy for resolving the optional TPM-compatible tmux config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TpmConfigPolicy {
    /// Do not load any TPM-compatible tmux config.
    Disabled,
    /// Discover the TPM-compatible config using the default search order.
    Discover,
    /// Use an already-resolved discovery result, including an explicit "not found".
    Resolved(Option<PathBuf>),
}

/// Complete request describing how the effective configuration should be loaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadRequest {
    /// High-level configuration mode to load.
    pub mode: ConfigMode,
    /// How the primary tmup.kdl should be handled if it is missing.
    pub tmup_policy: TmupConfigPolicy,
    /// How the optional TPM-compatible tmux config should be sourced.
    pub tpm_policy: TpmConfigPolicy,
}

impl LoadRequest {
    /// Build a request from CLI/runtime loading intent.
    pub fn from_command(
        mode: ConfigMode,
        create_missing: bool,
        tpm_policy: TpmConfigPolicy,
    ) -> Self {
        let tmup_policy = if create_missing {
            TmupConfigPolicy::CreateIfMissing
        } else {
            TmupConfigPolicy::ReadOnly
        };
        Self { mode, tmup_policy, tpm_policy }
    }
}

impl fmt::Display for ConfigMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pure => f.write_str("pure"),
            Self::Mixed => f.write_str("mixed"),
        }
    }
}

/// Loaded configuration plus non-fatal warnings encountered during load.
#[derive(Debug)]
pub struct LoadedConfig {
    /// The normalized effective configuration.
    pub config: Config,
    /// Warnings emitted while loading or merging config sources.
    pub warnings: Vec<String>,
    /// The resolved primary config path that should own the active lockfile.
    pub active_config_path: PathBuf,
    /// The resolved TPM config path when mixed mode discovered one.
    pub tpm_config_path: Option<PathBuf>,
}

/// Result of loading config for a concrete runtime request, including finalized paths.
#[derive(Debug)]
pub struct LoadedRequest {
    /// The normalized effective configuration.
    pub config: Config,
    /// Warnings emitted while loading or merging config sources.
    pub warnings: Vec<String>,
    /// Runtime paths retargeted to the resolved active config and lockfile pair.
    pub paths: Paths,
    /// The resolved TPM loading policy for downstream consumers such as init children.
    pub tpm_policy: TpmConfigPolicy,
    /// The resolved TPM config path when mixed mode discovered one.
    pub tpm_config_path: Option<PathBuf>,
}

/// Ensure the active tmup.kdl exists on disk using the default template.
pub fn ensure_tmup_config_exists(paths: &Paths) -> Result<()> {
    create_default_tmup_config(&paths.config_path)
}

/// Load configuration for the requested mode from explicit source paths.
pub fn load_from_sources(
    mode: ConfigMode,
    tmup_path: Option<&Path>,
    tpm_path: Option<&Path>,
) -> Result<LoadedConfig> {
    match mode {
        ConfigMode::Pure => {
            let path = tmup_path.context("tmup config file not found")?;
            resolve_loaded_config(load_tmup_config(path)?, path, None)
        }
        ConfigMode::Mixed => load_mixed(tmup_path, tpm_path),
    }
}

/// Load configuration according to an explicit request and finalize its runtime paths.
pub fn load_with_request(paths: &Paths, request: LoadRequest) -> Result<LoadedRequest> {
    let tmup_path = prepare_tmup_config_path(paths, request.tmup_policy)?;
    let (loaded, tpm_policy): (LoadedConfig, TpmConfigPolicy) = match request.mode {
        ConfigMode::Pure => (
            {
                let parsed = load_tmup_config_for_policy(&tmup_path, request.tmup_policy)?;
                resolve_loaded_config(parsed, &tmup_path, None)?
            },
            TpmConfigPolicy::Disabled,
        ),
        ConfigMode::Mixed => {
            let (tpm_path, warnings) = resolve_tpm_config_path(request.tpm_policy)?;
            let mut loaded = load_from_sources(
                ConfigMode::Mixed,
                Some(tmup_path.as_path()),
                tpm_path.as_deref(),
            )?;
            loaded.warnings.extend(warnings);
            (loaded, TpmConfigPolicy::Resolved(tpm_path))
        }
    };

    let finalized_paths = paths.with_config_path(loaded.active_config_path.clone())?;
    Ok(LoadedRequest {
        config: loaded.config,
        warnings: loaded.warnings,
        paths: finalized_paths,
        tpm_policy,
        tpm_config_path: loaded.tpm_config_path,
    })
}

fn load_mixed(tmup_path: Option<&Path>, tpm_path: Option<&Path>) -> Result<LoadedConfig> {
    let tmup_path = tmup_path.context("tmup config file not found")?;
    let tpm_config = tpm_path.map(config_tpm::load_config_from_path).transpose()?;
    let tmup_config = load_tmup_config_or_default(tmup_path)?;

    let parsed = match tpm_config {
        Some(tpm) => merge_configs(tmup_config, tpm),
        None => tmup_config,
    };
    resolve_loaded_config(parsed, tmup_path, tpm_path.map(Path::to_path_buf))
}

fn resolve_loaded_config(
    parsed: ParsedConfig,
    active_config_path: &Path,
    tpm_config_path: Option<PathBuf>,
) -> Result<LoadedConfig> {
    let working_dir = active_config_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let ParsedConfig { options, plugins, warnings } = parsed;
    let plugins = condition::project_enabled_plugins(plugins, working_dir)?;
    Ok(LoadedConfig {
        config: Config { options, plugins },
        warnings,
        active_config_path: active_config_path.to_path_buf(),
        tpm_config_path,
    })
}

fn resolve_tpm_config_path(policy: TpmConfigPolicy) -> Result<(Option<PathBuf>, Vec<String>)> {
    match policy {
        TpmConfigPolicy::Disabled => Ok((None, Vec::new())),
        TpmConfigPolicy::Discover => {
            let resolved = config_tpm::resolve_config_path()?;
            Ok((resolved.path, resolved.warnings))
        }
        TpmConfigPolicy::Resolved(path) => Ok((path, Vec::new())),
    }
}

fn load_tmup_config(path: &Path) -> Result<ParsedConfig> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read tmup config: {}", path.display()))?;
    config::parse_config_document(&content)
}

fn load_tmup_config_or_default(path: &Path) -> Result<ParsedConfig> {
    if path.exists() { load_tmup_config(path) } else { default_tmup_config() }
}

fn load_tmup_config_for_policy(path: &Path, policy: TmupConfigPolicy) -> Result<ParsedConfig> {
    match policy {
        TmupConfigPolicy::ReadOnly => load_tmup_config_or_default(path),
        TmupConfigPolicy::CreateIfMissing => load_tmup_config(path),
    }
}

fn merge_configs(mut kdl: ParsedConfig, tpm: Config) -> ParsedConfig {
    // Mixed mode preserves TPM-discovered order for TPM entries, while KDL-only
    // entries are appended afterward in their original KDL order. Conflicting
    // remote ids keep the TPM slot but use the KDL declaration.
    let mut merged = Vec::with_capacity(tpm.plugins.len() + kdl.plugins.len());
    let mut kdl_remote_indices = std::collections::HashMap::new();

    for (index, declaration) in kdl.plugins.iter().enumerate() {
        if let Some(id) = declaration.spec.remote_id() {
            kdl_remote_indices.insert(id.to_string(), index);
        }
    }

    let mut consumed_kdl = vec![false; kdl.plugins.len()];
    for plugin in tpm.plugins {
        let Some(id) = plugin.remote_id() else {
            // TPM scan only yields remote plugins today. Keep this defensive
            // skip in case future callers hand us already-parsed mixed input.
            continue;
        };

        if let Some(&index) = kdl_remote_indices.get(id) {
            merged.push(kdl.plugins[index].clone());
            consumed_kdl[index] = true;
            kdl.warnings.push(format!(
                "plugin \"{id}\" declared in both tmup.kdl and TPM config; using tmup.kdl entry"
            ));
        } else {
            merged.push(PluginDeclaration { spec: plugin, enabled: Condition::Bool(true) });
        }
    }

    for (index, plugin) in kdl.plugins.drain(..).enumerate() {
        if !consumed_kdl[index] {
            merged.push(plugin);
        }
    }

    debug_assert!(config::validate_unique_ids(merged.iter().map(|entry| &entry.spec)).is_ok());
    ParsedConfig { options: kdl.options, plugins: merged, warnings: kdl.warnings }
}

fn prepare_tmup_config_path(paths: &Paths, policy: TmupConfigPolicy) -> Result<PathBuf> {
    let path = paths.config_path.clone();
    if matches!(policy, TmupConfigPolicy::CreateIfMissing) {
        create_default_tmup_config(&path)?;
    }
    Ok(path)
}

fn create_default_tmup_config(path: &Path) -> Result<()> {
    let parent = path
        .parent()
        .with_context(|| format!("config path has no parent directory: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create config directory: {}", parent.display()))?;
    let mut file = match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => return Ok(()),
        Err(err) => {
            return Err(err)
                .with_context(|| format!("failed to create config: {}", path.display()));
        }
    };
    use std::io::Write;
    file.write_all(default_tmup_config_template().as_bytes())
        .with_context(|| format!("failed to write config: {}", path.display()))?;
    file.flush().with_context(|| format!("failed to flush config: {}", path.display()))?;
    Ok(())
}

fn default_tmup_config_template() -> &'static str {
    r#"// tmup configuration
// Add plugins here, for example:
// plugin "tmux-plugins/tmux-sensible"
//
// If you are migrating from TPM, you can set:
// TMUP_CONFIG_MODE=mixed

options {
    auto-install #true
    concurrency 16
}
"#
}

fn default_tmup_config() -> Result<ParsedConfig> {
    config::parse_config_document(default_tmup_config_template())
        .context("internal default tmup config invalid")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    #[test]
    fn create_default_tmup_config_does_not_overwrite_existing_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tmup.kdl");
        std::fs::write(&path, "plugin \"user/custom\"\n").unwrap();

        super::create_default_tmup_config(&path).unwrap();

        assert_eq!(std::fs::read_to_string(&path).unwrap(), "plugin \"user/custom\"\n");
    }
}
