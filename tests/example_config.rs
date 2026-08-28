mod utils;

use tmup::model::SetupOperation;
use utils::load_native_runtime_config;

#[test]
fn example_config_parses() {
    let input = std::fs::read_to_string("examples/tmup.kdl").unwrap();
    let cfg = load_native_runtime_config(&input).unwrap();
    assert!(cfg.options.auto_install);
    assert_eq!(cfg.plugins.len(), 6); // continuum disabled via slashdash
    // tmux-sensible
    assert_eq!(cfg.plugins[0].remote_id().unwrap(), "github.com/tmux-plugins/tmux-sensible");
    // tmux-yank pinned to tag
    assert!(matches!(cfg.plugins[1].tracking, tmup::model::Tracking::Tag(_)));
    // tmux-resurrect with branch + build + opts
    assert!(matches!(cfg.plugins[2].tracking, tmup::model::Tracking::Branch(_)));
    assert_eq!(cfg.plugins[2].build.as_deref(), Some("make install"));
    // catppuccin with opt-prefix
    assert_eq!(cfg.plugins[3].opt_prefix, "catppuccin_");
    let runtimes: Vec<_> = cfg.runtime_configuration().unwrap().plugins().collect();
    assert_eq!(runtimes[2].1.setup.len(), 2);
    assert_eq!(
        runtimes[3].1.setup[0],
        SetupOperation::Option { key: "flavor".into(), value: "mocha".into() }
    );
    // gitlab plugin
    assert_eq!(cfg.plugins[4].remote_id().unwrap(), "gitlab.com/user/my-plugin");
    // local plugin
    assert!(cfg.plugins[5].is_local());
    assert_eq!(cfg.plugins[5].name, "my-plugin-dev");
}
