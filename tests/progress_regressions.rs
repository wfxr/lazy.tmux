mod utils;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::tempdir;
use utils::{make_remote_repo, make_remote_repo_named, write_git_rewrite_config};

fn write_fake_tmux_with_log(root: &Path, log_path: &Path) -> PathBuf {
    let bin_dir = root.join("bin-with-log");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script = bin_dir.join("tmux");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1" in
  -V) printf 'tmux 3.3a\n'; exit 0 ;;
  set-environment|set|run-shell|wait-for|display-message|display-popup|split-window|set-option) exit 0 ;;
  *) exit 0 ;;
esac
"#,
            log = log_path.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    bin_dir
}

fn write_fake_tmux_bootstrap_with_log(
    root: &Path,
    log_path: &Path,
    popup_status: i32,
    split_status: i32,
) -> PathBuf {
    let bin_dir = root.join(format!("bin-bootstrap-{popup_status}-{split_status}"));
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script = bin_dir.join("tmux");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1" in
  -V) printf 'tmux 3.3a\n'; exit 0 ;;
  display-message)
    if [ "$2" = "-p" ]; then
      case "$3" in
        '#{{client_name}}') printf '/dev/pts/99\n'; exit 0 ;;
        '#{{pane_id}}') printf '%%42\n'; exit 0 ;;
      esac
    fi
    exit 0 ;;
  display-popup) exit {popup_status} ;;
  split-window) printf '%%42\n'; exit {split_status} ;;
  run-shell|wait-for|set-environment|set|set-option) exit 0 ;;
  *) exit 0 ;;
esac
"#,
            log = log_path.display(),
            popup_status = popup_status,
            split_status = split_status,
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    bin_dir
}

fn write_fake_tmux_versioned_with_log(
    root: &Path,
    log_path: &Path,
    version: &str,
    popup_status: i32,
    split_status: i32,
) -> PathBuf {
    let safe_version = version.replace([' ', '.'], "-");
    let bin_dir = root.join(format!("bin-versioned-{safe_version}-{popup_status}-{split_status}"));
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script = bin_dir.join("tmux");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1" in
  -V) printf '%s\n' '{version}'; exit 0 ;;
  display-message)
    if [ "$2" = "-p" ]; then
      case "$3" in
        '#{{client_name}}') printf '/dev/pts/99\n'; exit 0 ;;
        '#{{pane_id}}') printf '%%42\n'; exit 0 ;;
      esac
    fi
    exit 0 ;;
  display-popup) exit {popup_status} ;;
  split-window) exit {split_status} ;;
  run-shell|wait-for|set-environment|set|set-option) exit 0 ;;
  *) exit 0 ;;
esac
"#,
            log = log_path.display(),
            version = version,
            popup_status = popup_status,
            split_status = split_status,
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    bin_dir
}

fn write_fake_tmux_executing_popup(root: &Path, log_path: &Path) -> PathBuf {
    let bin_dir = root.join("bin-executing-popup");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script = bin_dir.join("tmux");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1" in
  -V) printf 'tmux 3.3a\n'; exit 0 ;;
  display-message)
    if [ "$2" = "-p" ]; then
      case "$3" in
        '#{{client_name}}') printf '/dev/pts/99\n'; exit 0 ;;
        '#{{pane_id}}') printf '%%42\n'; exit 0 ;;
      esac
    fi
    exit 0 ;;
  display-popup)
    for wrapper do :; done
    printf 'event:popup-entered\n' >> "{log}"
    sh -c "$wrapper"
    status=$?
    printf 'event:popup-returned\n' >> "{log}"
    exit "$status" ;;
  run-shell|wait-for|split-window|set-environment|set|set-option) exit 0 ;;
  *) exit 0 ;;
esac
"#,
            log = log_path.display(),
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    bin_dir
}

fn write_fake_tmux_executing_split(root: &Path, log_path: &Path, state_home: &Path) -> PathBuf {
    let bin_dir = root.join("bin-executing-split");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script = bin_dir.join("tmux");
    let handshake = root.join("split-handshake");
    std::fs::create_dir_all(&handshake).unwrap();
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
handshake="{handshake}"
printf '%s\n' "$*" >> "{log}"
case "$1" in
  -V) printf 'tmux 2.0\n'; exit 0 ;;
  display-message)
    if [ "$2" = "-p" ]; then
      case "$3" in
        '#{{client_name}}') printf '/dev/pts/99\n'; exit 0 ;;
        '#{{pane_id}}') printf '%%42\n'; exit 0 ;;
      esac
    fi
    exit 0 ;;
  split-window)
    for wrapper do :; done
    (
      attempts=0
      while [ ! -f "$handshake/wait-started" ]; do
        attempts=$((attempts + 1))
        [ "$attempts" -lt 500 ] || exit 70
        sleep 0.01
      done
      sh -c "$wrapper"
    ) >/dev/null 2>&1 &
    printf 'event:split-returned\n' >> "{log}"
    exit 0 ;;
  wait-for)
    if [ "$2" = "-S" ]; then
      result_present=false
      for result in "{state_home}"/tmup/init-sessions/session-*/child-result.json; do
        if [ -f "$result" ]; then
          result_present=true
          break
        fi
      done
      printf 'event:signal-result-%s\n' "$result_present" >> "{log}"
      : > "$handshake/signal-$3"
      exit 0
    fi
    : > "$handshake/wait-started"
    attempts=0
    while [ ! -f "$handshake/signal-$2" ]; do
      attempts=$((attempts + 1))
      [ "$attempts" -lt 500 ] || exit 71
      sleep 0.01
    done
    printf 'event:wait-returned\n' >> "{log}"
    exit 0 ;;
  run-shell|display-popup|set-environment|set|set-option) exit 0 ;;
  *) exit 0 ;;
esac
"#,
            handshake = handshake.display(),
            log = log_path.display(),
            state_home = state_home.display(),
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    bin_dir
}

