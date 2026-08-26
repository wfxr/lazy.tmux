use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use kdl::{KdlDocument, KdlEntry};

use crate::model::{Config, Options, PluginSource, PluginSpec, Tracking};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Condition {
    Bool(bool),
    Shell(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PluginDeclaration {
    pub(crate) spec: PluginSpec,
    pub(crate) enabled: Condition,
    pub(crate) load_condition: Condition,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedConfig {
    pub(crate) options: Options,
    pub(crate) plugins: Vec<PluginDeclaration>,
    pub(crate) warnings: Vec<String>,
}

/// Parse a KDL-formatted configuration string into a [`Config`].
pub fn parse_config(input: &str) -> Result<Config> {
    let parsed = parse_config_document(input)?;
    Ok(Config {
        options: parsed.options,
        plugins: parsed.plugins.into_iter().map(|declaration| declaration.spec).collect(),
    })
}

pub(crate) fn parse_config_document(input: &str) -> Result<ParsedConfig> {
    let doc: KdlDocument = input.parse().context("failed to parse KDL")?;

    let options = parse_options(&doc)?;
    let mut plugins = Vec::new();
    let mut warnings = Vec::new();

    for node in doc.nodes() {
        if node.name().value() == "plugin" {
            plugins.push(parse_plugin(node, &mut warnings)?);
        }
    }

    validate_unique_ids(plugins.iter().map(|declaration| &declaration.spec))?;

    Ok(ParsedConfig { options, plugins, warnings })
}

fn parse_options(doc: &KdlDocument) -> Result<Options> {
    let mut opts = Options::default();

    let Some(node) = doc.get("options") else {
        return Ok(opts);
    };
    let Some(children) = node.children() else {
        return Ok(opts);
    };

    if let Some(v) = children.get_arg("auto-install") {
        opts.auto_install = v.as_bool().context("auto-install must be a bool")?;
    }

    if let Some(v) = children.get_arg("concurrency") {
        let value = v.as_integer().context("concurrency must be an integer")?;
        ensure!(value >= 1, "concurrency must be at least 1");
        opts.concurrency =
            usize::try_from(value).context("concurrency is too large for this platform")?;
    }

    Ok(opts)
}

fn parse_plugin(node: &kdl::KdlNode, warnings: &mut Vec<String>) -> Result<PluginDeclaration> {
    let raw = node
        .get(0)
        .and_then(|v| v.as_string())
        .context("plugin requires a source string as first argument")?
        .to_string();

    validate_plugin_properties(node, &raw, warnings)?;

    let enabled = parse_condition(node, &raw, "enabled")?.unwrap_or(Condition::Bool(true));
    let load_condition = parse_condition(node, &raw, "cond")?.unwrap_or(Condition::Bool(true));

    let is_local = get_bool(node, &raw, "local")?.unwrap_or(false);

    let explicit_name = get_string(node, &raw, "name")?;

    let opt_prefix = get_string(node, &raw, "opt-prefix")?.unwrap_or_default();

    let branch = get_string(node, &raw, "branch")?;
    let tag = get_string(node, &raw, "tag")?;
    let commit = get_string(node, &raw, "commit")?;

    let build = get_string(node, &raw, "build")?;

    // Parse tracking selector
    let selector_count = [&branch, &tag, &commit].iter().filter(|v| v.is_some()).count();
    ensure!(
        selector_count <= 1,
        "plugin \"{raw}\": branch, tag, commit are mutually exclusive (got {selector_count})"
    );

    let tracking = if let Some(b) = branch {
        Tracking::Branch(b)
    } else if let Some(t) = tag {
        Tracking::Tag(t)
    } else if let Some(c) = commit {
        Tracking::Commit(c)
    } else {
        Tracking::DefaultBranch
    };

    // Parse child nodes: opt entries and build (as child node)
    let mut opts = Vec::new();
    let mut child_build: Option<String> = None;
    if let Some(children) = node.children() {
        for child in children.nodes() {
            match child.name().value() {
                "opt" => {
                    let key = child
                        .get(0)
                        .and_then(|v| v.as_string())
                        .context("opt requires a key string")?
                        .to_string();
                    let value = child
                        .get(1)
                        .and_then(|v| v.as_string())
                        .context("opt requires a value string")?
                        .to_string();
                    opts.push((key, value));
                }
                "build" => {
                    ensure!(
                        child_build.is_none(),
                        "plugin \"{raw}\": build child node may only be specified once"
                    );
                    child_build = Some(
                        child
                            .get(0)
                            .and_then(|v| v.as_string())
                            .context("build child node requires a command string")?
                            .to_string(),
                    );
                }
                "enabled" => {
                    bail!(
                        "plugin \"{raw}\": enabled child form is reserved; use enabled=#true, enabled=#false, or enabled=\"shell predicate\""
                    );
                }
                "cond" => {
                    bail!(
                        "plugin \"{raw}\": cond child form is reserved; use cond=#true, cond=#false, or cond=\"shell predicate\""
                    );
                }
                unknown => {
                    warnings.push(format!("plugin \"{raw}\": ignoring unknown child \"{unknown}\""))
                }
            }
        }
    }

    ensure!(
        !(build.is_some() && child_build.is_some()),
        "plugin \"{raw}\": build specified both as property and child node"
    );
    let build = build.or(child_build);

    let source = if is_local {
        let expanded_path = expand_local_path(&raw)?;
        ensure!(
            matches!(tracking, Tracking::DefaultBranch),
            "local plugin \"{raw}\": branch/tag/commit not allowed for local plugins"
        );
        ensure!(
            Path::new(&expanded_path).is_absolute(),
            "plugin \"{raw}\": local path must expand to an absolute path (got {expanded_path})"
        );
        let name = explicit_name.unwrap_or_else(|| {
            Path::new(&expanded_path)
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| expanded_path.clone())
        });
        PluginSpec {
            source: PluginSource::Local { path: expanded_path },
            name,
            opt_prefix,
            tracking,
            build,
            opts,
        }
    } else {
        PluginSpec::from_remote(raw, explicit_name, opt_prefix, tracking, build, opts)?
    };

    Ok(PluginDeclaration { spec: source, enabled, load_condition })
}

pub(crate) fn validate_unique_ids<'a>(
    plugins: impl IntoIterator<Item = &'a PluginSpec>,
) -> Result<()> {
    let mut seen = HashSet::new();
    for p in plugins {
        if let Some(id) = p.remote_id()
            && !seen.insert(id.to_string())
        {
            bail!("duplicate remote plugin id: \"{id}\"");
        }
    }
    Ok(())
}

fn validate_plugin_properties(
    node: &kdl::KdlNode,
    plugin: &str,
    warnings: &mut Vec<String>,
) -> Result<()> {
    const KNOWN_PROPERTIES: &[&str] =
        &["local", "name", "opt-prefix", "branch", "tag", "commit", "build", "enabled", "cond"];

    for (positional_index, _) in
        node.entries().iter().filter(|entry| entry.name().is_none()).enumerate().skip(1)
    {
        warnings.push(format!(
            "plugin \"{plugin}\": ignoring extra positional parameter at index {positional_index}"
        ));
    }

    let mut property_counts: HashMap<&str, usize> = HashMap::new();
    for name in node.entries().iter().filter_map(|entry| entry.name().map(|name| name.value())) {
        if KNOWN_PROPERTIES.contains(&name) {
            let count = property_counts.entry(name).or_default();
            *count += 1;
            ensure!(*count == 1, "plugin \"{plugin}\": {name} may only be specified once");
        } else {
            warnings.push(format!("plugin \"{plugin}\": ignoring unknown property \"{name}\""));
        }
    }
    Ok(())
}

fn parse_condition(node: &kdl::KdlNode, plugin: &str, key: &str) -> Result<Option<Condition>> {
    let Some(entry) = property_entry(node, key) else {
        return Ok(None);
    };
    ensure!(
        entry.ty().is_none(),
        "plugin \"{plugin}\": {key} does not support KDL type annotations"
    );
    if let Some(value) = entry.value().as_bool() {
        return Ok(Some(Condition::Bool(value)));
    }
    if let Some(value) = entry.value().as_string() {
        ensure!(
            !value.trim().is_empty(),
            "plugin \"{plugin}\": {key} shell predicate must not be empty or whitespace-only"
        );
        return Ok(Some(Condition::Shell(value.to_string())));
    }
    bail!("plugin \"{plugin}\": {key} must be a bool or shell predicate string")
}

fn property_entry<'a>(node: &'a kdl::KdlNode, key: &str) -> Option<&'a KdlEntry> {
    node.entries().iter().find(|entry| entry.name().is_some_and(|name| name.value() == key))
}

/// Extract an optional string property, erroring if the property exists but is not a string.
fn get_string(node: &kdl::KdlNode, plugin: &str, key: &str) -> Result<Option<String>> {
    match node.get(key) {
        None => Ok(None),
        Some(v) => match v.as_string() {
            Some(s) => Ok(Some(s.to_string())),
            None => bail!("plugin \"{plugin}\": {key} must be a string"),
        },
    }
}

/// Extract an optional bool property, erroring if the property exists but is not a bool.
fn get_bool(node: &kdl::KdlNode, plugin: &str, key: &str) -> Result<Option<bool>> {
    match node.get(key) {
        None => Ok(None),
        Some(v) => match v.as_bool() {
            Some(b) => Ok(Some(b)),
            None => bail!("plugin \"{plugin}\": {key} must be a bool"),
        },
    }
}

fn expand_local_path(raw: &str) -> Result<String> {
    let expanded = shellexpand::full(raw)
        .with_context(|| format!("failed to expand local path: {raw}"))?
        .into_owned();
    Ok(expanded)
}
