mod utils;

use tempfile::tempdir;
use tmup::config_mode::{
    ConfigMode, LoadRequest, ResolutionIntent, TmupConfigPolicy, TpmConfigPolicy,
    load_from_sources, load_from_sources_with_intent, load_with_request,
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
            intent: ResolutionIntent::ManagedState,
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
fn load_conditions_resolve_for_enabled_remote_and_local_plugins() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    let kdl = config_dir.join("tmup.kdl");
    let local = dir.path().join("local-plugin");
    write_file(&config_dir.join("load-marker"), "ready\n");
    write_file(
        &kdl,
        &format!(
            r#"
plugin "user/default"
plugin "user/false" cond=#false
plugin "user/shell-true" cond="test -f load-marker"
plugin "user/shell-false" cond="exit 37"
plugin "{}" local=#true cond=#false
"#,
            local.display(),
        ),
    );

    let loaded = load_from_sources_with_intent(
        ConfigMode::Pure,
        Some(&kdl),
        None,
        ResolutionIntent::LoadEligibility,
    )
    .unwrap();

    assert_eq!(loaded.config.plugins.len(), 5);
    assert_eq!(
        loaded.config.load_eligibility().map(|eligibility| eligibility.values()),
        Some(&[true, false, true, false, false][..])
    );
}

#[test]
fn runtime_configuration_selects_else_bindings_and_keeps_unconditional_bindings() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    write_file(
        &kdl,
        r#"
plugin "user/repo" {
    bind "always" { shell "./always" }
    if #false {
        bind "then" { shell "./then" }
    }
    else {
        bind "otherwise" { shell "./otherwise" }
    }
}
"#,
    );

    let loaded = load_from_sources_with_intent(
        ConfigMode::Pure,
        Some(&kdl),
        None,
        ResolutionIntent::RuntimeConfiguration,
    )
    .unwrap();

    let keys: Vec<_> =
        loaded.config.plugins[0].bindings.iter().map(|binding| binding.key.as_str()).collect();
    assert_eq!(keys, ["always", "otherwise"]);
}

#[test]
fn runtime_configuration_shell_predicates_use_config_directory_and_short_circuit_nested_else() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config");
    let kdl = config_dir.join("tmup.kdl");
    write_file(&config_dir.join("host-marker"), "ready\n");
    write_file(
        &kdl,
        r#"
plugin "user/repo" {
    if "test -f host-marker" {
        bind "host" { shell "./host" }
    }
    else {
        if "kill -TERM $$" {
            bind "unreachable" { shell "./unreachable" }
        }
    }
}
"#,
    );

    let loaded = load_from_sources_with_intent(
        ConfigMode::Pure,
        Some(&kdl),
        None,
        ResolutionIntent::RuntimeConfiguration,
    )
    .unwrap();

    let keys: Vec<_> =
        loaded.config.plugins[0].bindings.iter().map(|binding| binding.key.as_str()).collect();
    assert_eq!(keys, ["host"]);
}

#[test]
fn invalid_runtime_configuration_branch_forms_are_rejected_before_predicates_run() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let cases = [
        ("if {}", "if requires exactly one condition"),
        ("if #true #false {}", "if requires exactly one condition"),
        ("if 42 {}", "if condition must be a bool or shell predicate string"),
        (r#"if "" {}"#, "if shell predicate must not be empty"),
        ("if #true future=#true {}", "if must not have properties"),
        ("if (future)#true {}", "if does not support KDL type annotations"),
        ("if #true", "if requires a child block"),
        ("if #true {}\nelse \"entry\" {}", "else must not have arguments or properties"),
        ("if #true {}\nelse", "else requires a child block"),
        ("else {}", "else must immediately follow an if node"),
        (
            "if #true {}\nbind \"x\" { shell \"true\" }\nelse {}",
            "else must immediately follow an if node",
        ),
        ("if #true {}\nelse {}\nelse {}", "else must immediately follow an if node"),
        ("/-if #true {}\nelse {}", "else must immediately follow an if node"),
        ("if #true {}\nelse { build \"make\" }", "runtime configuration branch only allows"),
        ("if #true {}\nelse { enabled #false }", "runtime configuration branch only allows"),
        ("if #true {}\nelse { cond #false }", "runtime configuration branch only allows"),
        ("if #true {}\nelse { plugin \"other/repo\" }", "runtime configuration branch only allows"),
        ("if #true {}\nelse { future \"value\" }", "runtime configuration branch only allows"),
        (
            "if #true {}\nelse { opt \"key\" \"value\" \"extra\" }",
            "opt requires exactly 2 string arguments",
        ),
    ];

    for (index, (branch, expected)) in cases.into_iter().enumerate() {
        let marker = dir.path().join(format!("predicate-ran-{index}"));
        write_file(
            &kdl,
            &format!(
                "plugin \"user/first\" enabled=\"touch {}\"\nplugin \"user/repo\" {{\n{branch}\n}}",
                marker.display()
            ),
        );

        let error = load_from_sources_with_intent(
            ConfigMode::Pure,
            Some(&kdl),
            None,
            ResolutionIntent::RuntimeConfiguration,
        )
        .unwrap_err();

        assert!(error.to_string().contains(expected), "branch={branch:?}, error={error:#}");
        assert!(!marker.exists(), "branch={branch:?}: predicates must run after validation");
    }
}

