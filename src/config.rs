use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{Context, Result, bail, ensure};
use kdl::{KdlDocument, KdlEntry};

use crate::model::{
    EnvironmentOperation, KeyBinding, Options, PluginSource, PluginSpec, RuntimeConfiguration,
    Tracking,
};

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
    pub(crate) runtime: Vec<RuntimeDeclaration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RuntimeDeclaration {
    Option {
        key: String,
        value: String,
    },
    Environment(EnvironmentOperation),
    Binding(KeyBinding),
    Branch {
        condition: Condition,
        then_declarations: Vec<RuntimeDeclaration>,
        else_declarations: Vec<RuntimeDeclaration>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParsedConfig {
    pub(crate) options: Options,
    pub(crate) plugins: Vec<PluginDeclaration>,
    pub(crate) warnings: Vec<String>,
}

pub(crate) fn parse_config_document(input: &str) -> Result<ParsedConfig> {
    let doc: KdlDocument = input.parse().context("failed to parse KDL")?;

    let mut options = None;
    let mut plugins = Vec::new();
    let warnings = Vec::new();

    for node in doc.nodes() {
        match node.name().value() {
            "options" => {
                ensure!(options.is_none(), "options may only be specified once");
                options = Some(parse_options(node)?);
            }
            "plug" => plugins.push(parse_plugin(node)?),
            unknown => bail!("unknown root node \"{unknown}\""),
        }
    }

    validate_unique_ids(plugins.iter().map(|declaration| &declaration.spec))?;

    Ok(ParsedConfig { options: options.unwrap_or_default(), plugins, warnings })
}

fn parse_options(node: &kdl::KdlNode) -> Result<Options> {
    ensure!(node.ty().is_none(), "options does not support KDL type annotations");
    ensure!(node.entries().is_empty(), "options must not have arguments or properties");
    let children = node.children().context("options requires a child block")?;
    let mut opts = Options::default();
    let mut auto_install_seen = false;
    let mut concurrency_seen = false;

    for child in children.nodes() {
        match child.name().value() {
            "auto-install" => {
                ensure!(!auto_install_seen, "options.auto-install may only be specified once");
                auto_install_seen = true;
                validate_option_child(child, "auto-install", "bool")?;
                opts.auto_install = child.entries()[0]
                    .value()
                    .as_bool()
                    .context("options.auto-install must be a bool")?;
            }
            "concurrency" => {
                ensure!(!concurrency_seen, "options.concurrency may only be specified once");
                concurrency_seen = true;
                validate_option_child(child, "concurrency", "integer")?;
                let value = child.entries()[0]
                    .value()
                    .as_integer()
                    .context("options.concurrency must be an integer")?;
                ensure!(value >= 1, "concurrency must be at least 1");
                opts.concurrency =
                    usize::try_from(value).context("concurrency is too large for this platform")?;
            }
            unknown => bail!("unknown options child \"{unknown}\""),
        }
    }

    Ok(opts)
}

fn validate_option_child(node: &kdl::KdlNode, name: &str, value_type: &str) -> Result<()> {
    ensure!(node.ty().is_none(), "options.{name} does not support KDL type annotations");
    ensure!(node.children().is_none(), "options.{name} must not have child nodes");
    ensure!(
        node.entries().iter().all(|entry| entry.name().is_none()),
        "options.{name} must not have properties"
    );
    ensure!(node.entries().len() == 1, "options.{name} requires exactly one {value_type} argument");
    ensure!(
        node.entries()[0].ty().is_none(),
        "options.{name} does not support KDL type annotations"
    );
    Ok(())
}

fn parse_plugin(node: &kdl::KdlNode) -> Result<PluginDeclaration> {
    ensure!(node.ty().is_none(), "plugin does not support KDL type annotations");
    let positional: Vec<_> = node.entries().iter().filter(|entry| entry.name().is_none()).collect();
    ensure!(positional.len() == 1, "plugin requires exactly one source string argument");
    ensure!(positional[0].ty().is_none(), "plugin source does not support KDL type annotations");
    let raw = positional[0]
        .value()
        .as_string()
        .context("plugin requires exactly one source string argument")?
        .to_string();
    ensure!(!raw.trim().is_empty(), "plugin source must not be empty or whitespace-only");

    validate_plugin_properties(node, &raw)?;

    let enabled = parse_condition(node, &raw, "enabled")?.unwrap_or(Condition::Bool(true));
    let load_condition = parse_condition(node, &raw, "cond")?.unwrap_or(Condition::Bool(true));

    let is_local = get_bool(node, &raw, "local")?.unwrap_or(false);

    let explicit_name = get_non_empty_string(node, &raw, "name")?;

    let opt_prefix = get_string(node, &raw, "opt-prefix")?.unwrap_or_default();

    let branch = get_non_empty_string(node, &raw, "branch")?;
    let tag = get_non_empty_string(node, &raw, "tag")?;
    let commit = get_non_empty_string(node, &raw, "commit")?;

    let build = get_non_empty_string(node, &raw, "build")?;

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

    let runtime = node
        .children()
        .map(|children| {
            parse_runtime_declaration_sequence(
                children.nodes(),
                &raw,
                RuntimeDeclarationContext::Plugin,
            )
        })
        .transpose()?
        .unwrap_or_default();

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
            runtime: RuntimeConfiguration::Unresolved,
        }
    } else {
        PluginSpec::from_remote(raw, explicit_name, opt_prefix, tracking, build)?
    };

    Ok(PluginDeclaration { spec: source, enabled, load_condition, runtime })
}