fn write_fake_tmux_split_wait_failure(root: &Path, log_path: &Path) -> PathBuf {
    let bin_dir = root.join("bin-split-wait-failure");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script = bin_dir.join("tmux");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
printf '%s\n' "$*" >> "{log}"
case "$1" in
  -V) printf 'tmux 2.0\n'; exit 0 ;;
  display-message)
    if [ "$2" = "-p" ]; then
      case "$3" in
        '#{{client_name}}') printf '/dev/pts/99\n'; exit 0 ;;
        '#{{pane_id}}') printf '%%42\n'; exit 0 ;;
      esac
    fi
    exit 0 ;;
  split-window) exit 0 ;;
  wait-for) exit 1 ;;
  run-shell|display-popup|set-environment|set|set-option) exit 0 ;;
  *) exit 0 ;;
esac
"#,
            log = log_path.display(),
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    bin_dir
}

fn write_fake_tmux_retry_probe_with_log(root: &Path, log_path: &Path) -> PathBuf {
    write_fake_tmux_retry_probe_until_with_log(root, log_path, 1)
}

fn write_fake_tmux_retry_probe_after_delay_with_log(
    root: &Path,
    log_path: &Path,
    ready_after_ms: u64,
) -> PathBuf {
    let bin_dir = root.join(format!("bin-retry-probe-after-{ready_after_ms}ms"));
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script = bin_dir.join("tmux");
    let start_ms_file = root.join("probe-start-ms");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
start_ms_file="{start_ms_file}"
printf '%s\n' "$*" >> "{log}"
now_ms=$(date +%s%3N)
case "$1" in
  -V) printf 'tmux 3.3a\n'; exit 0 ;;
  display-message)
    if [ "$2" = "-p" ]; then
      if [ ! -f "$start_ms_file" ]; then
        printf '%s' "$now_ms" > "$start_ms_file"
      fi
      start_ms=$(cat "$start_ms_file")
      elapsed_ms=$((now_ms - start_ms))
      if [ "$elapsed_ms" -lt {ready_after_ms} ]; then
        printf '%s\n' 'no current client' >&2
        exit 1
      fi
      case "$3" in
        '#{{client_name}}') printf '/dev/pts/99\n'; exit 0 ;;
        '#{{pane_id}}') printf '%%42\n'; exit 0 ;;
      esac
    fi
    exit 0 ;;
  display-popup) exit 0 ;;
  split-window) printf '%%42\n'; exit 1 ;;
  run-shell|wait-for|set-environment|set|set-option) exit 0 ;;
  *) exit 0 ;;
esac
"#,
            start_ms_file = start_ms_file.display(),
            log = log_path.display(),
            ready_after_ms = ready_after_ms,
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    bin_dir
}

fn write_fake_tmux_retry_probe_until_with_log(
    root: &Path,
    log_path: &Path,
    fail_display_message_calls: usize,
) -> PathBuf {
    let bin_dir = root.join("bin-retry-probe");
    std::fs::create_dir_all(&bin_dir).unwrap();
    let script = bin_dir.join("tmux");
    let probe_count = root.join("probe-count");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
count_file="{count_file}"
printf '%s\n' "$*" >> "{log}"
case "$1" in
  -V) printf 'tmux 3.3a\n'; exit 0 ;;
  display-message)
    if [ "$2" = "-p" ]; then
      count=0
      [ -f "$count_file" ] && count=$(cat "$count_file")
      count=$((count + 1))
      printf '%s' "$count" > "$count_file"
      if [ "$count" -le {fail_display_message_calls} ]; then
        printf '%s\n' 'no current client' >&2
        exit 1
      fi
      case "$3" in
        '#{{client_name}}') printf '/dev/pts/99\n'; exit 0 ;;
        '#{{pane_id}}') printf '%%42\n'; exit 0 ;;
      esac
    fi
    exit 0 ;;
  display-popup) exit 0 ;;
  split-window) printf '%%42\n'; exit 1 ;;
  run-shell|wait-for|set-environment|set|set-option) exit 0 ;;
  *) exit 0 ;;
esac
"#,
            count_file = probe_count.display(),
            log = log_path.display(),
            fail_display_message_calls = fail_display_message_calls,
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    bin_dir
}

fn write_git_fetch_probe_wrapper(root: &Path, probe_dir: &Path) -> PathBuf {
    let bin_dir = root.join("bin-git-probe");
    std::fs::create_dir_all(&bin_dir).unwrap();
    std::fs::create_dir_all(probe_dir).unwrap();
    let script = bin_dir.join("git");
    std::fs::write(
        &script,
        format!(
            r#"#!/bin/sh
set -eu
probe_dir="{probe_dir}"
lock_dir="$probe_dir/.lock"

acquire_lock() {{
  while ! mkdir "$lock_dir" 2>/dev/null; do
    sleep 0.01
  done
}}

release_lock() {{
  rmdir "$lock_dir"
}}

read_num() {{
  file="$1"
  if [ -f "$file" ]; then
    cat "$file"
  else
    printf '%s\n' 0
  fi
}}

if [ "${{1-}}" = "fetch" ] && [ "${{2-}}" = "origin" ]; then
  acquire_lock
  in_flight=$(read_num "$probe_dir/in_flight")
  in_flight=$((in_flight + 1))
  printf '%s\n' "$in_flight" > "$probe_dir/in_flight"
  max_in_flight=$(read_num "$probe_dir/max_in_flight")
  if [ "$in_flight" -gt "$max_in_flight" ]; then
    printf '%s\n' "$in_flight" > "$probe_dir/max_in_flight"
  fi
  release_lock

  sleep 0.2

  acquire_lock
  in_flight=$(read_num "$probe_dir/in_flight")
  if [ "$in_flight" -gt 0 ]; then
    in_flight=$((in_flight - 1))
  fi
  printf '%s\n' "$in_flight" > "$probe_dir/in_flight"
  release_lock
fi

if [ -z "${{REAL_GIT-}}" ]; then
  printf '%s\n' "REAL_GIT is not set" >&2
  exit 2
fi

exec "$REAL_GIT" "$@"
"#,
            probe_dir = probe_dir.display(),
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        let mut perms = std::fs::metadata(&script).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&script, perms).unwrap();
    }
    bin_dir
}

