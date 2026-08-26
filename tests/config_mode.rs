mod utils;

use tempfile::tempdir;
use tmup::config_mode::{
    ConfigMode, LoadRequest, TmupConfigPolicy, TpmConfigPolicy, load_from_sources,
    load_with_request,
};
use tmup::model::{PluginSource, Tracking};
use tmup::state::Paths;
use utils::write_file;

#[test]
fn config_mode_pure_loads_only_kdl() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let tpm = dir.path().join("tmux.conf");
    write_file(&kdl, r#"plugin "tmux-plugins/tmux-sensible""#);
    write_file(&tpm, "set -g @plugin 'tmux-plugins/tmux-yank'\n");

    let loaded = load_from_sources(ConfigMode::Pure, Some(&kdl), Some(&tpm)).unwrap();

    assert_eq!(loaded.config.plugins.len(), 1);
    assert_eq!(
        loaded.config.plugins[0].remote_id().unwrap(),
        "github.com/tmux-plugins/tmux-sensible"
    );
    assert!(loaded.warnings.is_empty());
}

#[test]
fn config_mode_mixed_merges_tpm_plugins_into_kdl() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let tpm = dir.path().join("tmux.conf");
    write_file(&kdl, r#"plugin "tmux-plugins/tmux-sensible""#);
    write_file(&tpm, "set -g @plugin 'tmux-plugins/tmux-yank'\n");

    let loaded = load_from_sources(ConfigMode::Mixed, Some(&kdl), Some(&tpm)).unwrap();

    assert_eq!(loaded.config.plugins.len(), 2);
    assert_eq!(loaded.config.plugins[0].remote_id().unwrap(), "github.com/tmux-plugins/tmux-yank");
    assert_eq!(
        loaded.config.plugins[1].remote_id().unwrap(),
        "github.com/tmux-plugins/tmux-sensible"
    );
}

#[test]
fn config_mode_mixed_preserves_kdl_options() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let tpm = dir.path().join("tmux.conf");
    write_file(
        &kdl,
        r#"
options {
    auto-install #false
    concurrency 3
}
"#,
    );
    write_file(&tpm, "set -g @plugin 'tmux-plugins/tmux-yank'\n");

    let loaded = load_from_sources(ConfigMode::Mixed, Some(&kdl), Some(&tpm)).unwrap();

    assert!(!loaded.config.options.auto_install);
    assert_eq!(loaded.config.options.concurrency, 3);
}

#[test]
fn config_mode_mixed_prefers_kdl_for_duplicate_remote_plugin() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let tpm = dir.path().join("tmux.conf");
    write_file(&kdl, r#"plugin "tmux-plugins/tmux-sensible" branch="feature""#);
    write_file(&tpm, "set -g @plugin 'tmux-plugins/tmux-sensible'\n");

    let loaded = load_from_sources(ConfigMode::Mixed, Some(&kdl), Some(&tpm)).unwrap();

    assert_eq!(loaded.config.plugins.len(), 1);
    assert!(
        matches!(&loaded.config.plugins[0].tracking, Tracking::Branch(branch) if branch == "feature")
    );
    assert_eq!(loaded.warnings.len(), 1);
    assert!(loaded.warnings[0].contains("github.com/tmux-plugins/tmux-sensible"));
}

#[test]
fn config_mode_mixed_deduplicates_cross_format_remote_plugin_ids() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let tpm = dir.path().join("tmux.conf");
    write_file(
        &kdl,
        r#"plugin "https://github.com/tmux-plugins/tmux-sensible.git" branch="feature""#,
    );
    write_file(&tpm, "set -g @plugin 'tmux-plugins/tmux-sensible'\n");

    let loaded = load_from_sources(ConfigMode::Mixed, Some(&kdl), Some(&tpm)).unwrap();

    assert_eq!(loaded.config.plugins.len(), 1);
    assert!(
        matches!(&loaded.config.plugins[0].tracking, Tracking::Branch(branch) if branch == "feature")
    );
    assert_eq!(loaded.warnings.len(), 1);
    assert!(loaded.warnings[0].contains("github.com/tmux-plugins/tmux-sensible"));
}

