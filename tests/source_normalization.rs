mod utils;

use utils::load_native_config as parse_config;

#[test]
fn normalizes_github_shorthand_to_full_id() {
    let cfg = parse_config(r#"plugin "tmux-plugins/tmux-sensible""#).unwrap();
    assert_eq!(cfg.plugins[0].remote_id().unwrap(), "github.com/tmux-plugins/tmux-sensible");
}

#[test]
fn normalizes_https_url_to_full_id() {
    let cfg = parse_config(r#"plugin "https://github.com/user/repo.git""#).unwrap();
    assert_eq!(cfg.plugins[0].remote_id().unwrap(), "github.com/user/repo");
}

#[test]
fn normalizes_https_url_without_git_suffix() {
    let cfg = parse_config(r#"plugin "https://github.com/user/repo""#).unwrap();
    assert_eq!(cfg.plugins[0].remote_id().unwrap(), "github.com/user/repo");
}

#[test]
fn normalizes_ssh_git_url_to_full_id() {
    let cfg = parse_config(r#"plugin "git@github.com:user/repo.git""#).unwrap();
    assert_eq!(cfg.plugins[0].remote_id().unwrap(), "github.com/user/repo");
}

#[test]
fn normalizes_gitlab_https_url() {
    let cfg = parse_config(r#"plugin "https://gitlab.com/user/plugin.git""#).unwrap();
    assert_eq!(cfg.plugins[0].remote_id().unwrap(), "gitlab.com/user/plugin");
}

#[test]
fn normalizes_custom_host_url() {
    let cfg = parse_config(r#"plugin "https://git.example.com/team/plugin.git""#).unwrap();
    assert_eq!(cfg.plugins[0].remote_id().unwrap(), "git.example.com/team/plugin");
}

#[test]
fn rejects_remote_ids_with_parent_segments() {
    let err = parse_config(r#"plugin "https://git.example.com/team/../plugin.git""#).unwrap_err();
    assert!(err.to_string().contains("unsafe plugin id segment"), "{err}");
}

#[test]
fn rejects_remote_ids_with_empty_segments() {
    let err = parse_config(r#"plugin "git@github.com:team//plugin.git""#).unwrap_err();
    assert!(err.to_string().contains("unsafe plugin id segment"), "{err}");
}

#[test]
fn name_defaults_to_id_basename() {
    let cfg = parse_config(r#"plugin "tmux-plugins/tmux-sensible""#).unwrap();
    assert_eq!(cfg.plugins[0].name, "tmux-sensible");
}

#[test]
fn rejects_duplicate_remote_ids() {
    let input = r#"
plugin "tmux-plugins/tmux-sensible"
plugin "https://github.com/tmux-plugins/tmux-sensible.git"
    "#;
    assert!(parse_config(input).is_err());
}

#[test]
fn local_plugin_has_no_remote_id() {
    let cfg = parse_config(r#"plugin "~/dev/my-plugin" local=#true"#).unwrap();
    assert!(cfg.plugins[0].remote_id().is_none());
}
