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
use tmup::model::{Config, Options, PluginSource, PluginSpec, RuntimeConfiguration, Tracking};
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
  set-environment|set|run-shell|bind-key)
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
fn write_selectively_failing_tmux(root: &Path, log: &Path) -> PathBuf {
    let bin_dir = root.join("bin-selectively-failing-tmux");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script = bin_dir.join("tmux");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
case "$1" in
  -V) printf 'tmux 1.9\n'; exit 0 ;;
esac
printf 'command' >> '{log}'
for arg do
  printf '|%s' "$arg" >> '{log}'
done
printf '\n' >> '{log}'
for arg do
  case "$arg" in
    *"$TMUX_FAIL_ARG"*)
      printf 'selected tmux failure: %s\n' "$TMUX_FAIL_ARG" >&2
      exit 73
      ;;
  esac
done
printf 'applied\n' >> '{log}'
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
        runtime: RuntimeConfiguration::Unresolved,
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
        outcome.load_excluded_plugin_ids().contains("example.com/test/plugin"),
        "known failed desired state must not load against the preserved checkout"
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
    assert_eq!(outcome.load_excluded_plugin_ids().len(), 1);
    assert!(outcome.load_excluded_plugin_ids().contains("example.com/test/plugin"));

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
    std::fs::write(&config_path, format!(r#"plug "{}" local=#true"#, plugin_dir.display()))
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
    let local_plugin = dir.path().join("local plugin's repo");
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
plug "https://example.com/test/plugin.git" opt-prefix="remote_" {{
    env "SHARED" "remote"
    env "LITERAL" "$HOME ~ ${{PLUGIN_DIR}}"
    unset-env "STALE"
    env "TMUX_PLUGIN_MANAGER_PATH" "plugin-owned"
    opt "mode" "one"
    bind "C-w" {{
        options "-n" "-r" "-T" "root"
        shell "scripts/session.sh attach | tee \"$TMUX_FZF_LOG\""
    }}
}}
plug "$LOCAL_PLUGIN" local=#true opt-prefix="local_" {{
    env "SHARED" "local"
    unset-env "SHARED"
    env "SHARED" ""
    unset-env "TMUX_PLUGIN_MANAGER_PATH"
    opt "mode" "two"
    bind "C-w" {{
        shell "./launch > \"$OUTPUT\"" background=#true
    }}
}}
plug "{skipped_plugin}" local=#true cond=#false {{
    env "SKIPPED" "no"
    bind "skipped" {{ shell "false" }}
}}
plug "user/disabled" enabled=#false {{
    env "DISABLED" "no"
    bind "disabled" {{ shell "false" }}
}}
"#,
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
        .env("LOCAL_PLUGIN", &local_plugin)
        .env("PLUGIN_DIR", "runtime-plugin-dir")
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr:\n{}", String::from_utf8_lossy(&output.stderr));
    let plugin_root = dir.path().join("data/tmup/plugins");
    let local_plugin_shell = local_plugin.display().to_string().replace('\'', "'\"'\"'");
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
arg=<'{local_plugin_shell}/init.tmux'>\n\
end\n\
command\n\
arg=<bind-key>\n\
arg=<-n>\n\
arg=<-r>\n\
arg=<-T>\n\
arg=<root>\n\
arg=<C-w>\n\
arg=<run-shell>\n\
arg=<cd '{plugin_root}/example.com/test/plugin' && exec /bin/sh -c 'scripts/session.sh attach | tee \"$TMUX_FZF_LOG\"'>\n\
end\n\
command\n\
arg=<bind-key>\n\
arg=<C-w>\n\
arg=<run-shell>\n\
arg=<-b>\n\
arg=<cd '{local_plugin_shell}' && exec /bin/sh -c './launch > \"$OUTPUT\"'>\n\
end\n",
            plugin_root = plugin_root.display(),
            local_plugin_shell = local_plugin_shell,
        )
    );
}

