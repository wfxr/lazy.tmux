mod utils;

use tmup::model::{EnvironmentOperation, KeyBinding, SetupOperation};
use utils::{
    MALFORMED_BINDING_NODES, MALFORMED_ENVIRONMENT_NODES, load_native_config as parse_config,
    load_native_runtime_config,
};

#[test]
fn parses_plug_declaration() {
    let cfg = parse_config(r#"plug "tmux-plugins/tmux-sensible""#).unwrap();

    assert_eq!(cfg.plugins.len(), 1);
    assert_eq!(cfg.plugins[0].remote_id().unwrap(), "github.com/tmux-plugins/tmux-sensible");
}

#[test]
fn parses_remote_and_local_plugins() {
    let home = std::env::var("HOME").unwrap();
    let input = r#"
options {
    auto-install #true
}
plug "tmux-plugins/tmux-sensible"
plug "~/dev/my-plugin" local=#true name="my-plugin-dev"
    "#;

    let cfg = parse_config(input).unwrap();
    assert_eq!(cfg.plugins.len(), 2);
    assert!(cfg.plugins[0].is_remote());
    assert!(cfg.plugins[1].is_local());
    assert_eq!(cfg.plugins[1].name, "my-plugin-dev");
    match &cfg.plugins[1].source {
        tmup::model::PluginSource::Local { path } => {
            assert_eq!(path, &format!("{home}/dev/my-plugin"));
        }
        other => panic!("expected local plugin, got {other:?}"),
    }
}

#[test]
fn parses_options() {
    let input = r#"
options {
    auto-install #false
}
    "#;
    let cfg = parse_config(input).unwrap();
    assert!(!cfg.options.auto_install);
}

#[test]
fn parses_opts_and_opt_prefix() {
    let input = r##"
plug "catppuccin/tmux" opt-prefix="catppuccin_" {
    opt "flavor" "mocha"
    opt "window_text" "#W"
}
    "##;
    let cfg = load_native_runtime_config(input).unwrap();
    let (plugin, runtime) = cfg.runtime_configuration().unwrap().plugins().next().unwrap();
    assert_eq!(plugin.opt_prefix, "catppuccin_");
    assert_eq!(
        runtime.setup,
        vec![
            SetupOperation::Option { key: "flavor".into(), value: "mocha".into() },
            SetupOperation::Option { key: "window_text".into(), value: "#W".into() },
        ]
    );
}

#[test]
fn parses_ordered_environment_operations_for_remote_and_local_plugins() {
    let cfg = load_native_runtime_config(
        r#"
plug "user/remote" {
    env "MODE" "remote"
    unset-env "LEGACY_MODE"
    env "MODE" ""
}
plug "/opt/plugins/local" local=#true {
    unset-env "MODE"
}
"#,
    )
    .unwrap();

    let mut runtimes = cfg.runtime_configuration().unwrap().plugins();
    let (_, remote_runtime) = runtimes.next().unwrap();
    let (_, local_runtime) = runtimes.next().unwrap();
    assert_eq!(
        remote_runtime.setup,
        vec![
            SetupOperation::Environment(EnvironmentOperation::Set {
                name: "MODE".into(),
                value: "remote".into(),
            }),
            SetupOperation::Environment(EnvironmentOperation::Unset { name: "LEGACY_MODE".into() }),
            SetupOperation::Environment(EnvironmentOperation::Set {
                name: "MODE".into(),
                value: "".into(),
            }),
        ]
    );
    assert_eq!(
        local_runtime.setup,
        vec![SetupOperation::Environment(EnvironmentOperation::Unset { name: "MODE".into() })]
    );
}

#[test]
fn parses_ordered_shell_bindings_for_remote_and_local_plugins() {
    let cfg = load_native_runtime_config(
        r##"
plug "user/remote" {
    bind "C-w" {
        options "-n" "-r"
        shell "scripts/session.sh attach | tee /tmp/session.log" background=#true
    }
    bind "x" {
        shell "printf '%s\n' \"$CURRENT_MODE\""
    }
}

plug "/opt/plugins/local" local=#true {
    bind "M-l" {
        shell "./launch"
    }
}
"##,
    )
    .unwrap();

    let mut runtimes = cfg.runtime_configuration().unwrap().plugins();
    let (_, remote_runtime) = runtimes.next().unwrap();
    let (_, local_runtime) = runtimes.next().unwrap();
    assert_eq!(
        remote_runtime.bindings,
        vec![
            KeyBinding {
                key: "C-w".into(),
                options: vec!["-n".into(), "-r".into()],
                shell: "scripts/session.sh attach | tee /tmp/session.log".into(),
                background: true,
            },
            KeyBinding {
                key: "x".into(),
                options: vec![],
                shell: "printf '%s\n' \"$CURRENT_MODE\"".into(),
                background: false,
            },
        ]
    );
    assert_eq!(
        local_runtime.bindings,
        vec![KeyBinding {
            key: "M-l".into(),
            options: vec![],
            shell: "./launch".into(),
            background: false,
        }]
    );
}

#[test]
fn rejects_malformed_recognized_binding_nodes() {
    for &(input, expected) in MALFORMED_BINDING_NODES {
        let error = parse_config(input).unwrap_err();
        assert!(error.to_string().contains(expected), "input={input:?}, error={error:#}");
    }
}

#[test]
fn rejects_malformed_recognized_environment_nodes() {
    for &(input, expected) in MALFORMED_ENVIRONMENT_NODES {
        let error = parse_config(input).unwrap_err();
        assert!(error.to_string().contains(expected), "input={input:?}, error={error:#}");
    }
}

