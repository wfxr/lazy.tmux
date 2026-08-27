mod utils;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use assert_cmd::Command;
use tempfile::tempdir;
use tmup::lockfile::{
    LockEntry, LockFile, config_fingerprint, read_lockfile, remote_plugin_config_hash,
};
use tmup::model::{Config, Options, PluginSource, PluginSpec, Tracking};
use tmup::progress::NullReporter;
use tmup::state::{Paths, build_command_hash};
use tmup::sync;
use utils::*;

#[cfg(unix)]
fn write_inline_tmux(root: &Path) -> PathBuf {
    let bin_dir = root.join("bin-inline-tmux");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script = bin_dir.join("tmux");
    std::fs::write(
        &script,
        r#"#!/bin/sh
case "$1" in
  -V) printf 'tmux 1.9\n';;
esac
exit 0
"#,
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).unwrap();
    bin_dir
}

#[cfg(unix)]
fn write_recording_tmux(root: &Path, log: &Path) -> PathBuf {
    let bin_dir = root.join("bin-recording-tmux");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script = bin_dir.join("tmux");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
case "$1" in
  set-environment|set|run-shell)
    printf 'command\n' >> '{log}'
    for arg do
      printf 'arg=<%s>\n' "$arg" >> '{log}'
    done
    printf 'end\n' >> '{log}'
    ;;
  -V) printf 'tmux 1.9\n' ;;
esac
exit 0
"#,
            log = log.display(),
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).unwrap();
    bin_dir
}

#[cfg(unix)]
fn write_lock_probe_tmux(root: &Path) -> (PathBuf, PathBuf) {
    let bin_dir = root.join("bin-lock-probe-tmux");
    let handshake = root.join("init-lock-handshake");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(&handshake).unwrap();
    let script = bin_dir.join("tmux");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
handshake="{handshake}"
case "$1" in
  display-message)
    if [ "$2" != "-p" ]; then
      : > "$handshake/second-waiting"
    fi
    exit 0 ;;
  run-shell)
    if mkdir "$handshake/first-loader" 2>/dev/null; then
      : > "$handshake/first-loading"
      attempts=0
      while [ ! -f "$handshake/release-first" ]; do
        attempts=$((attempts + 1))
        [ "$attempts" -lt 500 ] || exit 72
        sleep 0.01
      done
    else
      : > "$handshake/second-loading"
    fi
    exit 0 ;;
  -V) printf 'tmux 1.9\n'; exit 0 ;;
  *) exit 0 ;;
esac
"#,
            handshake = handshake.display(),
        ),
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&script).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&script, permissions).unwrap();
    (bin_dir, handshake)
}

#[cfg(unix)]
fn wait_for_path(path: &Path) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if path.exists() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[cfg(unix)]
fn public_init_command(config_path: &Path, root: &Path, path: &str) -> std::process::Command {
    let binary = Command::cargo_bin("tmup").unwrap().get_program().to_owned();
    let mut command = std::process::Command::new(binary);
    command
        .arg("init")
        .env("TMUP_CONFIG", config_path)
        .env("XDG_CONFIG_HOME", root.join("xdg-config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", root.join("state"))
        .env("PATH", path);
    command
}

fn make_plugin(clone_url: &str, tracking: Tracking, build: Option<&str>) -> PluginSpec {
    PluginSpec {
        source: PluginSource::Remote {
            raw: "test/plugin".into(),
            id: "example.com/test/plugin".into(),
            clone_url: clone_url.into(),
        },
        name: "plugin".into(),
        opt_prefix: String::new(),
        tracking,
        build: build.map(String::from),
        opts: vec![],
        environment: vec![],
    }
}

fn make_config_from_plugin(plugin: PluginSpec) -> Config {
    Config { options: Options::default(), plugins: vec![plugin] }
}

#[tokio::test]
async fn init_does_not_retry_same_failed_build_tuple() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_url = format!("file://{}", bare.display());
    let marker_path = dir.path().join("build-retried.marker");
    let build_cmd = format!(": > \"{}\"; exit 1", marker_path.display());
    let plugin = make_plugin(&clone_url, Tracking::DefaultBranch, Some(&build_cmd));
    let old_plugin = make_plugin(&clone_url, Tracking::DefaultBranch, Some("touch old.marker"));
    let cfg = make_config_from_plugin(plugin);

    let mut lock = LockFile::new();
    let mut entry = LockEntry::default_branch("main", &commit);
    entry.config_hash = remote_plugin_config_hash(&old_plugin);
    lock.plugins.insert("example.com/test/plugin".into(), entry);

    let bh = build_command_hash(&build_cmd);
    let marker = tmup::state::FailureMarker {
        plugin_id: "example.com/test/plugin".into(),
        commit: commit.clone(),
        build_hash: bh.clone(),
        build_command: build_cmd.clone(),
        failed_at: "now".into(),
        stderr_summary: "error".into(),
    };
    tmup::state::write_failure_marker(&paths.failures_root, &marker).unwrap();

    let outcome = sync::run_and_write(
        &cfg,
        &mut lock,
        &paths,
        None,
        sync::SyncPolicy::init(true),
        sync::SyncMode::Init,
        &NullReporter,
    )
    .await
    .unwrap();

    assert!(
        outcome.plugin_failures.is_empty(),
        "init-mode sync should suppress known failed (id, commit, build) tuples"
    );
    assert!(
        !marker_path.exists(),
        "init-mode sync should skip publish/build when tuple is already known-failed"
    );
}