#[cfg(unix)]
#[test]
fn public_init_isolates_each_plugin_attributable_tmux_failure() {
    let cases = [
        ("A_FAIL_ENV", "A_BEFORE", &["A_AFTER_ENV", "@a_fail", "a-00-before.tmux", "A-before"][..]),
        ("@a_fail", "A_FAIL_ENV", &["@a_after", "a-00-before.tmux", "A-before"]),
        ("a-10-fail.tmux", "a-00-before.tmux", &["a-20-after.tmux", "A-before"]),
        ("A-fail", "A-before", &["A-after"]),
    ];

    for (fail_arg, earlier_success, skipped_after_failure) in cases {
        let dir = tempdir().unwrap();
        let plugin_a = dir.path().join("plugin-a");
        let plugin_b = dir.path().join("plugin-b");
        for plugin in [&plugin_a, &plugin_b] {
            std::fs::create_dir_all(plugin).unwrap();
        }
        for script in ["a-00-before.tmux", "a-10-fail.tmux", "a-20-after.tmux"] {
            std::fs::write(plugin_a.join(script), "#!/bin/sh\n").unwrap();
        }
        std::fs::write(plugin_b.join("b.tmux"), "#!/bin/sh\n").unwrap();
        let config_path = dir.path().join("tmup.kdl");
        std::fs::write(
            &config_path,
            format!(
                r#"
plug "{}" local=#true opt-prefix="a_" {{
    env "A_BEFORE" "yes"
    env "A_FAIL_ENV" "yes"
    env "A_AFTER_ENV" "yes"
    opt "fail" "yes"
    opt "after" "yes"
    bind "A-before" {{ shell "true" }}
    bind "A-fail" {{ shell "true" }}
    bind "A-after" {{ shell "true" }}
}}
plug "{}" local=#true opt-prefix="b_" {{
    env "B_ENV" "yes"
    opt "ok" "yes"
    bind "B-ok" {{ shell "true" }}
}}
"#,
                plugin_a.display(),
                plugin_b.display(),
            ),
        )
        .unwrap();
        let tmux_log = dir.path().join("tmux.log");
        let fake_tmux_dir = write_selectively_failing_tmux(dir.path(), &tmux_log);
        let path =
            format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

        let output = public_init_command(&config_path, dir.path(), &path)
            .env("TMUX_FAIL_ARG", fail_arg)
            .output()
            .unwrap();

        assert!(!output.status.success(), "{fail_arg} must make init fail");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.contains(&plugin_a.display().to_string()), "stderr:\n{stderr}");
        assert!(stderr.contains("selected tmux failure"), "stderr:\n{stderr}");
        let log = std::fs::read_to_string(&tmux_log).unwrap();
        assert!(log.contains(fail_arg), "the failing command must reach tmux:\n{log}");
        let lines: Vec<_> = log.lines().collect();
        let earlier_index =
            lines.iter().position(|line| line.contains(earlier_success)).unwrap_or_else(|| {
                panic!("missing earlier successful action {earlier_success}:\n{log}")
            });
        assert!(
            lines.get(earlier_index + 1).is_some_and(|line| *line == "applied"),
            "an earlier successful effect must remain applied:\n{log}"
        );
        for skipped in skipped_after_failure {
            assert!(!log.contains(skipped), "{skipped} must be skipped after {fail_arg}:\n{log}");
        }
        for neighbor_action in ["B_ENV", "@b_ok", "b.tmux", "B-ok"] {
            assert!(
                log.contains(neighbor_action),
                "independent plugin action {neighbor_action} must continue after {fail_arg}:\n{log}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn public_init_omits_failed_remote_reconciliation_without_replacing_working_state() {
    let dir = tempdir().unwrap();
    make_remote_repo(dir.path());
    let gitconfig = write_git_rewrite_config(dir.path());
    let neighbor = dir.path().join("neighbor");
    std::fs::create_dir_all(&neighbor).unwrap();
    std::fs::write(neighbor.join("neighbor.tmux"), "#!/bin/sh\n").unwrap();
    let config_path = dir.path().join("tmup.kdl");
    std::fs::write(
        &config_path,
        r#"
options { auto-install #true }
plug "https://example.com/test/plugin.git" build="printf old > built-version"
"#,
    )
    .unwrap();
    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_recording_tmux(dir.path(), &tmux_log);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let initial = public_init_command(&config_path, dir.path(), &path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .output()
        .unwrap();
    assert!(initial.status.success(), "stderr:\n{}", String::from_utf8_lossy(&initial.stderr));
    let lock_path = dir.path().join("tmup.lock");
    let original_lock = std::fs::read_to_string(&lock_path).unwrap();
    std::fs::write(&tmux_log, "").unwrap();
    std::fs::write(
        &config_path,
        format!(
            r#"
options {{ auto-install #true }}
plug "https://example.com/test/plugin.git" build="printf new > built-version; exit 41" opt-prefix="remote_" {{
    env "REMOTE_ENV" "must-not-load"
    opt "mode" "must-not-load"
    bind "REMOTE-bind" {{ shell "true" }}
}}
plug "{}" local=#true opt-prefix="neighbor_" {{
    env "NEIGHBOR_ENV" "loaded"
    opt "mode" "loaded"
    bind "NEIGHBOR-bind" {{ shell "true" }}
}}
"#,
            neighbor.display(),
        ),
    )
    .unwrap();

    let output = public_init_command(&config_path, dir.path(), &path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("example.com/test/plugin"), "stderr:\n{stderr}");
    let log = std::fs::read_to_string(&tmux_log).unwrap();
    for failed_plugin_action in
        ["REMOTE_ENV", "@remote_mode", "example.com/test/plugin/init.tmux", "REMOTE-bind"]
    {
        assert!(
            !log.contains(failed_plugin_action),
            "failed plugin action {failed_plugin_action} must be omitted:\n{log}"
        );
    }
    for neighbor_action in ["NEIGHBOR_ENV", "@neighbor_mode", "neighbor.tmux", "NEIGHBOR-bind"] {
        assert!(
            log.contains(neighbor_action),
            "unaffected plugin action {neighbor_action} must continue:\n{log}"
        );
    }
    assert_eq!(std::fs::read_to_string(&lock_path).unwrap(), original_lock);
    let installed = dir.path().join("data/tmup/plugins/example.com/test/plugin");
    assert_eq!(std::fs::read_to_string(installed.join("built-version")).unwrap(), "old");
    let markers =
        tmup::state::read_failure_markers(&dir.path().join("state/tmup/failures")).unwrap();
    assert_eq!(markers.len(), 1);
    assert_eq!(markers[0].plugin_id, "example.com/test/plugin");

    std::fs::write(&tmux_log, "").unwrap();
    let known_failure = public_init_command(&config_path, dir.path(), &path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .output()
        .unwrap();

    assert!(
        known_failure.status.success(),
        "a suppressed known failure is not a new sync failure: stderr:\n{}",
        String::from_utf8_lossy(&known_failure.stderr)
    );
    let log = std::fs::read_to_string(&tmux_log).unwrap();
    for failed_plugin_action in
        ["REMOTE_ENV", "@remote_mode", "example.com/test/plugin/init.tmux", "REMOTE-bind"]
    {
        assert!(
            !log.contains(failed_plugin_action),
            "known-failed plugin action {failed_plugin_action} must remain omitted:\n{log}"
        );
    }
    for neighbor_action in ["NEIGHBOR_ENV", "@neighbor_mode", "neighbor.tmux", "NEIGHBOR-bind"] {
        assert!(
            log.contains(neighbor_action),
            "unaffected plugin action {neighbor_action} must continue after known failure:\n{log}"
        );
    }
}

#[cfg(unix)]
#[test]
fn public_init_omits_remote_runtime_configuration_after_preparation_failure() {
    let dir = tempdir().unwrap();
    make_remote_repo(dir.path());
    let gitconfig = write_git_rewrite_config(dir.path());
    let neighbor = dir.path().join("neighbor");
    std::fs::create_dir_all(&neighbor).unwrap();
    std::fs::write(neighbor.join("neighbor.tmux"), "#!/bin/sh\n").unwrap();
    let config_path = dir.path().join("tmup.kdl");
    std::fs::write(
        &config_path,
        r#"
options { auto-install #true }
plug "https://example.com/test/plugin.git"
"#,
    )
    .unwrap();
    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_recording_tmux(dir.path(), &tmux_log);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let initial = public_init_command(&config_path, dir.path(), &path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .output()
        .unwrap();
    assert!(initial.status.success(), "stderr:\n{}", String::from_utf8_lossy(&initial.stderr));
    let lock_path = dir.path().join("tmup.lock");
    let original_lock = std::fs::read_to_string(&lock_path).unwrap();
    let installed = dir.path().join("data/tmup/plugins/example.com/test/plugin");
    let original_commit = git(&["rev-parse", "HEAD"], &installed);

    std::fs::write(&tmux_log, "").unwrap();
    std::fs::write(
        &config_path,
        format!(
            r#"
options {{ auto-install #true }}
plug "https://example.com/test/plugin.git" branch="missing" opt-prefix="remote_" {{
    env "REMOTE_ENV" "must-not-load"
    opt "mode" "must-not-load"
    bind "REMOTE-bind" {{ shell "true" }}
}}
plug "{}" local=#true opt-prefix="neighbor_" {{
    env "NEIGHBOR_ENV" "loaded"
    opt "mode" "loaded"
    bind "NEIGHBOR-bind" {{ shell "true" }}
}}
"#,
            neighbor.display(),
        ),
    )
    .unwrap();

    let output = public_init_command(&config_path, dir.path(), &path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .output()
        .unwrap();

    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("example.com/test/plugin"), "stderr:\n{stderr}");
    let log = std::fs::read_to_string(&tmux_log).unwrap();
    for failed_plugin_action in
        ["REMOTE_ENV", "@remote_mode", "example.com/test/plugin/init.tmux", "REMOTE-bind"]
    {
        assert!(
            !log.contains(failed_plugin_action),
            "preparation-failed plugin action {failed_plugin_action} must be omitted:\n{log}"
        );
    }
    for neighbor_action in ["NEIGHBOR_ENV", "@neighbor_mode", "neighbor.tmux", "NEIGHBOR-bind"] {
        assert!(
            log.contains(neighbor_action),
            "unaffected plugin action {neighbor_action} must continue:\n{log}"
        );
    }
    assert_eq!(std::fs::read_to_string(&lock_path).unwrap(), original_lock);
    assert_eq!(git(&["rev-parse", "HEAD"], &installed), original_commit);
}

#[cfg(unix)]
#[test]
fn public_init_aggregates_plugin_failures_but_keeps_global_setup_failure_operation_level() {
    let dir = tempdir().unwrap();
    let plugin_a = dir.path().join("plugin-a");
    let plugin_b = dir.path().join("plugin-b");
    let plugin_c = dir.path().join("plugin-c");
    for plugin in [&plugin_a, &plugin_b, &plugin_c] {
        std::fs::create_dir_all(plugin).unwrap();
    }
    let config_path = dir.path().join("tmup.kdl");
    std::fs::write(
        &config_path,
        format!(
            r#"
plug "{}" local=#true {{
    env "A_FAIL_ENV" "yes"
    env "A_AFTER" "no"
}}
plug "{}" local=#true {{
    env "B_FAIL_ENV" "yes"
    env "B_AFTER" "no"
}}
plug "{}" local=#true {{
    env "C_CONTINUES" "yes"
}}
"#,
            plugin_a.display(),
            plugin_b.display(),
            plugin_c.display(),
        ),
    )
    .unwrap();
    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_selectively_failing_tmux(dir.path(), &tmux_log);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let plugin_failures = public_init_command(&config_path, dir.path(), &path)
        .env("TMUX_FAIL_ARG", "FAIL_ENV")
        .output()
        .unwrap();

    assert!(!plugin_failures.status.success());
    let stderr = String::from_utf8_lossy(&plugin_failures.stderr);
    assert!(stderr.contains("2 failure(s)"), "stderr:\n{stderr}");
    assert!(stderr.contains(&plugin_a.display().to_string()), "stderr:\n{stderr}");
    assert!(stderr.contains(&plugin_b.display().to_string()), "stderr:\n{stderr}");
    let log = std::fs::read_to_string(&tmux_log).unwrap();
    assert!(!log.contains("A_AFTER"), "{log}");
    assert!(!log.contains("B_AFTER"), "{log}");
    assert!(log.contains("C_CONTINUES"), "{log}");

    std::fs::write(&tmux_log, "").unwrap();
    let global_failure = public_init_command(&config_path, dir.path(), &path)
        .env("TMUX_FAIL_ARG", "TMUX_PLUGIN_MANAGER_PATH")
        .output()
        .unwrap();

    assert!(!global_failure.status.success());
    let stderr = String::from_utf8_lossy(&global_failure.stderr);
    assert!(stderr.contains("tmux set-environment failed"), "stderr:\n{stderr}");
    assert!(!stderr.contains("init encountered"), "stderr:\n{stderr}");
    let log = std::fs::read_to_string(&tmux_log).unwrap();
    assert!(log.contains("TMUX_PLUGIN_MANAGER_PATH"), "{log}");
    assert!(!log.contains("A_FAIL_ENV"), "global setup failure must abort plugin loading:\n{log}");
}