fn find_sync_log(logs_root: &Path) -> PathBuf {
    std::fs::read_dir(logs_root)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.extension().is_some_and(|ext| ext == "log")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|name| name.ends_with("-sync.log"))
        })
        .expect("expected a sync log file")
}

fn find_single_bootstrap_record(state_home: &Path) -> PathBuf {
    let sessions_root = state_home.join("tmup/init-sessions");
    let records = std::fs::read_dir(&sessions_root)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", sessions_root.display()))
        .map(|entry| entry.unwrap().path().join("bootstrap.json"))
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    assert_eq!(records.len(), 1, "expected exactly one published bootstrap record");
    records.into_iter().next().unwrap()
}

fn schedule_bootstrap_record(
    root: &Path,
    config_path: &Path,
    config_mode: Option<&str>,
) -> PathBuf {
    let state_home = root.join("state");
    let schedule_log = root.join("schedule-tmux.log");
    let fake_tmux_dir = write_fake_tmux_with_log(root, &schedule_log);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());
    let mut command = Command::cargo_bin("tmup").unwrap();
    command
        .arg("init")
        .env("TMUP_CONFIG", config_path)
        .env("XDG_CONFIG_HOME", root.join("config"))
        .env("XDG_DATA_HOME", root.join("data"))
        .env("XDG_STATE_HOME", &state_home)
        .env("HOME", root)
        .env("PATH", path);
    if let Some(config_mode) = config_mode {
        command.env("TMUP_CONFIG_MODE", config_mode);
    }
    command.assert().success();
    find_single_bootstrap_record(&state_home)
}

fn write_ui_child_record(
    config_path: &Path,
    data_root: &Path,
    state_root: &Path,
) -> (PathBuf, PathBuf) {
    let session_dir = state_root.join("init-sessions/session-test-ui-child");
    std::fs::create_dir_all(&session_dir).unwrap();
    let record_path = session_dir.join("ui-child.json");
    let result_path = session_dir.join("child-result.json");
    let record = serde_json::json!({
        "version": 1,
        "continuation": {
            "role": "ui_child",
            "context": {
                "config_mode": "pure",
                "config_path": config_path,
                "tpm_identity": { "kind": "disabled" },
                "data_root": data_root,
                "state_root": state_root,
            },
            "source": "direct",
            "completion": {
                "kind": "popup",
                "result_name": "child-result.json",
            },
        },
    });
    std::fs::write(&record_path, serde_json::to_vec_pretty(&record).unwrap()).unwrap();
    (record_path, result_path)
}

fn resolve_real_git() -> String {
    let output = std::process::Command::new("sh").args(["-c", "command -v git"]).output().unwrap();
    assert!(output.status.success(), "failed to locate real git");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn assert_no_live_cursor_sequences(stderr: &str) {
    // Keep specific sequences for clearer failure messages, then use
    // "\x1b[" as a catch-all guard against any CSI escape leaking into
    // captured non-TTY stderr.
    for (sequence, description) in [
        ("\x1b[?25l", "hide-cursor"),
        ("\x1b[?25h", "show-cursor"),
        ("\x1b[2K", "clear-line"),
        ("\x1b[", "csi-escape"),
        ("\r", "carriage-return"),
    ] {
        assert!(
            !stderr.contains(sequence),
            "captured non-TTY stderr must stay append-only and not include {description} sequences, got:\n{stderr}"
        );
    }
}

#[cfg(unix)]
#[test]
fn sync_surfaces_lockfile_write_failure() {
    let dir = tempdir().unwrap();
    make_remote_repo(dir.path());
    let gitconfig = write_git_rewrite_config(dir.path());

    let config_dir = dir.path().join("alt-config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("custom.kdl");
    std::fs::write(&config_path, r#"plugin "https://example.com/test/plugin.git""#).unwrap();

    let original_mode = std::fs::metadata(&config_dir).unwrap().permissions().mode();
    let mut readonly = std::fs::metadata(&config_dir).unwrap().permissions();
    readonly.set_mode(0o555);
    std::fs::set_permissions(&config_dir, readonly).unwrap();

    let output = Command::cargo_bin("tmup")
        .unwrap()
        .arg("sync")
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", dir.path().join("xdg-config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", gitconfig)
        .env("HOME", dir.path())
        .output()
        .unwrap();

    let mut restored = std::fs::metadata(&config_dir).unwrap().permissions();
    restored.set_mode(original_mode);
    std::fs::set_permissions(&config_dir, restored).unwrap();

    assert!(!output.status.success(), "sync should fail when lockfile cannot be persisted");

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("failed to create")
            || stderr.contains("failed to rename")
            || stderr.contains("Permission denied"),
        "stderr should include the real lockfile write error, got:\n{stderr}"
    );
}

#[test]
fn sync_failure_log_includes_stage_and_context_metadata() {
    let dir = tempdir().unwrap();
    let bare = make_remote_repo(dir.path());
    let gitconfig = write_git_rewrite_config(dir.path());
    std::fs::remove_dir_all(&bare).unwrap();

    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "https://example.com/test/plugin.git""#).unwrap();

    let output = Command::cargo_bin("tmup")
        .unwrap()
        .arg("sync")
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", gitconfig)
        .env("HOME", dir.path())
        .output()
        .unwrap();

    assert!(!output.status.success(), "sync should fail when rewritten remote cannot be fetched");

    let logs_root = dir.path().join("state/tmup/logs");
    let log_path = find_sync_log(&logs_root);
    let log = std::fs::read_to_string(log_path).unwrap();

    assert!(log.contains("id=example.com/test/plugin"), "log: {log}");
    assert!(log.contains("stage=fetching"), "log: {log}");
    assert!(log.contains("clone_url: https://example.com/test/plugin.git"), "log: {log}");
    assert!(log.contains("tracking: default-branch"), "log: {log}");
}

#[cfg(unix)]
#[test]
fn sync_command_prepare_runs_with_real_parallelism_when_enabled() {
    let dir = tempdir().unwrap();
    make_remote_repo_named(dir.path(), "plugin-a");
    make_remote_repo_named(dir.path(), "plugin-b");
    let gitconfig = write_git_rewrite_config(dir.path());

    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(
        &config_path,
        r#"
options { concurrency 2 }
plugin "https://example.com/test/plugin-a.git"
plugin "https://example.com/test/plugin-b.git"
        "#,
    )
    .unwrap();

    let probe_dir = dir.path().join("git-probe");
    let wrapper_dir = write_git_fetch_probe_wrapper(dir.path(), &probe_dir);
    let path = format!("{}:{}", wrapper_dir.display(), std::env::var("PATH").unwrap_or_default());
    let real_git = resolve_real_git();

    let output = Command::cargo_bin("tmup")
        .unwrap()
        .arg("sync")
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", gitconfig)
        .env("HOME", dir.path())
        .env("PATH", path)
        .env("REAL_GIT", real_git)
        .output()
        .unwrap();

    assert!(output.status.success(), "sync should succeed with two healthy remotes");

    let max_in_flight = std::fs::read_to_string(probe_dir.join("max_in_flight"))
        .unwrap_or_else(|_| "0".to_string())
        .trim()
        .parse::<usize>()
        .unwrap_or(0);

    assert!(
        max_in_flight >= 2,
        "expected overlapping git fetch jobs during sync prepare, got max_in_flight={max_in_flight}"
    );
}

#[test]
fn init_parent_schedules_bootstrap_in_background() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo""#).unwrap();

    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_fake_tmux_with_log(dir.path(), &tmux_log);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let state_home = dir.path().join("state");
    let output = Command::cargo_bin("tmup")
        .unwrap()
        .arg("init")
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", &state_home)
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(output.status.success(), "init should succeed after scheduling bootstrap");

    let log = std::fs::read_to_string(&tmux_log).unwrap_or_default();
    assert!(
        log.contains("run-shell -b "),
        "expected init parent to schedule bootstrap with run-shell -b, got log:\n{log}"
    );
    let record_path = find_single_bootstrap_record(&state_home);
    assert!(record_path.is_absolute(), "resume record path must be absolute");
    assert!(
        log.contains(&format!("'init' '--resume' '{}'", record_path.display())),
        "expected scheduled bootstrap command to contain only the resume record, got log:\n{log}"
    );
    for legacy_argument in [
        "--bootstrap",
        "--config-path",
        "--tpm-config-path",
        "--no-tpm-config",
        "--data-root",
        "--state-root",
    ] {
        assert!(
            !log.contains(legacy_argument),
            "deferred bootstrap must not use legacy argument {legacy_argument}, got log:\n{log}"
        );
    }
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&record_path).unwrap()).unwrap();
    assert_eq!(record["version"], 1);
    assert_eq!(record["continuation"]["role"], "bootstrap");
    assert_eq!(record["continuation"]["context"]["config_path"], config_path.to_str().unwrap());
    assert!(
        !log.contains("display-popup "),
        "init parent should not open popup synchronously anymore, got log:\n{log}"
    );
    assert!(
        !log.contains("split-window "),
        "init parent should not open split synchronously anymore, got log:\n{log}"
    );
}