#[tokio::test]
async fn init_retries_when_build_command_changes() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_url = format!("file://{}", bare.display());
    let marker_path = dir.path().join("build-retried.marker");
    let previous_build = "make install";
    let new_build = format!(": > \"{}\"; exit 1", marker_path.display());
    let plugin = make_plugin(&clone_url, Tracking::DefaultBranch, Some(&new_build));
    let old_plugin = make_plugin(&clone_url, Tracking::DefaultBranch, Some(previous_build));
    let cfg = make_config_from_plugin(plugin);

    let mut lock = LockFile::new();
    let mut entry = LockEntry::default_branch("main", &commit);
    entry.config_hash = remote_plugin_config_hash(&old_plugin);
    lock.plugins.insert("example.com/test/plugin".into(), entry);

    let marker = tmup::state::FailureMarker {
        plugin_id: "example.com/test/plugin".into(),
        commit: commit.clone(),
        build_hash: build_command_hash(previous_build),
        build_command: previous_build.into(),
        failed_at: "now".into(),
        stderr_summary: "error".into(),
    };
    tmup::state::write_failure_marker(&paths.failures_root, &marker).unwrap();

    let outcome = sync::run_and_write(
        &cfg,
        &mut lock,
        &paths,
        None,
        sync::SyncPolicy::init(true),
        sync::SyncMode::Init,
        &NullReporter,
    )
    .await
    .unwrap();

    assert_eq!(
        outcome.plugin_failures.len(),
        1,
        "changed build command should not be suppressed and should retry"
    );
    assert!(
        marker_path.exists(),
        "changed build command should execute build and touch retry marker"
    );
}

#[tokio::test]
async fn init_preflight_sync_failure_preserves_previous_lock_snapshot() {
    let dir = tempdir().unwrap();
    let (bare, commit) = make_bare_repo(&dir.path().join("repo"));
    let paths = Paths::for_test(dir.path().join("data"), dir.path().join("state"));
    paths.ensure_dirs().unwrap();

    let clone_url = format!("file://{}", bare.display());
    let old_plugin = make_plugin(&clone_url, Tracking::DefaultBranch, Some("touch built-v1"));
    let new_plugin =
        make_plugin(&clone_url, Tracking::DefaultBranch, Some("touch built-v2; exit 1"));

    let mut lock = LockFile::new();
    let mut entry = LockEntry::default_branch("main", &commit);
    entry.config_hash = remote_plugin_config_hash(&old_plugin);
    lock.plugins.insert("example.com/test/plugin".into(), entry);
    lock.config_fingerprint = Some(config_fingerprint(&make_config_from_plugin(old_plugin)));

    let cfg = make_config_from_plugin(new_plugin);
    let result = sync::run_and_write(
        &cfg,
        &mut lock,
        &paths,
        None,
        sync::SyncPolicy::init(true),
        sync::SyncMode::Init,
        &NullReporter,
    )
    .await;
    let outcome = result.expect("init sync should surface plugin build failures in SyncOutcome");
    assert_eq!(outcome.plugin_failures.len(), 1, "expected one plugin-level sync failure");
    assert!(
        outcome.plugin_failures[0].contains("example.com/test/plugin"),
        "plugin failure should include plugin id"
    );

    let persisted = read_lockfile(&paths.lockfile_path).unwrap();
    let entry = persisted.plugins.get("example.com/test/plugin").unwrap();
    assert_eq!(entry.commit, commit);
    assert_eq!(entry.tracking.kind, "default-branch");
    assert_eq!(entry.config_hash, lock.plugins["example.com/test/plugin"].config_hash);
}