#[cfg(unix)]
#[test]
fn public_init_rejects_malformed_integration_nodes_before_tmux_loading_or_state_mutation() {
    let dir = tempdir().unwrap();
    for (index, &(config, expected)) in
        MALFORMED_ENVIRONMENT_NODES.iter().chain(MALFORMED_BINDING_NODES).enumerate()
    {
        let case_root = dir.path().join(format!("case-{index}"));
        std::fs::create_dir_all(&case_root).unwrap();
        let config_path = case_root.join("tmup.kdl");
        std::fs::write(&config_path, config).unwrap();
        let tmux_log = case_root.join("tmux.log");
        let fake_tmux_dir = write_recording_tmux(&case_root, &tmux_log);
        let path =
            format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

        let output = public_init_command(&config_path, &case_root, &path).output().unwrap();

        assert!(!output.status.success(), "config={config:?} must fail");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(expected),
            "config={config:?} stderr:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!tmux_log.exists(), "config={config:?} must fail before tmux loading");
        assert!(
            !case_root.join("data/tmup/plugins").exists(),
            "config={config:?} mutated plugin state"
        );
        assert!(!case_root.join("state/tmup").exists(), "config={config:?} mutated runtime state");
    }
}

#[cfg(unix)]
#[test]
fn public_init_hard_condition_error_precedes_managed_directory_creation() {
    let dir = tempdir().unwrap();
    let config_path = dir.path().join("config/tmux/tmup.kdl");
    write_file(&config_path, r#"plug "user/repo" enabled="kill -TERM $$""#);
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
    write_file(&config_path, r#"plug "user/repo" cond="kill -TERM $$""#);
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
fn public_init_hard_runtime_branch_error_precedes_remote_install_and_tmux_loading() {
    let dir = tempdir().unwrap();
    make_remote_repo(dir.path());
    let gitconfig = write_git_rewrite_config(dir.path());
    let config_path = dir.path().join("tmup.kdl");
    write_file(
        &config_path,
        r#"
options { auto-install #true }
plug "https://example.com/test/plugin.git" {
    if "kill -TERM $$" {
        bind "unreachable" { shell "./unreachable" }
    }
}
"#,
    );
    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_recording_tmux(dir.path(), &tmux_log);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let output = public_init_command(&config_path, dir.path(), &path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains("if shell predicate terminated by a signal"),
        "stderr:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!dir.path().join("tmup.lock").exists(), "branch failure must not create a lock entry");
    assert!(
        !dir.path().join("data/tmup/plugins/example.com/test/plugin").exists(),
        "branch failure must precede the managed checkout"
    );
    assert!(!tmux_log.exists(), "branch failure must precede all tmux loading effects");
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
        "options { auto-install #true }\nplug \"https://example.com/test/plugin.git\"\n",
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