#[test]
fn config_mode_mixed_keeps_kdl_local_plugins() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let tpm = dir.path().join("tmux.conf");
    let local = dir.path().join("local-plugin");
    std::fs::create_dir_all(&local).unwrap();
    write_file(&kdl, &format!(r#"plugin "{}" local=#true"#, local.display()));
    write_file(&tpm, "set -g @plugin 'tmux-plugins/tmux-sensible'\n");

    let loaded = load_from_sources(ConfigMode::Mixed, Some(&kdl), Some(&tpm)).unwrap();

    assert_eq!(loaded.config.plugins.len(), 2);
    assert!(matches!(loaded.config.plugins[1].source, PluginSource::Local { .. }));
}

#[test]
fn config_mode_mixed_requires_kdl_source() {
    let dir = tempdir().unwrap();
    let tpm = dir.path().join("tmux.conf");
    write_file(&tpm, "set -g @plugin 'tmux-plugins/tmux-yank'\n");

    let err = load_from_sources(ConfigMode::Mixed, None, Some(&tpm)).unwrap_err();
    assert!(err.to_string().contains("tmup config file not found"));
}

#[test]
fn config_mode_mixed_allows_missing_tpm_config() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    write_file(&kdl, r#"plugin "tmux-plugins/tmux-sensible""#);

    let loaded = load_from_sources(ConfigMode::Mixed, Some(&kdl), None).unwrap();

    assert_eq!(loaded.config.plugins.len(), 1);
    assert!(loaded.warnings.is_empty());
}

#[test]
fn config_mode_mixed_supports_empty_kdl_with_tpm_plugins() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let tpm = dir.path().join("tmux.conf");
    write_file(&kdl, "");
    write_file(&tpm, "set -g @plugin 'tmux-plugins/tmux-yank'\n");

    let loaded = load_from_sources(ConfigMode::Mixed, Some(&kdl), Some(&tpm)).unwrap();

    assert_eq!(loaded.config.plugins.len(), 1);
    assert_eq!(loaded.config.plugins[0].remote_id().unwrap(), "github.com/tmux-plugins/tmux-yank");
}

#[test]
fn config_mode_mixed_supports_empty_sources() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let tpm = dir.path().join("tmux.conf");
    write_file(&kdl, "");
    write_file(&tpm, "");

    let loaded = load_from_sources(ConfigMode::Mixed, Some(&kdl), Some(&tpm)).unwrap();

    assert!(loaded.config.plugins.is_empty());
    assert!(loaded.warnings.is_empty());
}

#[test]
fn config_mode_load_request_uses_resolved_tpm_path() {
    let dir = tempdir().unwrap();
    let data_root = dir.path().join("data");
    let state_root = dir.path().join("state");
    let kdl = dir.path().join("config/tmux/tmup.kdl");
    let tpm = dir.path().join("config/tmux/tmux.conf");
    write_file(&kdl, "");
    write_file(&tpm, "set -g @plugin 'tmux-plugins/tmux-yank'\n");

    let paths = Paths::from_runtime_roots(data_root, state_root, kdl.clone()).unwrap();
    let loaded = load_with_request(
        &paths,
        LoadRequest {
            mode: ConfigMode::Mixed,
            tmup_policy: TmupConfigPolicy::ReadOnly,
            tpm_policy: TpmConfigPolicy::Resolved(Some(tpm.clone())),
        },
    )
    .unwrap();

    assert_eq!(loaded.paths.config_path, kdl);
    assert_eq!(loaded.paths.lockfile_path, dir.path().join("config/tmux/tmup.lock"));
    assert_eq!(loaded.tpm_config_path.as_deref(), Some(tpm.as_path()));
    assert_eq!(loaded.config.plugins.len(), 1);
    assert_eq!(loaded.config.plugins[0].remote_id().unwrap(), "github.com/tmux-plugins/tmux-yank");
}

#[test]
fn enable_conditions_project_remote_and_local_declarations() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let local_enabled = dir.path().join("local-enabled");
    let local_disabled = dir.path().join("local-disabled");
    write_file(
        &kdl,
        &format!(
            r#"
plugin "user/default"
plugin "user/enabled" enabled=#true
plugin "user/disabled" enabled=#false
plugin "{}" local=#true enabled=#true
plugin "{}" local=#true enabled=#false
"#,
            local_enabled.display(),
            local_disabled.display(),
        ),
    );

    let loaded = load_from_sources(ConfigMode::Pure, Some(&kdl), None).unwrap();

    let visible: Vec<_> = loaded.config.plugins.iter().map(|plugin| plugin.name.as_str()).collect();
    assert_eq!(visible, ["default", "enabled", "local-enabled"]);
}