#[cfg(unix)]
#[test]
fn public_init_processes_serialize_through_tmux_loading() {
    let dir = tempdir().unwrap();
    let plugin_dir = dir.path().join("local-plugin");
    std::fs::create_dir_all(&plugin_dir).unwrap();
    std::fs::write(plugin_dir.join("init.tmux"), "#!/bin/sh\n").unwrap();
    let config_path = dir.path().join("tmup.kdl");
    std::fs::write(&config_path, format!(r#"plugin "{}" local=#true"#, plugin_dir.display()))
        .unwrap();
    let (fake_tmux_dir, handshake) = write_lock_probe_tmux(dir.path());
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let first = public_init_command(&config_path, dir.path(), &path).spawn().unwrap();
    let first_loading = wait_for_path(&handshake.join("first-loading"));
    if !first_loading {
        std::fs::write(handshake.join("release-first"), "").unwrap();
        let output = first.wait_with_output().unwrap();
        panic!(
            "first init never reached tmux loading; status={:?}, stderr={} ",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let second = public_init_command(&config_path, dir.path(), &path).spawn().unwrap();
    let second_waiting = wait_for_path(&handshake.join("second-waiting"));
    let loaded_before_release = handshake.join("second-loading").exists();
    std::fs::write(handshake.join("release-first"), "").unwrap();

    let first_output = first.wait_with_output().unwrap();
    let second_output = second.wait_with_output().unwrap();
    assert!(
        first_output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&first_output.stderr)
    );
    assert!(
        second_output.status.success(),
        "stderr:\n{}",
        String::from_utf8_lossy(&second_output.stderr)
    );
    assert!(second_waiting, "the second public init never observed lock contention");
    assert!(
        !loaded_before_release,
        "the second public init must not load tmux while the first still holds the operation lock"
    );
    assert!(
        handshake.join("second-loading").exists(),
        "the second public init should load after the first releases the operation lock"
    );
}

#[cfg(unix)]
#[test]
fn public_init_applies_literal_environment_operations_before_all_plugin_scripts() {
    let dir = tempdir().unwrap();
    make_remote_repo(dir.path());
    let gitconfig = write_git_rewrite_config(dir.path());
    let local_plugin = dir.path().join("local-plugin");
    let skipped_plugin = dir.path().join("skipped-plugin");
    for plugin in [&local_plugin, &skipped_plugin] {
        std::fs::create_dir_all(plugin).unwrap();
        std::fs::write(plugin.join("init.tmux"), "#!/bin/sh\n").unwrap();
    }
    let config_path = dir.path().join("tmup.kdl");
    std::fs::write(
        &config_path,
        format!(
            r#"
options {{ auto-install #true }}
plugin "https://example.com/test/plugin.git" opt-prefix="remote_" {{
    env "SHARED" "remote"
    env "LITERAL" "$HOME ~ ${{PLUGIN_DIR}}"
    unset-env "STALE"
    env "TMUX_PLUGIN_MANAGER_PATH" "plugin-owned"
    opt "mode" "one"
}}
plugin "{local_plugin}" local=#true opt-prefix="local_" {{
    env "SHARED" "local"
    unset-env "SHARED"
    env "SHARED" ""
    unset-env "TMUX_PLUGIN_MANAGER_PATH"
    opt "mode" "two"
}}
plugin "{skipped_plugin}" local=#true cond=#false {{
    env "SKIPPED" "no"
}}
plugin "user/disabled" enabled=#false {{
    env "DISABLED" "no"
}}
"#,
            local_plugin = local_plugin.display(),
            skipped_plugin = skipped_plugin.display(),
        ),
    )
    .unwrap();
    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_recording_tmux(dir.path(), &tmux_log);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let output = public_init_command(&config_path, dir.path(), &path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .env("HOME", dir.path().join("runtime-home"))
        .env("PLUGIN_DIR", "runtime-plugin-dir")
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr:\n{}", String::from_utf8_lossy(&output.stderr));
    let plugin_root = dir.path().join("data/tmup/plugins");
    assert_eq!(
        std::fs::read_to_string(&tmux_log).unwrap(),
        format!(
            "command\n\
arg=<set-environment>\n\
arg=<-g>\n\
arg=<TMUX_PLUGIN_MANAGER_PATH>\n\
arg=<{plugin_root}/>\n\
end\n\
command\n\
arg=<set-environment>\n\
arg=<-g>\n\
arg=<SHARED>\n\
arg=<remote>\n\
end\n\
command\n\
arg=<set-environment>\n\
arg=<-g>\n\
arg=<LITERAL>\n\
arg=<$HOME ~ ${{PLUGIN_DIR}}>\n\
end\n\
command\n\
arg=<set-environment>\n\
arg=<-gu>\n\
arg=<STALE>\n\
end\n\
command\n\
arg=<set-environment>\n\
arg=<-g>\n\
arg=<TMUX_PLUGIN_MANAGER_PATH>\n\
arg=<plugin-owned>\n\
end\n\
command\n\
arg=<set>\n\
arg=<-g>\n\
arg=<@remote_mode>\n\
arg=<one>\n\
end\n\
command\n\
arg=<set-environment>\n\
arg=<-g>\n\
arg=<SHARED>\n\
arg=<local>\n\
end\n\
command\n\
arg=<set-environment>\n\
arg=<-gu>\n\
arg=<SHARED>\n\
end\n\
command\n\
arg=<set-environment>\n\
arg=<-g>\n\
arg=<SHARED>\n\
arg=<>\n\
end\n\
command\n\
arg=<set-environment>\n\
arg=<-gu>\n\
arg=<TMUX_PLUGIN_MANAGER_PATH>\n\
end\n\
command\n\
arg=<set>\n\
arg=<-g>\n\
arg=<@local_mode>\n\
arg=<two>\n\
end\n\
command\n\
arg=<run-shell>\n\
arg=<'{plugin_root}/example.com/test/plugin/init.tmux'>\n\
end\n\
command\n\
arg=<run-shell>\n\
arg=<'{local_plugin}/init.tmux'>\n\
end\n",
            plugin_root = plugin_root.display(),
            local_plugin = local_plugin.display(),
        )
    );
}

