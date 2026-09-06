#![cfg(unix)]

use std::fs;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use tempfile::TempDir;

// Keep fixture writes out of other tests' fork/exec windows: inherited writable
// descriptors can transiently make a just-copied executable fail with ETXTBSY.
static FIXTURE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

struct Fixture {
    _root: TempDir,
    executable: PathBuf,
    bin: PathBuf,
    tmp: PathBuf,
    helper: PathBuf,
    log: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let install = root.path().join("program directory");
        let bin = root.path().join("tools");
        let tmp = root.path().join("temporary");
        for path in [&install, &bin, &tmp] {
            fs::create_dir(path).unwrap();
        }
        let executable = install.join("tmup-current");
        fs::copy(assert_cmd::cargo::cargo_bin!("tmup"), &executable).unwrap();
        let helper = root.path().join("helper.sh");
        let log = root.path().join("calls");
        fs::write(&helper, r#"#!/bin/sh
set -eu
printf 'helper %s\n' "$*" >> "$CALL_LOG"
if [ "$1" = --resolve-version ]; then
    case "${QUERY_MODE:-}" in
      fail) echo 'query failed' >&2; exit 1 ;;
      malformed) printf '0.4.0\nnoise\n'; exit 0 ;;
      block) touch "$BLOCK_MARKER"; sleep 1 ;;
    esac
    if [ "${2:-}" = --version ]; then printf '%s\n' "${3#v}"; else printf '%s\n' "${SELECTED_VERSION:-0.4.0}"; fi
    exit 0
fi
version=$2
shift 2
while [ "$#" -gt 0 ]; do
    case "$1" in --target) shift 2 ;; --to) dest=$2; shift 2 ;; --quiet) shift ;; *) exit 9 ;; esac
done
case "${PREP_MODE:-}" in
  fail) echo 'missing release asset' >&2; exit 1 ;;
  mutate) printf 'external replacement' > "$dest/external"; mv "$dest/external" "$CHANGE_DEST" ;;
  mismatch) version=9.9.9 ;;
  fifo) mkfifo "$dest/tmup"; exit 0 ;;
  link) ln -s "$CANDIDATE_SOURCE" "$dest/tmup"; exit 0 ;;
esac
printf '#!/bin/sh\nprintf "tmup %s\\n"\n' "$version" > "$dest/tmup"
chmod 755 "$dest/tmup"
"#).unwrap();
        write_executable(
            &bin.join("curl"),
            r#"#!/bin/sh
set -eu
printf 'download %s\n' "$*" >> "$CALL_LOG"
out=
for arg do
  if [ "${previous:-}" = --output ]; then out=$arg; fi
  previous=$arg
  url=$arg
done
case "${DOWNLOAD_MODE:-}" in
  fail) echo 'certificate failed' >&2; exit 60 ;;
esac
case "$url" in
  *raw.githubusercontent.com*) cp "$HELPER_FILE" "$out" ;;
  *.tar.xz) cp "$ARCHIVE_FILE" "$out" ;;
  *) echo "unexpected download: $url" >&2; exit 22 ;;
esac
printf 200
"#,
        );
        Self { _root: root, executable, bin, tmp, helper, log }
    }
    fn command(&self) -> Command {
        let mut command = Command::new(&self.executable);
        command
            .arg("upgrade")
            .env(
                "PATH",
                format!("{}:{}", self.bin.display(), std::env::var("PATH").unwrap_or_default()),
            )
            .env("TMPDIR", &self.tmp)
            .env("HELPER_FILE", &self.helper)
            .env("CALL_LOG", &self.log)
            .env("TMUP_CONFIG_MODE", "deliberately invalid")
            .env("TMUP_CONFIG", self._root.path().join("missing.kdl"));
        command
    }
    fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }
    fn assert_clean(&self) {
        assert_eq!(fs::read_dir(&self.tmp).unwrap().count(), 0);
        for entry in fs::read_dir(self.executable.parent().unwrap()).unwrap() {
            let name = entry.unwrap().file_name();
            assert!(
                !name.to_string_lossy().starts_with(".tmup-upgrade-"),
                "candidate remains: {name:?}"
            );
        }
    }
}
fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn rejects_unmarked_build_and_conflicting_flags_before_download() {
    let _guard = FIXTURE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new();
    let output = fixture.run(&[]);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("--force"));
    assert!(!fixture.log.exists());
    assert!(!fixture.run(&["--force", "--pre", "--version", "0.4.0"]).status.success());
    assert!(!fixture.run(&["--force", "--version", "../../bad"]).status.success());
    assert!(!fixture.log.exists());
}

