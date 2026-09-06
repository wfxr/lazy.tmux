use std::cmp::Ordering;
use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result, bail, ensure};
use semver::Version;

mod install;
mod installer;
mod process;

/// Selection and takeover policy for an explicit tmup upgrade.
#[derive(Debug, Default, clap::Args)]
pub struct Options {
    /// Include prereleases when selecting the latest release
    #[arg(long, conflicts_with = "version")]
    pub pre: bool,
    /// Install an exact release version, allowing a deliberate downgrade
    #[arg(long, value_name = "VERSION")]
    pub version: Option<String>,
    /// Replace custom builds and reinstall even when the version is unchanged
    #[arg(long)]
    pub force: bool,
}

/// A completed upgrade or an explained successful no-op.
#[derive(Debug)]
pub struct Outcome {
    message: String,
}

impl fmt::Display for Outcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

#[derive(Clone, Copy)]
struct Budgets {
    bootstrap: Duration,
    resolve: Duration,
    prepare: Duration,
    smoke: Duration,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            bootstrap: Duration::from_secs(120),
            resolve: Duration::from_secs(60),
            prepare: Duration::from_secs(300),
            smoke: Duration::from_secs(5),
        }
    }
}

/// Upgrade the OS-resolved running executable without reading plugin configuration.
pub fn run(options: Options) -> Result<Outcome> {
    let current = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let official = option_env!("TMUP_OFFICIAL_RELEASE") == Some("1");
    validate_options(&options, official)?;
    let target = target(std::env::consts::OS, std::env::consts::ARCH)?;
    let destination = std::env::current_exe()
        .context("cannot locate the running executable")?
        .canonicalize()
        .context("cannot resolve the running executable's real path")?;
    execute(options, current, target, destination, Budgets::default())
}

fn validate_options(options: &Options, official: bool) -> Result<()> {
    ensure!(!(options.pre && options.version.is_some()), "--pre conflicts with --version");
    if let Some(version) = &options.version {
        release_version(version)?;
    }
    if !official {
        ensure!(
            options.force,
            "this is not an official release build; use --force to replace it with an official binary"
        );
        eprintln!(
            "warning: --force will replace this custom build with an official binary; custom build choices will be lost and another installer's records may disagree"
        );
    }
    Ok(())
}

fn execute(
    options: Options,
    current: Version,
    target: &str,
    destination: PathBuf,
    budgets: Budgets,
) -> Result<Outcome> {
    let destination = install::Destination::capture(destination)?;
    let mut lock = destination.lock()?;
    let _guard = lock
        .try_write()
        .context("cannot lock executable; another tmup upgrade may be in progress")?;
    destination.check_unchanged()?;
    let workspace = tempfile::Builder::new()
        .prefix("tmup-upgrade-")
        .tempdir()
        .context("cannot create upgrade workspace in system temporary directory")?;
    let mut candidate = None;
    let mut published = false;
    let result = (|| {
        let script = installer::download(workspace.path(), budgets.bootstrap)?;
        let selected = installer::resolve(&script, workspace.path(), &options, budgets.resolve)?;
        if let Some(requested) = &options.version {
            ensure!(
                release_version(requested)? == selected,
                "installer resolved a different version than requested"
            );
        }
        if let Some(reason) = skip_reason(&current, &selected, &options) {
            return Ok(Outcome { message: reason });
        }
        installer::prepare(&script, workspace.path(), &selected, target, budgets.prepare)?;
        candidate = Some(destination.copy_candidate(&workspace.path().join("prepared/tmup"))?);
        destination.verify_candidate(
            candidate.as_ref().unwrap(),
            &selected,
            workspace.path(),
            budgets.smoke,
        )?;
        destination.publish(&mut candidate)?;
        published = true;
        Ok(Outcome {
            message: format!("Upgraded tmup to {selected} at {}", destination.path.display()),
        })
    })();
    let mut cleanup_errors = Vec::new();
    if let Some(candidate) = candidate {
        let path = candidate.to_path_buf();
        if let Err(error) = candidate.close() {
            cleanup_errors.push(format!("{}: {error}", path.display()));
        }
    }
    let path = workspace.path().to_owned();
    if let Err(error) = workspace.close() {
        cleanup_errors.push(format!("{}: {error}", path.display()));
    }
    finish_cleanup(result, cleanup_errors, published, &destination.path)
}

fn finish_cleanup(
    result: Result<Outcome>,
    errors: Vec<String>,
    published: bool,
    destination: &std::path::Path,
) -> Result<Outcome> {
    if errors.is_empty() {
        return result;
    }
    let state = if published {
        format!("tmup was already replaced at {}", destination.display())
    } else {
        "the installed executable was not replaced".into()
    };
    let cleanup =
        format!("temporary cleanup failed ({state}); remaining paths: {}", errors.join("; "));
    match result {
        Ok(_) => bail!("{cleanup}"),
        Err(error) => Err(error.context(cleanup)),
    }
}

fn release_version(input: &str) -> Result<Version> {
    let version = Version::parse(input.strip_prefix('v').unwrap_or(input))
        .with_context(|| format!("invalid release version: {input:?}"))?;
    ensure!(version.build.is_empty(), "release build metadata is not supported");
    Ok(version)
}

fn skip_reason(current: &Version, selected: &Version, options: &Options) -> Option<String> {
    match selected.cmp_precedence(current) {
        Ordering::Equal if !options.force => Some(format!("tmup {current} is already installed")),
        Ordering::Less if options.version.is_none() => Some(format!(
            "Keeping tmup {current}: selected release {selected} is older; use --version {selected} to downgrade"
        )),
        _ => None,
    }
}

fn target(os: &str, arch: &str) -> Result<&'static str> {
    match (os, arch) {
        ("linux", "x86_64") => Ok("x86_64-unknown-linux-musl"),
        ("linux", "aarch64") => Ok("aarch64-unknown-linux-musl"),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin"),
        ("macos", "aarch64") => Ok("aarch64-apple-darwin"),
        _ => bail!("tmup upgrade does not support {os}/{arch}"),
    }
}

#[cfg(test)]
mod tests;