fn parse_runtime_branch(
    if_node: &kdl::KdlNode,
    next_node: Option<&kdl::KdlNode>,
    plugin: &str,
) -> Result<(RuntimeDeclaration, bool)> {
    let condition = parse_runtime_branch_condition(if_node, plugin)?;
    let then_declarations = parse_runtime_branch_children(if_node, plugin)?;
    let else_node = next_node.filter(|node| node.name().value() == "else");
    let else_declarations = else_node
        .map(|node| {
            ensure!(
                node.ty().is_none(),
                "plugin \"{plugin}\": else does not support KDL type annotations"
            );
            ensure!(
                node.entries().is_empty(),
                "plugin \"{plugin}\": else must not have arguments or properties"
            );
            parse_runtime_branch_children(node, plugin)
        })
        .transpose()?
        .unwrap_or_default();
    Ok((
        RuntimeDeclaration::Branch { condition, then_declarations, else_declarations },
        else_node.is_some(),
    ))
}

fn parse_runtime_branch_condition(node: &kdl::KdlNode, plugin: &str) -> Result<Condition> {
    ensure!(node.ty().is_none(), "plugin \"{plugin}\": if does not support KDL type annotations");
    ensure!(
        node.entries().iter().all(|entry| entry.name().is_none()),
        "plugin \"{plugin}\": if must not have properties"
    );
    ensure!(node.entries().len() == 1, "plugin \"{plugin}\": if requires exactly one condition");
    let entry = &node.entries()[0];
    ensure!(entry.ty().is_none(), "plugin \"{plugin}\": if does not support KDL type annotations");
    if let Some(value) = entry.value().as_bool() {
        return Ok(Condition::Bool(value));
    }
    if let Some(value) = entry.value().as_string() {
        ensure!(
            !value.trim().is_empty(),
            "plugin \"{plugin}\": if shell predicate must not be empty or whitespace-only"
        );
        return Ok(Condition::Shell(value.to_string()));
    }
    bail!("plugin \"{plugin}\": if condition must be a bool or shell predicate string")
}