#[cfg(unix)]
#[test]
fn public_init_hard_condition_error_precedes_managed_directory_creation() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config/tmux/tmup.kdl");
    write_file(&config_path, r#"plugin "user/repo" enabled="kill -TERM $$""#);
    let path = std::env::var("PATH").unwrap_or_default();

    let output = public_init_command(&config_path, dir.path(), &path).output().unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("terminated by a signal"),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for managed_path in [
        dir.path().join("data/tmup/plugins"),
        dir.path().join("data/tmup/.staging"),
        dir.path().join("data/tmup/.repos"),
        dir.path().join("state/tmup/failures"),
        dir.path().join("state/tmup/logs"),
    ] {
        assert!(
            !managed_path.exists(),
            "hard condition error must precede managed-state creation: {}",
            managed_path.display()
        );
    }
}

#[cfg(unix)]
#[test]
fn public_init_hard_load_condition_error_precedes_managed_directory_creation() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config/tmux/tmup.kdl");
    write_file(&config_path, r#"plugin "user/repo" cond="kill -TERM $$""#);
    let path = std::env::var("PATH").unwrap_or_default();

    let output = public_init_command(&config_path, dir.path(), &path).output().unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("cond shell predicate terminated by a signal"),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    for managed_path in [
        dir.path().join("data/tmup/plugins"),
        dir.path().join("data/tmup/.staging"),
        dir.path().join("data/tmup/.repos"),
        dir.path().join("state/tmup/failures"),
        dir.path().join("state/tmup/logs"),
    ] {
        assert!(
            !managed_path.exists(),
            "hard load condition error must precede managed-state creation: {}",
            managed_path.display()
        );
    }
}

#[cfg(unix)]
#[test]
fn public_init_does_not_advance_a_locked_remote_revision() {
    let dir = tempdir().unwrap();
    let bare = make_remote_repo(dir.path());
    let initial_commit = git(&["rev-parse", "refs/heads/main"], &bare);
    let gitconfig = write_git_rewrite_config(dir.path());
    let config_path = dir.path().join("tmup.kdl");
    std::fs::write(
        &config_path,
        "options { auto-install #true }\nplugin \"https://example.com/test/plugin.git\"\n",
    )
    .unwrap();
    let fake_tmux_dir = write_inline_tmux(dir.path());
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let first = public_init_command(&config_path, dir.path(), &path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .output()
        .unwrap();
    assert!(first.status.success(), "stderr:\n{}", String::from_utf8_lossy(&first.stderr));

    let newer_commit = push_commit(&bare, "newer");
    assert_ne!(newer_commit, initial_commit);

    let second = public_init_command(&config_path, dir.path(), &path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .output()
        .unwrap();
    assert!(second.status.success(), "stderr:\n{}", String::from_utf8_lossy(&second.stderr));

    let lock = read_lockfile(&dir.path().join("tmup.lock")).unwrap();
    assert_eq!(lock.plugins["example.com/test/plugin"].commit, initial_commit);
    let installed = dir.path().join("data/tmup/plugins/example.com/test/plugin");
    assert_eq!(git(&["rev-parse", "HEAD"], &installed), initial_commit);
    assert_ne!(
        git(&["rev-parse", "refs/heads/main"], &bare),
        lock.plugins["example.com/test/plugin"].commit,
        "the remote must actually contain a newer revision than the lock snapshot"
    );
}
