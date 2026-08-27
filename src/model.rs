use anyhow::{Context, Result, bail, ensure};

use crate::state::validate_plugin_id;

/// Top-level configuration holding global options and the list of plugins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Config {
    /// Global options that apply to all plugins.
    pub options: Options,
    /// Ordered list of plugin specifications.
    pub plugins: Vec<PluginSpec>,
}

/// Global options that control tmup behaviour.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Automatically install missing plugins on tmux startup when true.
    pub auto_install: bool,
    /// Maximum number of concurrent remote prepare jobs.
    pub concurrency: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self { auto_install: true, concurrency: 16 }
    }
}

/// Describes where a plugin originates from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginSource {
    /// Plugin hosted on a remote Git forge.
    Remote {
        /// Raw source string from config (e.g. "tmux-plugins/tmux-sensible")
        raw: String,
        /// Canonical id (e.g. "github.com/tmux-plugins/tmux-sensible")
        id: String,
        /// Resolved clone URL
        clone_url: String,
    },
    /// Plugin that lives on the local filesystem.
    Local {
        /// Absolute or home-relative path to the plugin directory.
        path: String,
    },
}

/// Specifies which Git ref a plugin should track.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Tracking {
    /// Follow the repository's default branch.
    DefaultBranch,
    /// Follow a named branch.
    Branch(String),
    /// Pin to a specific tag.
    Tag(String),
    /// Pin to a specific commit hash.
    Commit(String),
}

/// An ordered tmux global environment operation declared by a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentOperation {
    /// Set a global environment variable to a literal value.
    Set {
        /// Environment variable name.
        name: String,
        /// Literal environment variable value.
        value: String,
    },
    /// Remove a global environment variable.
    Unset {
        /// Environment variable name.
        name: String,
    },
}

/// A plugin-scoped key binding that runs a shell command from the plugin directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyBinding {
    /// Key passed to `tmux bind-key`.
    pub key: String,
    /// Ordered `bind-key` option tokens placed before the key.
    pub options: Vec<String>,
    /// Shell command evaluated when the key is pressed.
    pub shell: String,
    /// Whether `tmux run-shell` runs the command in the background.
    pub background: bool,
}

/// Full specification for a single plugin entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginSpec {
    /// Origin of the plugin (remote URL or local path).
    pub source: PluginSource,
    /// Short display name derived from the source.
    pub name: String,
    /// Prefix used when setting tmux options for this plugin.
    pub opt_prefix: String,
    /// Which Git ref to track for updates.
    pub tracking: Tracking,
    /// Optional shell command to run after installing or updating.
    pub build: Option<String>,
    /// Extra key-value options passed to the plugin.
    pub opts: Vec<(String, String)>,
    /// Ordered global tmux environment operations applied while loading the plugin.
    pub environment: Vec<EnvironmentOperation>,
    /// Ordered key bindings registered after plugin scripts load.
    pub bindings: Vec<KeyBinding>,
}

impl Config {
    /// Validate that a target id matches at least one remote plugin in config.
    pub fn validate_target_id(&self, target_id: Option<&str>) -> anyhow::Result<()> {
        if let Some(target) = target_id {
            let exists = self.plugins.iter().any(|p| p.remote_id() == Some(target));
            if !exists {
                anyhow::bail!("unknown plugin id: \"{target}\"");
            }
        }
        Ok(())
    }
}

impl PluginSpec {
    fn build_remote(
        display_raw: String,
        source: &str,
        explicit_name: Option<String>,
        opt_prefix: String,
        tracking: Tracking,
        build: Option<String>,
        opts: Vec<(String, String)>,
    ) -> Result<Self> {
        let (id, clone_url) = normalize_remote_source(source)?;
        let name =
            explicit_name.unwrap_or_else(|| id.rsplit('/').next().unwrap_or(&id).to_string());
        Ok(Self {
            source: PluginSource::Remote { raw: display_raw, id, clone_url },
            name,
            opt_prefix,
            tracking,
            build,
            opts,
            environment: Vec::new(),
            bindings: Vec::new(),
        })
    }

    /// Build a remote plugin spec from a raw source string plus resolved metadata.
    pub fn from_remote(
        raw: String,
        explicit_name: Option<String>,
        opt_prefix: String,
        tracking: Tracking,
        build: Option<String>,
        opts: Vec<(String, String)>,
    ) -> Result<Self> {
        Self::build_remote(raw.clone(), &raw, explicit_name, opt_prefix, tracking, build, opts)
    }

    /// Build a remote plugin spec from a raw TPM declaration.
    pub fn from_tpm_remote(raw: &str) -> Result<Self> {
        let (source, tracking) = match raw.rsplit_once('#') {
            Some((source, branch)) if !branch.is_empty() => {
                (source.to_string(), Tracking::Branch(branch.to_string()))
            }
            _ => (raw.to_string(), Tracking::DefaultBranch),
        };

        Self::build_remote(
            raw.to_string(),
            &source,
            None,
            String::new(),
            tracking,
            None,
            Vec::new(),
        )
    }

    /// Returns true if the plugin comes from a remote Git forge.
    pub fn is_remote(&self) -> bool {
        matches!(self.source, PluginSource::Remote { .. })
    }

    /// Returns true if the plugin resides on the local filesystem.
    pub fn is_local(&self) -> bool {
        matches!(self.source, PluginSource::Local { .. })
    }

    /// Returns the canonical remote id, or None for local plugins.
    pub fn remote_id(&self) -> Option<&str> {
        match &self.source {
            PluginSource::Remote { id, .. } => Some(id),
            PluginSource::Local { .. } => None,
        }
    }
}

fn normalize_remote_source(raw: &str) -> Result<(String, String)> {
    if let Some(rest) = raw.strip_prefix("git@") {
        let (host, path) = rest.split_once(':').context("invalid SSH URL: missing ':'")?;
        let id = normalize_remote_id(host, path)?;
        return Ok((id, raw.to_string()));
    }

    if raw.starts_with("https://") || raw.starts_with("http://") {
        let without_scheme =
            raw.strip_prefix("https://").or_else(|| raw.strip_prefix("http://")).unwrap();
        let (host, path) = without_scheme
            .split_once('/')
            .context("invalid remote URL: missing repository path")?;
        let id = normalize_remote_id(host, path)?;
        return Ok((id, raw.to_string()));
    }

    let parts: Vec<&str> = raw.split('/').collect();
    if parts.len() == 2 && !parts[0].is_empty() && !parts[1].is_empty() {
        let id = format!("github.com/{raw}");
        validate_plugin_id(&id)?;
        let clone_url = format!("https://github.com/{raw}.git");
        return Ok((id, clone_url));
    }

    bail!("cannot parse remote source: \"{raw}\"")
}

fn normalize_remote_id(host: &str, path: &str) -> Result<String> {
    ensure!(
        !host.is_empty()
            && host != "."
            && host != ".."
            && !host.contains('/')
            && !host.contains('\\'),
        "unsafe remote host: {host:?}"
    );
    let path = path.trim_end_matches('/');
    let path = path.strip_suffix(".git").unwrap_or(path);
    ensure!(!path.is_empty(), "invalid remote URL: missing repository path");
    let id = format!("{host}/{path}");
    validate_plugin_id(&id)?;
    Ok(id)
}