#[test]
fn shell_enable_conditions_use_config_directory_and_nonzero_is_false() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("nested");
    let kdl = config_dir.join("tmup.kdl");
    write_file(&config_dir.join("host-marker"), "local\n");
    write_file(
        &kdl,
        r#"
plugin "user/from-cwd" enabled="test -f host-marker"
plugin "user/status-one" enabled="exit 1"
plugin "user/status-127" enabled="exit 127"
"#,
    );

    let loaded = load_from_sources(ConfigMode::Pure, Some(&kdl), None).unwrap();

    assert_eq!(loaded.config.plugins.len(), 1);
    assert_eq!(loaded.config.plugins[0].remote_id(), Some("github.com/user/from-cwd"));
    assert!(loaded.warnings.is_empty());
}

#[test]
fn mixed_mode_merges_before_evaluating_enable_conditions() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let tpm = dir.path().join("tmux.conf");
    write_file(&kdl, r#"plugin "tmux-plugins/tmux-sensible" enabled=#false"#);
    write_file(
        &tpm,
        concat!(
            "set -g @plugin 'tmux-plugins/tmux-sensible'\n",
            "set -g @plugin 'tmux-plugins/tmux-yank'\n",
        ),
    );

    let loaded = load_from_sources(ConfigMode::Mixed, Some(&kdl), Some(&tpm)).unwrap();

    assert_eq!(loaded.config.plugins.len(), 1);
    assert_eq!(loaded.config.plugins[0].remote_id(), Some("github.com/tmux-plugins/tmux-yank"));
    assert_eq!(loaded.warnings.len(), 1);
}

#[test]
fn unknown_plugin_parameters_warn_without_hiding_recognized_configuration() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    write_file(
        &kdl,
        r#"
plugin "user/repo" "future-argument" future-property="value" {
    future-child "value"
}
"#,
    );

    let loaded = load_from_sources(ConfigMode::Pure, Some(&kdl), None).unwrap();

    assert_eq!(loaded.config.plugins.len(), 1);
    assert_eq!(loaded.warnings.len(), 3, "{:?}", loaded.warnings);
    assert!(loaded.warnings.iter().any(|warning| warning.contains("positional")));
    assert!(loaded.warnings.iter().any(|warning| warning.contains("future-property")));
    assert!(loaded.warnings.iter().any(|warning| warning.contains("future-child")));
}

#[test]
fn invalid_enable_condition_forms_are_rejected() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let cases = [
        (r#"plugin "user/repo" enabled=42"#, "bool or shell predicate string"),
        (r#"plugin "user/repo" enabled="""#, "must not be empty"),
        (r#"plugin "user/repo" enabled="   ""#, "whitespace-only"),
        (r#"plugin "user/repo" enabled=(future)#true"#, "type annotations"),
        ("plugin \"user/repo\" { enabled { future #true } }", "enabled child form is reserved"),
    ];

    for (input, expected) in cases {
        write_file(&kdl, input);
        let error = load_from_sources(ConfigMode::Pure, Some(&kdl), None).unwrap_err();
        assert!(error.to_string().contains(expected), "input={input:?}, error={error:#}");
    }
}

#[test]
fn repeated_known_plugin_values_are_rejected() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let cases = [
        r#"plugin "user/repo" enabled=#true enabled=#false"#,
        r#"plugin "user/repo" name="one" name="two""#,
        r#"plugin "user/repo" { build "one"; build "two"; }"#,
    ];

    for input in cases {
        write_file(&kdl, input);
        let error = load_from_sources(ConfigMode::Pure, Some(&kdl), None).unwrap_err();
        assert!(error.to_string().contains("only be specified once"), "{error:#}");
    }
}

#[test]
fn every_declaration_is_validated_before_enable_conditions_run() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let marker = dir.path().join("predicate-ran");
    write_file(
        &kdl,
        r#"
plugin "user/first" enabled="touch predicate-ran"
plugin "user/invalid" branch=42 enabled=#false
"#,
    );

    let error = load_from_sources(ConfigMode::Pure, Some(&kdl), None).unwrap_err();

    assert!(error.to_string().contains("branch must be a string"), "{error:#}");
    assert!(!marker.exists(), "no predicate should run until every declaration is valid");
}

#[test]
fn disabled_declarations_still_participate_in_identity_validation() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    write_file(
        &kdl,
        r#"
plugin "user/repo" enabled=#false
plugin "https://github.com/user/repo.git" enabled=#true
"#,
    );

    let error = load_from_sources(ConfigMode::Pure, Some(&kdl), None).unwrap_err();

    assert!(error.to_string().contains("duplicate remote plugin id"), "{error:#}");
}