fn parse_runtime_branch_children(
    node: &kdl::KdlNode,
    plugin: &str,
) -> Result<Vec<RuntimeDeclaration>> {
    let children = node
        .children()
        .with_context(|| format!("plugin \"{plugin}\": {} requires a child block", node.name()))?;
    parse_runtime_declaration_sequence(children.nodes(), plugin, RuntimeDeclarationContext::Branch)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RuntimeDeclarationContext {
    Plugin,
    Branch,
}

fn parse_runtime_declaration_sequence(
    nodes: &[kdl::KdlNode],
    plugin: &str,
    context: RuntimeDeclarationContext,
) -> Result<Vec<RuntimeDeclaration>> {
    let mut declarations = Vec::new();
    let mut index = 0;
    while index < nodes.len() {
        let child = &nodes[index];
        match child.name().value() {
            "opt" => {
                let (key, value) = parse_plugin_option(child, plugin)?;
                declarations.push(RuntimeDeclaration::Option { key, value });
            }
            "env" | "unset-env" => declarations
                .push(RuntimeDeclaration::Environment(parse_environment_operation(child, plugin)?)),
            "bind" => {
                declarations.push(RuntimeDeclaration::Binding(parse_key_binding(child, plugin)?))
            }
            "if" => {
                let (branch, consumed_else) =
                    parse_runtime_branch(child, nodes.get(index + 1), plugin)?;
                declarations.push(branch);
                if consumed_else {
                    index += 1;
                }
            }
            "else" => bail!("plugin \"{plugin}\": else must immediately follow an if node"),
            "enabled" if matches!(context, RuntimeDeclarationContext::Plugin) => bail!(
                "plugin \"{plugin}\": enabled child form is reserved; use enabled=#true, enabled=#false, or enabled=\"shell predicate\""
            ),
            "cond" if matches!(context, RuntimeDeclarationContext::Plugin) => bail!(
                "plugin \"{plugin}\": cond child form is reserved; use cond=#true, cond=#false, or cond=\"shell predicate\""
            ),
            unknown if matches!(context, RuntimeDeclarationContext::Plugin) => {
                bail!("plugin \"{plugin}\": unknown child \"{unknown}\"")
            }
            unknown => bail!(
                "plugin \"{plugin}\": runtime configuration branch only allows opt, env, unset-env, bind, and nested if nodes (found \"{unknown}\")"
            ),
        }
        index += 1;
    }
    Ok(declarations)
}

fn parse_plugin_option(node: &kdl::KdlNode, plugin: &str) -> Result<(String, String)> {
    ensure!(node.ty().is_none(), "plugin \"{plugin}\": opt does not support KDL type annotations");
    ensure!(node.children().is_none(), "plugin \"{plugin}\": opt must not have child nodes");
    ensure!(
        node.entries().iter().all(|entry| entry.name().is_none()),
        "plugin \"{plugin}\": opt must not have properties"
    );
    ensure!(
        node.entries().len() == 2,
        "plugin \"{plugin}\": opt requires exactly 2 string arguments"
    );
    ensure!(
        node.entries().iter().all(|entry| entry.ty().is_none()),
        "plugin \"{plugin}\": opt does not support KDL type annotations"
    );
    let key = node
        .get(0)
        .and_then(|value| value.as_string())
        .with_context(|| format!("plugin \"{plugin}\": opt requires a key string"))?
        .to_string();
    ensure!(
        !key.trim().is_empty(),
        "plugin \"{plugin}\": opt key must not be empty or whitespace-only"
    );
    let value = node
        .get(1)
        .and_then(|value| value.as_string())
        .with_context(|| format!("plugin \"{plugin}\": opt requires a value string"))?
        .to_string();
    Ok((key, value))
}

fn parse_key_binding(node: &kdl::KdlNode, plugin: &str) -> Result<KeyBinding> {
    ensure!(node.ty().is_none(), "plugin \"{plugin}\": bind does not support KDL type annotations");
    ensure!(
        node.entries().iter().all(|entry| entry.name().is_none()),
        "plugin \"{plugin}\": bind must not have properties"
    );
    ensure!(
        node.entries().len() == 1,
        "plugin \"{plugin}\": bind requires exactly 1 key string argument"
    );
    ensure!(
        node.entries()[0].ty().is_none(),
        "plugin \"{plugin}\": bind does not support KDL type annotations"
    );
    let key = node.entries()[0]
        .value()
        .as_string()
        .with_context(|| format!("plugin \"{plugin}\": bind key must be a string"))?
        .to_string();
    ensure!(
        !key.trim().is_empty(),
        "plugin \"{plugin}\": bind key must not be empty or whitespace-only"
    );
    let children = node
        .children()
        .with_context(|| format!("plugin \"{plugin}\": bind requires a child block"))?;
    let mut options = None;
    let mut shell = None;
    for child in children.nodes() {
        match child.name().value() {
            "options" => {
                ensure!(
                    options.is_none(),
                    "plugin \"{plugin}\": bind options child may only be specified once"
                );
                options = Some(parse_bind_options(child, plugin)?);
            }
            "shell" => {
                ensure!(
                    shell.is_none(),
                    "plugin \"{plugin}\": bind requires exactly one shell child"
                );
                shell = Some(parse_bind_shell(child, plugin)?);
            }
            unknown => bail!("plugin \"{plugin}\": unknown bind child \"{unknown}\""),
        }
    }
    let (shell, background) = shell
        .with_context(|| format!("plugin \"{plugin}\": bind requires exactly one shell child"))?;

    Ok(KeyBinding { key, options: options.unwrap_or_default(), shell, background })
}

fn parse_bind_options(node: &kdl::KdlNode, plugin: &str) -> Result<Vec<String>> {
    ensure!(
        node.ty().is_none(),
        "plugin \"{plugin}\": bind options does not support KDL type annotations"
    );
    ensure!(
        node.children().is_none(),
        "plugin \"{plugin}\": bind options must not have child nodes"
    );
    ensure!(
        node.entries().iter().all(|entry| entry.name().is_none()),
        "plugin \"{plugin}\": bind options must not have properties"
    );
    ensure!(
        !node.entries().is_empty(),
        "plugin \"{plugin}\": bind options requires at least 1 non-empty string"
    );
    ensure!(
        node.entries().iter().all(|entry| entry.ty().is_none()),
        "plugin \"{plugin}\": bind options does not support KDL type annotations"
    );
    node.entries()
        .iter()
        .map(|entry| {
            let option = entry
                .value()
                .as_string()
                .with_context(|| format!("plugin \"{plugin}\": bind options must be strings"))?;
            ensure!(
                !option.trim().is_empty(),
                "plugin \"{plugin}\": bind option strings must not be empty or whitespace-only"
            );
            Ok(option.to_string())
        })
        .collect()
}

fn parse_bind_shell(node: &kdl::KdlNode, plugin: &str) -> Result<(String, bool)> {
    ensure!(
        node.ty().is_none(),
        "plugin \"{plugin}\": bind shell does not support KDL type annotations"
    );
    ensure!(node.children().is_none(), "plugin \"{plugin}\": bind shell must not have child nodes");
    let positional: Vec<_> = node.entries().iter().filter(|entry| entry.name().is_none()).collect();
    ensure!(positional.len() == 1, "plugin \"{plugin}\": shell requires exactly 1 command string");
    ensure!(
        positional[0].ty().is_none(),
        "plugin \"{plugin}\": bind shell does not support KDL type annotations"
    );
    let shell = positional[0]
        .value()
        .as_string()
        .with_context(|| format!("plugin \"{plugin}\": shell command must be a string"))?
        .to_string();
    ensure!(
        !shell.trim().is_empty(),
        "plugin \"{plugin}\": shell command must not be empty or whitespace-only"
    );

    let mut background = false;
    let mut background_seen = false;
    for entry in node.entries().iter().filter(|entry| entry.name().is_some()) {
        let name = entry.name().expect("filtered to named entries").value();
        ensure!(name == "background", "plugin \"{plugin}\": unknown shell property \"{name}\"");
        ensure!(
            !background_seen,
            "plugin \"{plugin}\": shell background may only be specified once"
        );
        ensure!(
            entry.ty().is_none(),
            "plugin \"{plugin}\": shell background does not support KDL type annotations"
        );
        background = entry
            .value()
            .as_bool()
            .with_context(|| format!("plugin \"{plugin}\": shell background must be a bool"))?;
        background_seen = true;
    }

    Ok((shell, background))
}

fn parse_environment_operation(node: &kdl::KdlNode, plugin: &str) -> Result<EnvironmentOperation> {
    let kind = node.name().value();
    let argument_count = match kind {
        "env" => 2,
        "unset-env" => 1,
        _ => unreachable!("only recognized environment nodes are parsed here"),
    };
    ensure!(
        node.ty().is_none(),
        "plugin \"{plugin}\": {kind} does not support KDL type annotations"
    );
    ensure!(node.children().is_none(), "plugin \"{plugin}\": {kind} must not have child nodes");
    ensure!(
        node.entries().iter().all(|entry| entry.name().is_none()),
        "plugin \"{plugin}\": {kind} must not have properties"
    );
    ensure!(
        node.entries().len() == argument_count,
        "plugin \"{plugin}\": {kind} requires exactly {argument_count} string argument{}",
        if argument_count == 1 { "" } else { "s" }
    );
    ensure!(
        node.entries().iter().all(|entry| entry.ty().is_none()),
        "plugin \"{plugin}\": {kind} does not support KDL type annotations"
    );
    let arguments =
        node.entries()
            .iter()
            .map(|entry| {
                entry.value().as_string().map(str::to_owned).with_context(|| {
                    format!("plugin \"{plugin}\": {kind} arguments must be strings")
                })
            })
            .collect::<Result<Vec<_>>>()?;
    let mut arguments = arguments.into_iter();
    let name =
        arguments.next().with_context(|| format!("plugin \"{plugin}\": {kind} requires a name"))?;
    ensure!(
        !name.trim().is_empty(),
        "plugin \"{plugin}\": {kind} name must not be empty or whitespace-only"
    );

    match kind {
        "env" => {
            let value = arguments
                .next()
                .with_context(|| format!("plugin \"{plugin}\": env requires a value"))?;
            Ok(EnvironmentOperation::Set { name, value })
        }
        "unset-env" => Ok(EnvironmentOperation::Unset { name }),
        _ => unreachable!("only recognized environment nodes are parsed here"),
    }
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

fn validate_plugin_properties(node: &kdl::KdlNode, plugin: &str) -> Result<()> {
    const KNOWN_PROPERTIES: &[&str] =
        &["local", "name", "opt-prefix", "branch", "tag", "commit", "build", "enabled", "cond"];

    let mut property_counts: HashMap<&str, usize> = HashMap::new();
    for name in node.entries().iter().filter_map(|entry| entry.name().map(|name| name.value())) {
        if KNOWN_PROPERTIES.contains(&name) {
            let count = property_counts.entry(name).or_default();
            *count += 1;
            ensure!(*count == 1, "plugin \"{plugin}\": {name} may only be specified once");
        } else {
            bail!("plugin \"{plugin}\": unknown property \"{name}\"");
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
    match property_entry(node, key) {
        None => Ok(None),
        Some(entry) => {
            ensure!(
                entry.ty().is_none(),
                "plugin \"{plugin}\": {key} does not support KDL type annotations"
            );
            match entry.value().as_string() {
                Some(s) => Ok(Some(s.to_string())),
                None => bail!("plugin \"{plugin}\": {key} must be a string"),
            }
        }
    }
}

fn get_non_empty_string(node: &kdl::KdlNode, plugin: &str, key: &str) -> Result<Option<String>> {
    let value = get_string(node, plugin, key)?;
    if let Some(value) = value.as_deref() {
        ensure!(
            !value.trim().is_empty(),
            "plugin \"{plugin}\": {key} must not be empty or whitespace-only"
        );
    }
    Ok(value)
}

/// Extract an optional bool property, erroring if the property exists but is not a bool.
fn get_bool(node: &kdl::KdlNode, plugin: &str, key: &str) -> Result<Option<bool>> {
    match property_entry(node, key) {
        None => Ok(None),
        Some(entry) => {
            ensure!(
                entry.ty().is_none(),
                "plugin \"{plugin}\": {key} does not support KDL type annotations"
            );
            match entry.value().as_bool() {
                Some(b) => Ok(Some(b)),
                None => bail!("plugin \"{plugin}\": {key} must be a bool"),
            }
        }
    }
}

fn expand_local_path(raw: &str) -> Result<String> {
    let expanded = shellexpand::full(raw)
        .with_context(|| format!("failed to expand local path: {raw}"))?
        .into_owned();
    Ok(expanded)
}
