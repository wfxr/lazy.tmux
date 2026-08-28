mod utils;

use tempfile::tempdir;
use tmup::config_mode::{
    ConfigMode, ResolutionIntent, load_from_sources, load_from_sources_with_intent,
};
use tmup::lockfile::{config_fingerprint, remote_plugin_config_hash};
use utils::{load_native_config as parse_config, write_file};

#[test]
fn default_branch_hash_uses_declared_selector_semantics() {
    let default_cfg = parse_config(r#"plugin "user/repo""#).unwrap();
    let branch_cfg = parse_config(r#"plugin "user/repo" branch="main""#).unwrap();

    let default_hash = remote_plugin_config_hash(&default_cfg.plugins[0]).unwrap();
    let default_hash_again = remote_plugin_config_hash(&default_cfg.plugins[0]).unwrap();
    let branch_hash = remote_plugin_config_hash(&branch_cfg.plugins[0]).unwrap();

    assert_eq!(default_hash, default_hash_again);
    assert_ne!(default_hash, branch_hash);
}

#[test]
fn config_fingerprint_ignores_non_lock_affecting_changes() {
    let cfg_a = parse_config(
        r#"
plugin "user/beta" name="beta" opt-prefix="beta_" {
    opt "flavor" "mocha"
}
plugin "https://github.com/user/alpha.git" name="alpha"
plugin "/tmp/local-a" local=#true name="local-a"
"#,
    )
    .unwrap();

    let cfg_b = parse_config(
        r#"
plugin "/tmp/local-b" local=#true name="local-b"
plugin "git@github.com:user/alpha.git" name="renamed-alpha" opt-prefix="ignored_" {
    opt "theme" "light"
}
plugin "https://github.com/user/beta.git"
"#,
    )
    .unwrap();

    assert_eq!(config_fingerprint(&cfg_a), config_fingerprint(&cfg_b));
}

#[test]
fn runtime_declarations_do_not_affect_plugin_or_config_fingerprints() {
    let without_runtime_declarations = parse_config(r#"plugin "user/repo""#).unwrap();
    let first = parse_config(
        r##"
plugin "user/repo" {
    env "MODE" "one"
    unset-env "LEGACY"
    bind "C-w" {
        options "-n" "-r"
        shell "./first" background=#true
    }
}
"##,
    )
    .unwrap();
    let second = parse_config(
        r#"
plugin "user/repo" {
    unset-env "MODE"
    env "MODE" "two"
    bind "M-x" {
        options "-T" "root"
        shell "printf '%s\n' \"$MODE\""
    }
}
"#,
    )
    .unwrap();

    assert_eq!(
        remote_plugin_config_hash(&without_runtime_declarations.plugins[0]),
        remote_plugin_config_hash(&first.plugins[0])
    );
    assert_eq!(
        remote_plugin_config_hash(&first.plugins[0]),
        remote_plugin_config_hash(&second.plugins[0])
    );
    assert_eq!(config_fingerprint(&without_runtime_declarations), config_fingerprint(&first));
    assert_eq!(config_fingerprint(&first), config_fingerprint(&second));
}

#[test]
fn config_fingerprint_changes_when_build_changes() {
    let cfg_a = parse_config(r#"plugin "user/repo" build="make install""#).unwrap();
    let cfg_b = parse_config(r#"plugin "user/repo" build="just build""#).unwrap();

    assert_ne!(config_fingerprint(&cfg_a), config_fingerprint(&cfg_b));
}

#[test]
fn sync_fingerprint_config_mode_uses_merged_kdl_precedence() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let tmux_conf = dir.path().join("tmux.conf");
    write_file(&kdl, r#"plugin "tmux-plugins/tmux-sensible" branch="feature""#);
    write_file(&tmux_conf, "set -g @plugin 'tmux-plugins/tmux-sensible'\n");

    let loaded = load_from_sources(ConfigMode::Mixed, Some(&kdl), Some(&tmux_conf)).unwrap();
    let expected = parse_config(r#"plugin "tmux-plugins/tmux-sensible" branch="feature""#).unwrap();

    assert_eq!(config_fingerprint(&loaded.config), config_fingerprint(&expected));
}

#[test]
fn enable_condition_expression_text_does_not_affect_fingerprints() {
    let dir = tempdir().unwrap();
    let first = dir.path().join("first/tmup.kdl");
    let second = dir.path().join("second/tmup.kdl");
    write_file(&first, r#"plugin "user/repo" enabled="exit 0""#);
    write_file(&second, r#"plugin "user/repo" enabled="test 1 = 1""#);

    let first = load_from_sources(ConfigMode::Pure, Some(&first), None).unwrap();
    let second = load_from_sources(ConfigMode::Pure, Some(&second), None).unwrap();

    assert_eq!(
        remote_plugin_config_hash(&first.config.plugins[0]),
        remote_plugin_config_hash(&second.config.plugins[0]),
    );
    assert_eq!(config_fingerprint(&first.config), config_fingerprint(&second.config));
}

#[test]
fn false_enable_condition_changes_effective_spec_and_fingerprint_membership() {
    let dir = tempdir().unwrap();
    let enabled = dir.path().join("enabled/tmup.kdl");
    let disabled = dir.path().join("disabled/tmup.kdl");
    write_file(&enabled, r#"plugin "user/repo" enabled=#true"#);
    write_file(&disabled, r#"plugin "user/repo" enabled=#false"#);

    let enabled = load_from_sources(ConfigMode::Pure, Some(&enabled), None).unwrap();
    let disabled = load_from_sources(ConfigMode::Pure, Some(&disabled), None).unwrap();

    assert_eq!(enabled.config.plugins.len(), 1);
    assert!(disabled.config.plugins.is_empty());
    assert_ne!(config_fingerprint(&enabled.config), config_fingerprint(&disabled.config));
}

#[test]
fn load_conditions_do_not_affect_plugin_or_config_fingerprints() {
    let dir = tempdir().unwrap();
    let first = dir.path().join("first/tmup.kdl");
    let second = dir.path().join("second/tmup.kdl");
    write_file(&first, r#"plugin "user/repo" cond=#false"#);
    write_file(&second, r#"plugin "user/repo" cond="kill -TERM $$""#);

    let first = load_from_sources(ConfigMode::Pure, Some(&first), None).unwrap();
    let second = load_from_sources(ConfigMode::Pure, Some(&second), None).unwrap();

    assert_eq!(
        remote_plugin_config_hash(&first.config.plugins[0]),
        remote_plugin_config_hash(&second.config.plugins[0]),
    );
    assert_eq!(config_fingerprint(&first.config), config_fingerprint(&second.config));
}

#[test]
fn runtime_configuration_predicates_and_selected_contents_do_not_affect_fingerprints() {
    let dir = tempdir().unwrap();
    let first = dir.path().join("first/tmup.kdl");
    let second = dir.path().join("second/tmup.kdl");
    write_file(
        &first,
        r#"
plugin "user/repo" {
    if "exit 0" {
        bind "first" { shell "./first" }
    }
}
"#,
    );
    write_file(
        &second,
        r#"
plugin "user/repo" {
    if "test 1 = 1" {
        bind "second" { shell "./second" }
    }
}
"#,
    );

    let first = load_from_sources_with_intent(
        ConfigMode::Pure,
        Some(&first),
        None,
        ResolutionIntent::RuntimeConfiguration,
    )
    .unwrap();
    let second = load_from_sources_with_intent(
        ConfigMode::Pure,
        Some(&second),
        None,
        ResolutionIntent::RuntimeConfiguration,
    )
    .unwrap();

    let first_runtime = first.config.runtime_configuration().unwrap().plugins().next().unwrap().1;
    let second_runtime = second.config.runtime_configuration().unwrap().plugins().next().unwrap().1;
    assert_eq!(first_runtime.bindings[0].key, "first");
    assert_eq!(second_runtime.bindings[0].key, "second");
    assert_eq!(
        remote_plugin_config_hash(&first.config.plugins[0]),
        remote_plugin_config_hash(&second.config.plugins[0]),
    );
    assert_eq!(config_fingerprint(&first.config), config_fingerprint(&second.config));
}