#[test]
fn init_resume_uses_recorded_sources_and_cleans_its_session() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo""#).unwrap();
    let state_home = dir.path().join("state");
    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_fake_tmux_with_log(dir.path(), &tmux_log);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("init")
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", &state_home)
        .env("PATH", &path)
        .assert()
        .success();
    let record_path = find_single_bootstrap_record(&state_home);
    let session_dir = record_path.parent().unwrap().to_path_buf();

    std::fs::write(&config_path, "").unwrap();
    let bad_config = dir.path().join("bad.kdl");
    std::fs::write(&bad_config, "plugin {\n").unwrap();

    Command::cargo_bin("tmup")
        .unwrap()
        .args(["init", "--resume", record_path.to_str().unwrap()])
        .env("TMUP_CONFIG_MODE", "not-a-mode")
        .env("TMUP_CONFIG", &bad_config)
        .env("XDG_CONFIG_HOME", dir.path().join("unused-config"))
        .env("XDG_DATA_HOME", dir.path().join("unused-data"))
        .env("XDG_STATE_HOME", dir.path().join("unused-state"))
        .env("HOME", dir.path().join("unused-home"))
        .env("PATH", path)
        .assert()
        .success()
        .stderr(predicate::str::contains("TMUP_CONFIG_MODE").not())
        .stderr(predicate::str::contains("failed to parse KDL").not());

    assert!(!session_dir.exists(), "a normally completed resume must clean its own session");
}

#[test]
fn init_parent_bootstrap_uses_resolved_tmup_config_path() {
    let dir = tempdir().unwrap();
    let default_config_dir = dir.path().join("config/tmux");
    let override_dir = dir.path().join("alt-config");
    std::fs::create_dir_all(&default_config_dir).unwrap();
    std::fs::create_dir_all(&override_dir).unwrap();
    let override_config = override_dir.join("custom.kdl");
    std::fs::write(default_config_dir.join("tmup.kdl"), r#"plugin "user/default""#).unwrap();
    std::fs::write(&override_config, r#"plugin "user/override""#).unwrap();

    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_fake_tmux_with_log(dir.path(), &tmux_log);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let state_home = dir.path().join("state");
    let output = Command::cargo_bin("tmup")
        .unwrap()
        .arg("init")
        .env("TMUP_CONFIG", &override_config)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", &state_home)
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(output.status.success(), "init should succeed after scheduling bootstrap");

    let record_path = find_single_bootstrap_record(&state_home);
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(record_path).unwrap()).unwrap();
    assert_eq!(record["continuation"]["context"]["config_path"], override_config.to_str().unwrap());
}

#[test]
fn init_parent_bootstrap_uses_resolved_tpm_config_path_in_mixed_mode() {
    let dir = tempdir().unwrap();
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let tmup_config = config_dir.join("tmup.kdl");
    let tpm_config = config_dir.join("tmux.conf");
    std::fs::write(&tmup_config, "").unwrap();
    std::fs::write(&tpm_config, "set -g @plugin 'tmux-plugins/tmux-sensible'\n").unwrap();

    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_fake_tmux_with_log(dir.path(), &tmux_log);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let state_home = dir.path().join("state");
    let output = Command::cargo_bin("tmup")
        .unwrap()
        .arg("init")
        .env("TMUP_CONFIG_MODE", "mixed")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", &state_home)
        .env("HOME", dir.path())
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(output.status.success(), "init should succeed after scheduling bootstrap");

    let record_path = find_single_bootstrap_record(&state_home);
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(record_path).unwrap()).unwrap();
    assert_eq!(record["continuation"]["context"]["config_mode"], "mixed");
    assert_eq!(record["continuation"]["context"]["tpm_identity"]["kind"], "path");
    assert_eq!(
        record["continuation"]["context"]["tpm_identity"]["path"],
        tpm_config.to_str().unwrap()
    );
}