#[test]
fn runtime_configuration_allows_empty_branches() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    write_file(
        &kdl,
        r#"
plugin "user/repo" {
    if #true {
    }
    else {
    }
}
"#,
    );

    let loaded = load_from_sources_with_intent(
        ConfigMode::Pure,
        Some(&kdl),
        None,
        ResolutionIntent::RuntimeConfiguration,
    )
    .unwrap();

    assert!(loaded.config.plugins[0].environment.is_empty());
    assert!(loaded.config.plugins[0].opts.is_empty());
    assert!(loaded.config.plugins[0].bindings.is_empty());
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
fn mixed_mode_preserves_tpm_order_and_applies_kdl_load_conditions_after_merge() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let tpm = dir.path().join("tmux.conf");
    write_file(&kdl, r#"plugin "tmux-plugins/tmux-sensible" cond=#false"#);
    write_file(
        &tpm,
        concat!(
            "set -g @plugin 'tmux-plugins/tmux-sensible'\n",
            "set -g @plugin 'tmux-plugins/tmux-yank'\n",
        ),
    );

    let loaded = load_from_sources_with_intent(
        ConfigMode::Mixed,
        Some(&kdl),
        Some(&tpm),
        ResolutionIntent::LoadEligibility,
    )
    .unwrap();

    let ids: Vec<_> =
        loaded.config.plugins.iter().filter_map(|plugin| plugin.remote_id()).collect();
    assert_eq!(ids, ["github.com/tmux-plugins/tmux-sensible", "github.com/tmux-plugins/tmux-yank"]);
    assert_eq!(
        loaded.config.load_eligibility().map(|eligibility| eligibility.values()),
        Some(&[false, true][..])
    );
}

#[test]
fn mixed_mode_projects_kdl_runtime_configuration_in_the_tpm_slot() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let tpm = dir.path().join("tmux.conf");
    write_file(
        &kdl,
        r#"
plugin "user/repo" branch="feature" {
    if #false {
        bind "then" { shell "./then" }
    }
    else {
        bind "otherwise" { shell "./otherwise" }
    }
}
"#,
    );
    write_file(
        &tpm,
        concat!(
            "set -g @plugin 'user/first'\n",
            "set -g @plugin 'user/repo'\n",
            "set -g @plugin 'user/last'\n",
        ),
    );

    let loaded = load_from_sources_with_intent(
        ConfigMode::Mixed,
        Some(&kdl),
        Some(&tpm),
        ResolutionIntent::RuntimeConfiguration,
    )
    .unwrap();

    let ids: Vec<_> =
        loaded.config.plugins.iter().filter_map(|plugin| plugin.remote_id()).collect();
    assert_eq!(ids, ["github.com/user/first", "github.com/user/repo", "github.com/user/last"]);
    assert!(matches!(
        &loaded.config.plugins[1].tracking,
        Tracking::Branch(branch) if branch == "feature"
    ));
    let keys: Vec<_> =
        loaded.config.plugins[1].bindings.iter().map(|binding| binding.key.as_str()).collect();
    assert_eq!(keys, ["otherwise"]);
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
fn invalid_load_condition_forms_are_rejected_before_predicates_run() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let marker = dir.path().join("predicate-ran");
    let cases = [
        (r#"plugin "user/repo" cond=42"#, "bool or shell predicate string"),
        (r#"plugin "user/repo" cond="""#, "must not be empty"),
        (r#"plugin "user/repo" cond="   ""#, "whitespace-only"),
        (r#"plugin "user/repo" cond=(future)#true"#, "type annotations"),
        ("plugin \"user/repo\" { cond { future #true } }", "cond child form is reserved"),
    ];

    for (input, expected) in cases {
        write_file(
            &kdl,
            &format!("plugin \"user/first\" enabled=\"touch {}\"\n{input}", marker.display()),
        );
        let error = load_from_sources(ConfigMode::Pure, Some(&kdl), None).unwrap_err();
        assert!(error.to_string().contains(expected), "input={input:?}, error={error:#}");
        assert!(!marker.exists(), "structural errors must precede predicate execution");
    }
}

#[test]
fn managed_state_resolution_does_not_evaluate_load_conditions() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    write_file(&kdl, r#"plugin "user/repo" cond="kill -TERM $$""#);

    let loaded = load_from_sources(ConfigMode::Pure, Some(&kdl), None).unwrap();

    assert_eq!(loaded.config.plugins.len(), 1);
    assert!(loaded.config.load_eligibility().is_none());
}

#[test]
fn repeated_known_plugin_values_are_rejected() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let cases = [
        r#"plugin "user/repo" enabled=#true enabled=#false"#,
        r#"plugin "user/repo" cond=#true cond=#false"#,
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

#[cfg(unix)]
#[test]
fn timed_out_predicate_cannot_leave_descendants_running() {
    let dir = tempdir().unwrap();
    let kdl = dir.path().join("tmup.kdl");
    let marker = dir.path().join("descendant-survived");
    write_file(&kdl, r#"plugin "user/repo" enabled="(sleep 6; touch descendant-survived) & wait""#);

    let error = load_from_sources(ConfigMode::Pure, Some(&kdl), None).unwrap_err();
    assert!(error.to_string().contains("timed out after 5 seconds"), "{error:#}");
    std::thread::sleep(std::time::Duration::from_secs(2));

    assert!(!marker.exists(), "a timed-out predicate must terminate its whole process group");
}