#[test]
fn forced_upgrade_reuses_snapshot_and_ignores_plugin_configuration() {
    let _guard = FIXTURE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new();
    let output = fixture.run(&["--force"]);
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains(fixture.executable.to_str().unwrap()));
    assert_eq!(
        Command::new(&fixture.executable).arg("--version").output().unwrap().stdout,
        b"tmup 0.4.0\n"
    );
    let log = fs::read_to_string(&fixture.log).unwrap();
    assert_eq!(log.lines().filter(|line| line.starts_with("download")).count(), 1);
    assert!(log.contains("helper --resolve-version"));
    assert!(log.contains("helper --version 0.4.0 --target"));
    assert!(log.contains("--quiet"));
    assert!(!fixture.executable.parent().unwrap().join("tmup").exists());
    fixture.assert_clean();
}

#[test]
fn force_reinstalls_equal_but_does_not_implicitly_downgrade() {
    let _guard = FIXTURE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new();
    let original = fs::read(&fixture.executable).unwrap();
    let output =
        fixture.command().arg("--force").env("SELECTED_VERSION", "0.1.0").output().unwrap();
    success(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("--version"));
    assert_eq!(fs::read(&fixture.executable).unwrap(), original);
    assert!(!fs::read_to_string(&fixture.log).unwrap().contains("--quiet"));
    success(&fixture.run(&["--force", "--version", env!("CARGO_PKG_VERSION")]));
    assert_ne!(fs::read(&fixture.executable).unwrap(), original);
    fixture.assert_clean();
}

#[test]
fn explicit_downgrade_and_prerelease_selection() {
    let _guard = FIXTURE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new();
    success(&fixture.run(&["--force", "--version", "v0.1.0"]));
    assert!(fs::read_to_string(&fixture.executable).unwrap().contains("0.1.0"));
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .args(["--force", "--pre"])
        .env("SELECTED_VERSION", "0.4.0-rc.1")
        .output()
        .unwrap();
    success(&output);
    assert!(fs::read_to_string(fixture.log).unwrap().contains("--resolve-version --pre"));
}

#[test]
fn failures_preserve_installed_bytes_and_remove_all_temporary_files() {
    let _guard = FIXTURE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    for (key, value) in [
        ("DOWNLOAD_MODE", "fail"),
        ("QUERY_MODE", "fail"),
        ("QUERY_MODE", "malformed"),
        ("PREP_MODE", "fail"),
        ("PREP_MODE", "mismatch"),
        ("PREP_MODE", "link"),
        ("PREP_MODE", "fifo"),
    ] {
        let fixture = Fixture::new();
        let before = fs::read(&fixture.executable).unwrap();
        let output = fixture
            .command()
            .arg("--force")
            .env(key, value)
            .env("CANDIDATE_SOURCE", &fixture.executable)
            .output()
            .unwrap();
        assert!(!output.status.success(), "{key}={value}");
        assert_eq!(fs::read(&fixture.executable).unwrap(), before, "{key}={value}");
        fixture.assert_clean();
    }
}