#[test]
fn init_parent_bootstrap_marks_absent_tpm_config_as_resolved_none() {
    let dir = tempdir().unwrap();
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let tmup_config = config_dir.join("tmup.kdl");
    std::fs::write(&tmup_config, r#"plugin "user/repo""#).unwrap();

    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_fake_tmux_with_log(dir.path(), &tmux_log);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let state_home = dir.path().join("state");
    let output = Command::cargo_bin("tmup")
        .unwrap()
        .arg("init")
        .env("TMUP_CONFIG_MODE", "mixed")
        .env("XDG_CONFIG_HOME", &config_home)
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", &state_home)
        .env("HOME", dir.path())
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(output.status.success(), "init should succeed after scheduling bootstrap");

    let record_path = find_single_bootstrap_record(&state_home);
    let record: serde_json::Value =
        serde_json::from_slice(&std::fs::read(record_path).unwrap()).unwrap();
    assert_eq!(record["continuation"]["context"]["config_mode"], "mixed");
    assert_eq!(record["continuation"]["context"]["tpm_identity"]["kind"], "absent");
    assert!(record["continuation"]["context"]["tpm_identity"].get("path").is_none());
}

#[test]
fn init_bootstrap_mixed_uses_tpm_plugins_when_tmup_kdl_is_missing() {
    let dir = tempdir().unwrap();
    let _bare = make_remote_repo(dir.path());
    let gitconfig = write_git_rewrite_config(dir.path());
    let config_dir = dir.path().join("config/tmux");
    let tmup_config = config_dir.join("tmup.kdl");
    let tpm_config = config_dir.join("tmux.conf");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(&tpm_config, "set -g @plugin 'https://example.com/test/plugin.git'\n").unwrap();
    let state_home = dir.path().join("state");
    let schedule_log = dir.path().join("schedule-tmux.log");
    let schedule_tmux_dir = write_fake_tmux_with_log(dir.path(), &schedule_log);
    let schedule_path =
        format!("{}:{}", schedule_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    Command::cargo_bin("tmup")
        .unwrap()
        .arg("init")
        .env("TMUP_CONFIG_MODE", "mixed")
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", &state_home)
        .env("HOME", dir.path())
        .env("PATH", schedule_path)
        .assert()
        .success();
    let record_path = find_single_bootstrap_record(&state_home);

    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_fake_tmux_versioned_with_log(dir.path(), &tmux_log, "tmux 1.9", 1, 1);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());
    Command::cargo_bin("tmup")
        .unwrap()
        .args(["init", "--resume", record_path.to_str().unwrap()])
        .env("TMUP_CONFIG_MODE", "invalid-after-publication")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", &gitconfig)
        .env("HOME", dir.path())
        .env("PATH", path)
        .assert()
        .success();

    assert!(tmup_config.exists(), "bootstrap should scaffold a missing tmup.kdl");

    let lock = std::fs::read_to_string(config_dir.join("tmup.lock")).unwrap();
    assert!(lock.contains(r#""example.com/test/plugin""#), "{lock}");
}

#[test]
fn init_bootstrap_no_tpm_config_disables_rediscovery() {
    let dir = tempdir().unwrap();
    let config_home = dir.path().join("config");
    let config_dir = config_home.join("tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let tmup_config = config_dir.join("tmup.kdl");
    let tpm_config = config_dir.join("tmux.conf");
    std::fs::write(&tmup_config, r#"plugin "user/repo""#).unwrap();
    let record_path = schedule_bootstrap_record(dir.path(), &tmup_config, Some("mixed"));

    std::fs::write(&tmup_config, "").unwrap();
    std::fs::write(&tpm_config, "set -g @plugin 'https://example.com/test/plugin.git'\n").unwrap();
    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_fake_tmux_versioned_with_log(dir.path(), &tmux_log, "tmux 1.9", 1, 1);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    Command::cargo_bin("tmup")
        .unwrap()
        .args(["init", "--resume", record_path.to_str().unwrap()])
        .env("TMUP_CONFIG_MODE", "invalid-after-publication")
        .env("PATH", path)
        .assert()
        .success();

    let lock_path = config_dir.join("tmup.lock");
    if lock_path.exists() {
        let lock = std::fs::read_to_string(&lock_path).unwrap();
        assert!(
            !lock.contains(r#""example.com/test/plugin""#),
            "bootstrap should not rediscover TPM plugins when --no-tpm-config is set: {lock}"
        );
    }
}

#[test]
fn init_parent_uses_immediate_ui_when_attached_client_is_ready() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo""#).unwrap();

    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_fake_tmux_bootstrap_with_log(dir.path(), &tmux_log, 0, 1);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let output = Command::cargo_bin("tmup")
        .unwrap()
        .arg("init")
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "attached init should synchronously observe popup child failure"
    );

    let log = std::fs::read_to_string(&tmux_log).unwrap_or_default();
    assert!(
        log.contains("display-popup "),
        "attached init should use popup immediately when a client target is already available, got log:\n{log}"
    );
    assert!(
        !log.contains("run-shell -b "),
        "attached init should not defer to background bootstrap, got log:\n{log}"
    );
}

#[test]
fn init_parent_missing_popup_result_includes_popup_context() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo""#).unwrap();

    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_fake_tmux_bootstrap_with_log(dir.path(), &tmux_log, 0, 1);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let output = Command::cargo_bin("tmup")
        .unwrap()
        .arg("init")
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "attached init should fail when popup succeeds without producing a result file"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("reading popup init result"),
        "stderr should include popup result context, got:\n{stderr}"
    );
}

#[test]
fn init_parent_tmux_3_2_popup_omits_title_flag() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo""#).unwrap();

    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_fake_tmux_versioned_with_log(dir.path(), &tmux_log, "tmux 3.2", 0, 1);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let output = Command::cargo_bin("tmup")
        .unwrap()
        .arg("init")
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "attached init should still fail when popup leaves no result file"
    );

    let log = std::fs::read_to_string(&tmux_log).unwrap_or_default();
    let popup_header = log
        .lines()
        .find(|line| line.starts_with("display-popup "))
        .unwrap_or_else(|| panic!("expected display-popup call, got:\n{log}"));
    assert!(
        popup_header.contains("display-popup -E -w 80% -h 80% -c /dev/pts/99 -- "),
        "tmux 3.2 popup should still be used, got:\n{popup_header}"
    );
    assert!(
        !popup_header.contains("-T tmup init"),
        "tmux 3.2 popup should omit the title flag, got:\n{popup_header}"
    );
}