#[test]
fn parses_tracking_selectors() {
    let branch = parse_config(r#"plug "user/repo" branch="main""#).unwrap();
    let tag = parse_config(r#"plug "user/repo" tag="v2.3""#).unwrap();
    let commit = parse_config(r#"plug "user/repo" commit="abc123""#).unwrap();
    let default = parse_config(r#"plug "user/repo""#).unwrap();

    assert!(matches!(branch.plugins[0].tracking, tmup::model::Tracking::Branch(_)));
    assert!(matches!(tag.plugins[0].tracking, tmup::model::Tracking::Tag(_)));
    assert!(matches!(commit.plugins[0].tracking, tmup::model::Tracking::Commit(_)));
    assert!(matches!(default.plugins[0].tracking, tmup::model::Tracking::DefaultBranch));
}

#[test]
fn rejects_multiple_tracking_selectors() {
    let input = r#"plug "tmux-plugins/tmux-yank" branch="main" tag="v1.0.0""#;
    assert!(parse_config(input).is_err());
}

#[test]
fn rejects_local_plugin_with_tracking_selector() {
    let input = r#"plug "~/dev/my-plugin" local=#true branch="main""#;
    assert!(parse_config(input).is_err());
}

#[test]
fn parses_build_property() {
    let input = r#"plug "tmux-plugins/tmux-resurrect" build="make install""#;
    let cfg = parse_config(input).unwrap();
    assert_eq!(cfg.plugins[0].build.as_deref(), Some("make install"));
}

#[test]
fn defaults_are_applied() {
    let cfg = parse_config("").unwrap();
    assert!(cfg.options.auto_install);
    assert!(cfg.plugins.is_empty());
}

#[test]
fn rejects_wrong_type_branch() {
    let err = parse_config(r#"plug "user/repo" branch=123"#).unwrap_err();
    assert!(err.to_string().contains("branch must be a string"), "{err}");
}

#[test]
fn rejects_wrong_type_local() {
    let err = parse_config(r#"plug "user/repo" local="yes""#).unwrap_err();
    assert!(err.to_string().contains("local must be a bool"), "{err}");
}

#[test]
fn rejects_wrong_type_build() {
    let err = parse_config(r#"plug "user/repo" build=42"#).unwrap_err();
    assert!(err.to_string().contains("build must be a string"), "{err}");
}

#[test]
fn rejects_build_child_when_build_property_exists() {
    let input = r#"
plug "tmux-plugins/tmux-resurrect" build="make install" {
    build "cargo build --release"
}
    "#;
    let err = parse_config(input).unwrap_err();
    assert!(err.to_string().contains("unknown child \"build\""), "{err}");
}

#[test]
fn rejects_build_as_child_node() {
    let input = r#"
plug "tmux-plugins/tmux-resurrect" {
    build "make install"
    }
    "#;
    let err = parse_config(input).unwrap_err();
    assert!(err.to_string().contains("unknown child \"build\""), "{err}");
}

#[test]
fn rejects_local_with_remote_style_path() {
    let err = parse_config(r#"plug "user/repo" local=#true"#).unwrap_err();
    assert!(err.to_string().contains("must expand to an absolute path"), "{err}");
}

#[test]
fn accepts_local_with_valid_paths() {
    parse_config(r#"plug "~/dev/my-plugin" local=#true"#).unwrap();
    parse_config(r#"plug "/opt/plugins/foo" local=#true"#).unwrap();
}

#[test]
fn expands_env_var_local_paths() {
    let home = std::env::var("HOME").unwrap();
    let cfg = parse_config(r#"plug "$HOME/dev/my-plugin" local=#true"#).unwrap();
    match &cfg.plugins[0].source {
        tmup::model::PluginSource::Local { path } => {
            assert_eq!(path, &format!("{home}/dev/my-plugin"));
        }
        other => panic!("expected local plugin, got {other:?}"),
    }
}

#[test]
fn rejects_relative_local_paths_after_expansion() {
    let err = parse_config(r#"plug "./local-plugin" local=#true"#).unwrap_err();
    assert!(err.to_string().contains("must expand to an absolute path"), "{err}");
}

#[test]
fn parses_concurrency_option() {
    let cfg = parse_config("options { concurrency 8 }").unwrap();
    assert_eq!(cfg.options.concurrency, 8);
}

#[test]
fn accepts_concurrency_one() {
    let cfg = parse_config("options { concurrency 1 }").unwrap();
    assert_eq!(cfg.options.concurrency, 1);
}

#[test]
fn defaults_concurrency_to_sixteen() {
    let cfg = parse_config(r#"plug "user/repo""#).unwrap();
    assert_eq!(cfg.options.concurrency, 16);
}

#[test]
fn rejects_zero_concurrency() {
    let err = parse_config("options { concurrency 0 }").unwrap_err();
    assert!(err.to_string().contains("concurrency must be at least 1"));
}

#[test]
fn rejects_too_large_concurrency() {
    let too_large = (usize::MAX as u128) + 1;
    let input = format!("options {{ concurrency {too_large} }}");
    let err = parse_config(&input).unwrap_err();
    assert!(err.to_string().contains("concurrency is too large for this platform"), "{err}");
}

#[test]
fn rejects_non_integer_concurrency_string() {
    let err = parse_config(r#"options { concurrency "abc" }"#).unwrap_err();
    assert!(err.to_string().contains("concurrency must be an integer"), "{err}");
}

#[test]
fn rejects_non_integer_concurrency_float() {
    let err = parse_config("options { concurrency 3.14 }").unwrap_err();
    assert!(err.to_string().contains("concurrency must be an integer"), "{err}");
}