#[test]
fn preserves_relative_chained_links_and_symlinked_parent() {
    let _guard = FIXTURE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new();
    let root = fixture._root.path();
    symlink(fixture.executable.parent().unwrap(), root.join("parent-link")).unwrap();
    symlink("parent-link/tmup-current", root.join("relative-link")).unwrap();
    symlink(root.join("relative-link"), root.join("absolute-link")).unwrap();
    let command = fixture.command();
    // Rebuild command to execute through the chain while preserving its isolated environment.
    let mut linked = Command::new(root.join("absolute-link"));
    linked
        .args(command.get_args())
        .envs(command.get_envs().filter_map(|(key, value)| value.map(|value| (key, value))))
        .arg("--force");
    success(&linked.output().unwrap());
    assert_eq!(
        fs::read_link(root.join("relative-link")).unwrap(),
        PathBuf::from("parent-link/tmup-current")
    );
    assert_eq!(fs::read_link(root.join("absolute-link")).unwrap(), root.join("relative-link"));
    assert!(!fixture.executable.parent().unwrap().join("tmup").exists());
    fixture.assert_clean();
}

#[test]
fn concurrent_aliases_contend_for_real_destination_lock() {
    let _guard = FIXTURE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new();
    let marker = fixture._root.path().join("blocked");
    let mut first = fixture
        .command()
        .arg("--force")
        .env("QUERY_MODE", "block")
        .env("BLOCK_MARKER", &marker)
        .spawn()
        .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !marker.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(marker.exists());
    let alias = fixture._root.path().join("alias");
    symlink(&fixture.executable, &alias).unwrap();
    let output = Command::new(alias).args(["upgrade", "--force"]).output().unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("another tmup upgrade"));
    // Allow the first helper to finish and verify normal cleanup/release.
    assert!(first.wait().unwrap().success());
    fixture.assert_clean();
}

#[test]
fn real_installer_supports_system_temp_and_noexec_mount() {
    let _guard = FIXTURE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new();
    fs::copy(Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh"), &fixture.helper).unwrap();
    let version = "0.4.0";
    let target = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "x86_64-unknown-linux-musl",
        ("linux", "aarch64") => "aarch64-unknown-linux-musl",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("macos", "aarch64") => "aarch64-apple-darwin",
        _ => return,
    };
    let archive_root = fixture._root.path().join(format!("tmup-v{version}-{target}"));
    fs::create_dir(&archive_root).unwrap();
    write_executable(&archive_root.join("tmup"), "#!/bin/sh\nprintf 'tmup 0.4.0\\n'\n");
    let archive = fixture._root.path().join("release.tar.xz");
    success(
        &Command::new("tar")
            .arg("cJf")
            .arg(&archive)
            .arg("-C")
            .arg(fixture._root.path())
            .arg(archive_root.file_name().unwrap())
            .output()
            .unwrap(),
    );
    let mut command = fixture.command();
    command.args(["--force", "--version", version]).env("ARCHIVE_FILE", archive);
    let noexec_workspace =
        std::env::var_os("TMUP_TEST_NOEXEC_DIR").map(|path| tempfile::tempdir_in(path).unwrap());
    if let Some(workspace) = &noexec_workspace {
        command.env("TMPDIR", workspace.path());
    }
    success(&command.output().unwrap());
    if let Some(workspace) = noexec_workspace {
        assert_eq!(fs::read_dir(workspace.path()).unwrap().count(), 0);
    }
    fixture.assert_clean();
    assert_eq!(
        Command::new(&fixture.executable).arg("--version").output().unwrap().stdout,
        b"tmup 0.4.0\n"
    );
}

#[test]
fn helper_destination_change_prevents_publication_and_cleans_candidates() {
    let _guard = FIXTURE_LOCK.lock().unwrap_or_else(|error| error.into_inner());
    let fixture = Fixture::new();
    let output = fixture
        .command()
        .arg("--force")
        .env("PREP_MODE", "mutate")
        .env("CHANGE_DEST", &fixture.executable)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("destination changed"));
    assert_eq!(fs::read_to_string(&fixture.executable).unwrap(), "external replacement");
    fixture.assert_clean();
}