#[test]
fn init_parent_uses_split_only_when_tmux_is_2_0() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo""#).unwrap();

    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_fake_tmux_versioned_with_log(dir.path(), &tmux_log, "tmux 2.0", 1, 0);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let output = Command::cargo_bin("tmup")
        .unwrap()
        .arg("init")
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "split-only init should still fail when split child leaves no result file"
    );

    let log = std::fs::read_to_string(&tmux_log).unwrap_or_default();
    assert!(!log.contains("display-popup "), "tmux 2.0 should not attempt popup, got log:\n{log}");
    assert!(log.contains("split-window "), "tmux 2.0 should use split-window, got log:\n{log}");
}

#[test]
fn init_parent_uses_inline_when_tmux_is_1_9() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo""#).unwrap();

    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_fake_tmux_versioned_with_log(dir.path(), &tmux_log, "tmux 1.9", 1, 1);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let _output = Command::cargo_bin("tmup")
        .unwrap()
        .arg("init")
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("PATH", path)
        .output()
        .unwrap();

    let log = std::fs::read_to_string(&tmux_log).unwrap_or_default();
    assert!(
        !log.contains("display-popup "),
        "tmux 1.9 inline mode should not attempt popup, got log:\n{log}"
    );
    assert!(
        !log.contains("split-window "),
        "tmux 1.9 inline mode should not attempt split-window, got log:\n{log}"
    );
}

#[test]
fn init_bootstrap_uses_split_when_tmux_is_2_0() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo""#).unwrap();
    let record_path = schedule_bootstrap_record(dir.path(), &config_path, None);

    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_fake_tmux_versioned_with_log(dir.path(), &tmux_log, "tmux 2.0", 1, 0);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let output = Command::cargo_bin("tmup")
        .unwrap()
        .args(["init", "--resume", record_path.to_str().unwrap()])
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "bootstrap should fail when split child leaves no result file"
    );

    let log = std::fs::read_to_string(&tmux_log).unwrap_or_default();
    let split_index = log.find("split-window -v -l 50% -t %42 -- ");
    let inline_option_index =
        log.find("tmux set-option -p remain-on-exit failed >/dev/null 2>&1 || true");
    assert!(
        split_index.is_some(),
        "expected tmux 2.0 bootstrap to use split-window, got log:\n{log}"
    );
    assert!(
        !log.contains("display-popup "),
        "tmux 2.0 bootstrap should not attempt popup, got log:\n{log}"
    );
    assert!(
        inline_option_index.is_some(),
        "split wrapper should set remain-on-exit before launching the child, got log:\n{log}"
    );
    let split_index = split_index.unwrap();
    let inline_option_index = inline_option_index.unwrap();
    let child_index = log[split_index..]
        .find(" 'init' '--resume' '")
        .map(|offset| split_index + offset)
        .expect("expected child command in split-window tmux log");
    assert!(
        split_index < inline_option_index && inline_option_index < child_index,
        "split wrapper should set remain-on-exit before launching the child, got log:\n{log}"
    );
    for legacy_transport in ["--ui-child", "--wait-channel", "--config-path", "exit_code"] {
        assert!(
            !log[split_index..].contains(legacy_transport),
            "split wrapper must not contain legacy transport {legacy_transport}, got log:\n{log}"
        );
    }
    assert!(
        log.lines().any(|line| line.starts_with("wait-for ")),
        "split path should still wait for the child signal, got log:\n{log}"
    );
    let key_wait_index = log.find("dd bs=1 count=1").unwrap_or_else(|| {
        panic!("split wrapper should wait for keypress before closing, got log:\n{log}")
    });
    let wait_index = log
        .rfind("\nwait-for ")
        .map(|offset| offset + 1)
        .unwrap_or_else(|| panic!("split path should signal completion, got log:\n{log}"));
    assert!(
        key_wait_index < wait_index,
        "split wrapper should wait for keypress before the parent wait-for returns, got log:\n{log}"
    );
}

/// Regression test for the popup path: verifies that
/// 1. the wrapper is passed as a single shell-command (no extra `sh -c`),
/// 2. `wait-for` is NOT called after `display-popup` (it blocks until close).
#[test]
fn init_bootstrap_popup_path_targets_explicit_client_and_skips_wait_for() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo""#).unwrap();
    let record_path = schedule_bootstrap_record(dir.path(), &config_path, None);

    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_fake_tmux_bootstrap_with_log(dir.path(), &tmux_log, 0, 1);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let output = Command::cargo_bin("tmup")
        .unwrap()
        .args(["init", "--resume", record_path.to_str().unwrap()])
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("PATH", path)
        .output()
        .unwrap();

    // Bootstrap fails because the fake tmux doesn't actually run the child (no result file).
    assert!(!output.status.success());

    let log = std::fs::read_to_string(&tmux_log).unwrap_or_default();

    // display-popup was called (the wrapper is multi-line, so the entry spans
    // several lines in the log — match the first line that starts with
    // "display-popup" and use the full log for content checks).
    let popup_start = log
        .find("display-popup ")
        .unwrap_or_else(|| panic!("expected display-popup call, got:\n{log}"));
    let popup_header =
        &log[popup_start..log[popup_start..].find('\n').map_or(log.len(), |i| popup_start + i)];

    // Wrapper is passed directly — no extra `sh -c` between `--` and the wrapper body.
    assert!(
        popup_header.contains(
            "display-popup -E -w 80% -h 80% -c /dev/pts/99 -T  tmup init (press #[bold,fg=red]q#[default] to exit)  -- "
        ),
        "display-popup should target the probed client explicitly with the new argument shape, got:\n{popup_header}"
    );
    assert!(
        !popup_header.contains(" -- sh -c "),
        "display-popup should NOT wrap command in sh -c (tmux does this internally), got:\n{popup_header}"
    );

    // The wrapper (multi-line) must contain only the record-based child command.
    assert!(
        log[popup_start..].contains(" 'init' '--resume' '"),
        "display-popup wrapper should resume the UI child from its record, got log:\n{log}"
    );
    for legacy_transport in ["--ui-child", "--wait-channel", "--config-path", "exit_code"] {
        assert!(
            !log[popup_start..].contains(legacy_transport),
            "display-popup wrapper must not contain legacy transport {legacy_transport}, got log:\n{log}"
        );
    }
    let child_index =
        log[popup_start..].find(" 'init' '--resume' '").map(|offset| popup_start + offset).unwrap();
    let key_wait_index = log[popup_start..]
        .find("dd bs=1 count=1")
        .map(|offset| popup_start + offset)
        .unwrap_or_else(|| {
            panic!("popup wrapper should wait for keypress before exit, got log:\n{log}")
        });
    assert!(
        child_index < key_wait_index,
        "popup wrapper should run the child before waiting for keypress, got log:\n{log}"
    );
    assert!(
        log[popup_start..].contains("exit 0"),
        "popup wrapper should exit cleanly after q closes the UI, got log:\n{log}"
    );

    // wait-for must NOT be called on the popup path (display-popup blocks until close).
    assert!(
        !log.lines().any(|line| line.starts_with("wait-for ")),
        "popup path should not call wait-for (display-popup already blocks), got log:\n{log}"
    );
}

#[test]
fn production_popup_blocks_until_the_real_child_publishes_its_result() {
    let dir = tempdir().unwrap();
    make_remote_repo(dir.path());
    let gitconfig = write_git_rewrite_config(dir.path());
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(
        &config_path,
        "options { auto-install #true }\nplugin \"https://example.com/test/plugin.git\"\n",
    )
    .unwrap();
    let record_path = schedule_bootstrap_record(dir.path(), &config_path, None);
    let session_dir = record_path.parent().unwrap().to_path_buf();
    let state_home = dir.path().join("state");

    let tmux_log = dir.path().join("executing-popup.log");
    let fake_tmux_dir = write_fake_tmux_executing_popup(dir.path(), &tmux_log);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let output = Command::cargo_bin("tmup")
        .unwrap()
        .args(["init", "--resume", record_path.to_str().unwrap()])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", gitconfig)
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr:\n{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        dir.path().join("data/tmup/plugins/example.com/test/plugin/init.tmux").is_file(),
        "the popup fake must execute the real hosted child"
    );
    assert!(
        !session_dir.exists(),
        "the popup parent may clean only after consuming the published child result"
    );
    let log = std::fs::read_to_string(&tmux_log).unwrap();
    let entered = log.find("event:popup-entered").unwrap();
    let returned = log.find("event:popup-returned").unwrap();
    assert!(entered < returned, "popup execution events were out of order:\n{log}");
    assert!(
        !log.lines().any(|line| line.starts_with("wait-for ")),
        "popup completion must be provided by the blocking display-popup call:\n{log}"
    );
    assert!(
        state_home.join("tmup/init-sessions").exists(),
        "the production adapter should use the isolated Init Session root"
    );
}

#[test]
fn production_split_waits_for_child_signal_before_consuming_the_result() {
    let dir = tempdir().unwrap();
    make_remote_repo(dir.path());
    let gitconfig = write_git_rewrite_config(dir.path());
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(
        &config_path,
        "options { auto-install #true }\nplugin \"https://example.com/test/plugin.git\"\n",
    )
    .unwrap();
    let record_path = schedule_bootstrap_record(dir.path(), &config_path, None);
    let session_dir = record_path.parent().unwrap().to_path_buf();
    let state_home = dir.path().join("state");

    let tmux_log = dir.path().join("executing-split.log");
    let fake_tmux_dir = write_fake_tmux_executing_split(dir.path(), &tmux_log, &state_home);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let output = Command::cargo_bin("tmup")
        .unwrap()
        .args(["init", "--resume", record_path.to_str().unwrap()])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", gitconfig)
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(output.status.success(), "stderr:\n{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        !session_dir.exists(),
        "the split parent may clean only after signal and result consumption"
    );
    let log = std::fs::read_to_string(&tmux_log).unwrap();
    let split_returned = log.find("event:split-returned").unwrap();
    let signal = log.find("event:signal-result-true").unwrap_or_else(|| {
        panic!("the child must publish its result before signaling the split parent:\n{log}")
    });
    let wait_returned = log.find("event:wait-returned").unwrap();
    assert!(
        split_returned < signal && signal < wait_returned,
        "split launch, child publication signal, and parent wait must be ordered:\n{log}"
    );
}

#[test]
fn production_split_preserves_session_when_wait_completion_is_unknown() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo""#).unwrap();
    let record_path = schedule_bootstrap_record(dir.path(), &config_path, None);
    let session_dir = record_path.parent().unwrap().to_path_buf();

    let tmux_log = dir.path().join("split-wait-failure.log");
    let fake_tmux_dir = write_fake_tmux_split_wait_failure(dir.path(), &tmux_log);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let output = Command::cargo_bin("tmup")
        .unwrap()
        .args(["init", "--resume", record_path.to_str().unwrap()])
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(!output.status.success(), "a failed split wait must fail the parent command");
    assert!(
        session_dir.exists(),
        "a successful split launch with unknown terminal completion must preserve its Init Session"
    );
    assert!(
        session_dir.join("ui-child.json").is_file(),
        "the preserved session must retain the unclaimed child continuation"
    );
    let log = std::fs::read_to_string(&tmux_log).unwrap();
    assert!(log.contains("split-window "), "the child must have been launched:\n{log}");
    assert!(
        log.lines().any(|line| line.starts_with("wait-for ")),
        "the failure must occur while waiting for terminal completion:\n{log}"
    );
}

#[test]
fn init_bootstrap_retries_probe_until_target_is_ready() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo""#).unwrap();
    let record_path = schedule_bootstrap_record(dir.path(), &config_path, None);

    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_fake_tmux_retry_probe_with_log(dir.path(), &tmux_log);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let output = Command::cargo_bin("tmup")
        .unwrap()
        .args(["init", "--resume", record_path.to_str().unwrap()])
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "bootstrap should still fail once popup child leaves no result file"
    );

    let log = std::fs::read_to_string(&tmux_log).unwrap_or_default();
    let probe_calls = log.lines().filter(|line| line.starts_with("display-message -p ")).count();
    assert!(
        probe_calls >= 3,
        "bootstrap should retry target probing after a transient display-message failure, got log:\n{log}"
    );
    assert!(
        log.contains("display-popup "),
        "bootstrap should eventually reach popup after retrying the probe, got log:\n{log}"
    );
}

#[test]
fn init_bootstrap_keeps_probing_long_enough_for_late_target() {
    let dir = tempdir().unwrap();
    let config_dir = dir.path().join("config/tmux");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(&config_path, r#"plugin "user/repo""#).unwrap();
    let record_path = schedule_bootstrap_record(dir.path(), &config_path, None);

    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir =
        write_fake_tmux_retry_probe_after_delay_with_log(dir.path(), &tmux_log, 1000);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let output = Command::cargo_bin("tmup")
        .unwrap()
        .args(["init", "--resume", record_path.to_str().unwrap()])
        .env("TMUP_CONFIG", &config_path)
        .env("XDG_CONFIG_HOME", dir.path().join("config"))
        .env("XDG_DATA_HOME", dir.path().join("data"))
        .env("XDG_STATE_HOME", dir.path().join("state"))
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "bootstrap should still fail once popup child leaves no result file"
    );

    let log = std::fs::read_to_string(&tmux_log).unwrap_or_default();
    assert!(
        log.contains("display-popup "),
        "bootstrap should still reach popup when the target appears near the end of the retry window, got log:\n{log}"
    );
}

#[test]
fn init_loads_tmux_after_sync_plugin_failures() {
    let dir = tempdir().unwrap();
    let _bare = make_remote_repo(dir.path());
    let gitconfig = write_git_rewrite_config(dir.path());

    let config_dir = dir.path().join("config");
    std::fs::create_dir_all(&config_dir).unwrap();
    let config_path = config_dir.join("tmup.kdl");
    std::fs::write(
        &config_path,
        r#"
options { auto-install #true }
plugin "https://example.com/test/plugin.git" build="exit 1"
        "#,
    )
    .unwrap();

    let data_root = dir.path().join("data");
    let state_root = dir.path().join("state");
    let xdg_config_home = dir.path().join("xdg-config");
    std::fs::create_dir_all(&data_root).unwrap();
    std::fs::create_dir_all(&state_root).unwrap();
    let (record_path, result_path) = write_ui_child_record(&config_path, &data_root, &state_root);

    let tmux_log = dir.path().join("tmux.log");
    let fake_tmux_dir = write_fake_tmux_with_log(dir.path(), &tmux_log);
    let path = format!("{}:{}", fake_tmux_dir.display(), std::env::var("PATH").unwrap_or_default());

    let output = Command::cargo_bin("tmup")
        .unwrap()
        .args(["init", "--resume", record_path.to_str().unwrap()])
        .env("PATH", path)
        .env("TMUP_CONFIG_MODE", "invalid-ambient-mode")
        .env("TMUP_CONFIG", dir.path().join("ignored.kdl"))
        .env("XDG_CONFIG_HOME", &xdg_config_home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", gitconfig)
        .env("HOME", dir.path())
        .output()
        .unwrap();

    assert!(
        !output.status.success(),
        "ui child should still return non-zero after plugin-level sync failures"
    );

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Fetching"), "stderr:\n{stderr}");
    assert!(
        stderr.contains("Loading tmux applying load plan"),
        "tmux loading stage should be visible in append-only mode when init continues after plugin failures, got:\n{stderr}"
    );
    assert_no_live_cursor_sequences(&stderr);

    let result: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&result_path).unwrap()).unwrap();
    assert_eq!(result["version"], 1);
    assert_eq!(result["result"]["status"], "completed_with_plugin_failures");
    assert_eq!(result["result"]["failures"].as_array().unwrap().len(), 1);

    let log = std::fs::read_to_string(&tmux_log).unwrap_or_default();
    let has_plugin_manager_env = log
        .lines()
        .any(|line| line.contains("set-environment") && line.contains("TMUX_PLUGIN_MANAGER_PATH"));
    let has_run_shell = log.lines().any(|line| line.split_whitespace().next() == Some("run-shell"));
    assert!(
        has_plugin_manager_env || has_run_shell,
        "expected loader activity (`set-environment ... TMUX_PLUGIN_MANAGER_PATH` or `run-shell`) \
after sync plugin failure, got log:\n{log}"
    );
}
