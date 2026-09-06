use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::fs::{PermissionsExt, symlink};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use std::{env, fs};

use tempfile::TempDir;

const TARGET: &str = "x86_64-unknown-linux-musl";

struct InstallerTest {
    root: TempDir,
    bin_dir: PathBuf,
    fixtures_dir: PathBuf,
    tmp_dir: PathBuf,
    download_log: PathBuf,
    authorization_log: PathBuf,
    host_log: PathBuf,
    cargo_log: PathBuf,
}

impl InstallerTest {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let bin_dir = root.path().join("bin");
        let fixtures_dir = root.path().join("fixtures");
        let tmp_dir = root.path().join("tmp");
        fs::create_dir(&bin_dir).unwrap();
        fs::create_dir(&fixtures_dir).unwrap();
        fs::create_dir(&tmp_dir).unwrap();

        let test = Self {
            download_log: root.path().join("downloads.log"),
            authorization_log: root.path().join("authorization.log"),
            host_log: root.path().join("host.log"),
            cargo_log: root.path().join("cargo.log"),
            root,
            bin_dir,
            fixtures_dir,
            tmp_dir,
        };
        for command in [
            "awk", "cat", "chmod", "cp", "grep", "gzip", "ln", "mkdir", "mktemp", "mv", "rm", "tar",
        ] {
            test.link_command(command);
        }
        test.write_fake_curl();
        test.write_fake_wget();
        test.write_fake_host_tools();
        test.write_executable(
            &test.bin_dir.join("cargo"),
            "#!/bin/sh\nprintf 'cargo\\n' >> \"$TMUP_TEST_CARGO_LOG\"\nexit 99\n",
        );
        test
    }

    fn link_command(&self, command: &str) {
        symlink(find_command(command), self.bin_dir.join(command)).unwrap();
    }

    fn write_fake_curl(&self) {
        self.write_executable(
            &self.bin_dir.join("curl"),
            r#"#!/bin/sh
output=
url=
authorization=
progress=false
while [ "$#" -gt 0 ]; do
    case "$1" in
        --progress-bar) progress=true; shift ;;
        --silent) progress=false; shift ;;
        --output)
            output=$2
            shift 2
            ;;
        --header)
            authorization=$2
            shift 2
            ;;
        http://*|https://*)
            url=$1
            shift
            ;;
        *)
            shift
            ;;
    esac
done
printf 'curl %s\n' "$url" >> "$TMUP_TEST_DOWNLOAD_LOG"
[ -z "$authorization" ] || printf '%s\n' "$authorization" >> "$TMUP_TEST_AUTHORIZATION_LOG"
fixture=${url##*/}
fixture=${fixture%%\?*}
status=200
[ -f "$TMUP_TEST_FIXTURES/$fixture" ] || status=404
if [ -f "$TMUP_TEST_FIXTURES/$fixture.status" ]; then
    status=$(cat "$TMUP_TEST_FIXTURES/$fixture.status")
fi
printf '%s' "$status"
if [ -f "$TMUP_TEST_FIXTURES/$fixture.exit" ]; then
    exit "$(cat "$TMUP_TEST_FIXTURES/$fixture.exit")"
fi
[ "$status" = 200 ] || exit 22
if [ "$progress" = true ]; then
    printf '\rdownload progress: %s\n' "$fixture" >&2
    if [ "${TMUP_TEST_WAIT_FOR_PROGRESS:-}" = 1 ]; then
        read -r acknowledgement
        [ "$acknowledgement" = continue ] || exit 99
    fi
fi
cp "$TMUP_TEST_FIXTURES/$fixture" "$output"
"#,
        );
    }

    fn write_fake_wget(&self) {
        self.write_executable(
            &self.bin_dir.join("wget"),
            r#"#!/bin/sh
output=
url=
authorization=
progress=false
log_output=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --show-progress) progress=true; shift ;;
        --output-file) log_output=$2; shift 2 ;;
        --output-document)
            output=$2
            shift 2
            ;;
        --header)
            authorization=$2
            shift 2
            ;;
        http://*|https://*)
            url=$1
            shift
            ;;
        *)
            shift
            ;;
    esac
done
printf 'wget %s\n' "$url" >> "$TMUP_TEST_DOWNLOAD_LOG"
[ -z "$authorization" ] || printf '%s\n' "$authorization" >> "$TMUP_TEST_AUTHORIZATION_LOG"
fixture=${url##*/}
fixture=${fixture%%\?*}
status=200
[ -f "$TMUP_TEST_FIXTURES/$fixture" ] || status=404
if [ -f "$TMUP_TEST_FIXTURES/$fixture.status" ]; then
    status=$(cat "$TMUP_TEST_FIXTURES/$fixture.status")
fi
if [ -n "$log_output" ]; then
    printf '  HTTP/1.1 302 Found\n  Location: https://example.test/asset\n  HTTP/1.1 %s Response\n' "$status" >"$log_output"
else
    printf '  HTTP/1.1 302 Found\n  Location: https://example.test/asset\n  HTTP/1.1 %s Response\n' "$status" >&2
fi
if [ -f "$TMUP_TEST_FIXTURES/$fixture.exit" ]; then
    exit "$(cat "$TMUP_TEST_FIXTURES/$fixture.exit")"
fi
[ "$status" = 200 ] || exit 8
if [ "$progress" = true ]; then
    printf '\rdownload progress: %s\n' "$fixture" >&2
    if [ "${TMUP_TEST_WAIT_FOR_PROGRESS:-}" = 1 ]; then
        read -r acknowledgement
        [ "$acknowledgement" = continue ] || exit 99
    fi
fi
cp "$TMUP_TEST_FIXTURES/$fixture" "$output"
"#,
        );
    }

    fn write_fake_host_tools(&self) {
        self.write_executable(
            &self.bin_dir.join("uname"),
            r#"#!/bin/sh
printf 'uname %s\n' "$1" >> "$TMUP_TEST_HOST_LOG"
case "$1" in
    -s) printf '%s\n' "$TMUP_TEST_HOST_OS" ;;
    -m) printf '%s\n' "$TMUP_TEST_HOST_ARCH" ;;
    *) exit 1 ;;
esac
"#,
        );
        self.write_executable(
            &self.bin_dir.join("sysctl"),
            r#"#!/bin/sh
printf 'sysctl %s\n' "$*" >> "$TMUP_TEST_HOST_LOG"
[ "$TMUP_TEST_ROSETTA" = 1 ] || exit 1
printf '1\n'
"#,
        );
    }

    fn remove_tool(&self, tool: &str) {
        fs::remove_file(self.bin_dir.join(tool)).unwrap();
    }

    fn fail_archive_extraction(&self) {
        let tar = find_command("tar");
        self.remove_tool("tar");
        self.write_executable(
            &self.bin_dir.join("tar"),
            &format!(
                "#!/bin/sh\nif [ \"$1\" = -xzf ]; then exit 1; fi\nexec '{}' \"$@\"\n",
                tar.display()
            ),
        );
    }

    fn assert_temporary_storage_is_empty(&self) {
        assert_eq!(fs::read_dir(&self.tmp_dir).unwrap().count(), 0);
    }

    fn write_executable(&self, path: &Path, contents: &str) {
        fs::write(path, contents).unwrap();
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    fn add_release(&self, version: &str, target: &str, binary: &str) {
        let archive_dir = format!("tmup-v{version}-{target}");
        let payload_root = self.root.path().join("payload");
        let payload_dir = payload_root.join(&archive_dir);
        fs::create_dir_all(&payload_dir).unwrap();
        self.write_executable(&payload_dir.join("tmup"), binary);

        self.package_payload(version, target, &archive_dir);
    }

    fn add_xz_archive(&self, version: &str, target: &str) {
        let name = format!("tmup-v{version}-{target}.tar.xz");
        let status = Command::new(find_command("tar"))
            .arg("-cJf")
            .arg(self.fixtures_dir.join(&name))
            .arg("-C")
            .arg(self.root.path().join("payload"))
            .arg(format!("tmup-v{version}-{target}"))
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn add_latest_release(&self, version: &str, target: &str, binary: &str) {
        self.add_release(version, target, binary);
        self.write_latest_release(version);
    }

    fn write_latest_release(&self, version: &str) {
        fs::write(
            self.fixtures_dir.join("latest"),
            format!("{{\n  \"tag_name\": \"v{version}\"\n}}\n"),
        )
        .unwrap();
    }

    fn write_release_list(&self, releases: &str) {
        fs::write(self.fixtures_dir.join("releases"), releases).unwrap();
    }

    fn package_payload(&self, version: &str, target: &str, payload_member: &str) {
        let archive_name = format!("tmup-v{version}-{target}.tar.gz");
        let archive_path = self.fixtures_dir.join(&archive_name);
        let status = Command::new(find_command("tar"))
            .args(["-czf"])
            .arg(&archive_path)
            .arg("-C")
            .arg(self.root.path().join("payload"))
            .arg(payload_member)
            .status()
            .unwrap();
        assert!(status.success());
    }

    fn archive_path(&self, version: &str, target: &str) -> PathBuf {
        self.fixtures_dir.join(format!("tmup-v{version}-{target}.tar.gz"))
    }

    fn payload_dir(&self, version: &str, target: &str) -> PathBuf {
        self.root.path().join("payload").join(format!("tmup-v{version}-{target}"))
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("/bin/sh");
        command
            .arg(installer_path())
            .args(args)
            .env("PATH", &self.bin_dir)
            .env("TMPDIR", &self.tmp_dir)
            .env("TMUP_TEST_FIXTURES", &self.fixtures_dir)
            .env("TMUP_TEST_DOWNLOAD_LOG", &self.download_log)
            .env("TMUP_TEST_AUTHORIZATION_LOG", &self.authorization_log)
            .env("TMUP_TEST_HOST_LOG", &self.host_log)
            .env("TMUP_TEST_CARGO_LOG", &self.cargo_log)
            .env("TMUP_TEST_HOST_OS", "Linux")
            .env("TMUP_TEST_HOST_ARCH", "x86_64")
            .env("TMUP_TEST_ROSETTA", "0")
            .env("HOME", self.root.path().join("home"))
            .env("TERM", "xterm-256color")
            .env_remove("NO_COLOR")
            .env_remove("TMUP_TEST_WAIT_FOR_PROGRESS")
            .env_remove("GITHUB_TOKEN");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.command(args).output().unwrap()
    }

    fn install(&self, version: &str, target: &str, destination: &Path) -> Output {
        self.run_install(version, target, destination, false)
    }

    fn force_install(&self, version: &str, target: &str, destination: &Path) -> Output {
        self.run_install(version, target, destination, true)
    }

    fn run_install(&self, version: &str, target: &str, destination: &Path, force: bool) -> Output {
        let mut args =
            vec!["--version", version, "--target", target, "--to", destination.to_str().unwrap()];
        if force {
            args.push("--force");
        }
        self.run(&args)
    }
}

fn installer_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("install.sh")
}

fn find_command(name: &str) -> PathBuf {
    find_optional_command(name)
        .unwrap_or_else(|| panic!("required test command `{name}` was not found"))
}

fn find_optional_command(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH").unwrap())
        .map(|directory| directory.join(name))
        .find(|path| path.is_file())
}

fn run_with_terminal_stderr(command: &mut Command, wait_for_progress: bool) -> Output {
    let mut master = -1;
    let mut slave = -1;
    // SAFETY: openpty initializes both descriptors; the optional arguments are null.
    let result = unsafe {
        libc::openpty(
            &mut master,
            &mut slave,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    assert_eq!(result, 0, "openpty: {}", std::io::Error::last_os_error());
    // SAFETY: each newly opened descriptor is transferred to exactly one File.
    let (mut master, slave) =
        unsafe { (fs::File::from_raw_fd(master), fs::File::from_raw_fd(slave)) };
    let mut child =
        command.stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(slave).spawn().unwrap();
    // Command retains its copy of the slave. Close it so the reader can observe EOF.
    command.stderr(Stdio::null());
    let (progress_tx, progress_rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut output = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            match master.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    output.extend_from_slice(&buffer[..count]);
                    if output
                        .windows(b"download progress:".len())
                        .any(|w| w == b"download progress:")
                    {
                        let _ = progress_tx.send(());
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                // Linux reports EIO when the final slave closes; macOS returns EOF.
                Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
                Err(error) => panic!("reading installer terminal: {error}"),
            }
        }
        output
    });
    if wait_for_progress {
        if let Err(error) = progress_rx.recv_timeout(Duration::from_secs(10)) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("download progress was not visible before completion: {error}");
        }
        child.stdin.take().unwrap().write_all(b"continue\n").unwrap();
    }
    let mut output = child.wait_with_output().unwrap();
    output.stderr = reader.join().unwrap();
    output
}

#[test]
fn redirected_installation_reports_ordered_plain_stages_only_on_stderr() {
    for use_wget in [false, true] {
        let test = InstallerTest::new();
        if use_wget {
            test.remove_tool("curl");
        }
        test.add_latest_release("1.2.3", TARGET, "new tmup\n");
        let destination = test.root.path().join("destination");
        let output = test.run(&["--to", destination.to_str().unwrap()]);
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{stderr}");
        assert!(output.stdout.is_empty());
        assert_eq!(
            stderr,
            format!(
                "[info] Resolving the latest stable release...\n\
                 [info] Downloading tmup-v1.2.3-{TARGET}.tar.gz...\n\
                 [info] Extracting tmup-v1.2.3-{TARGET}.tar.gz...\n\
                 [info] Installing tmup to {0}/tmup...\n\
                 [info] Installed tmup v1.2.3 to {0}/tmup\n\
                 [warn] {0} is not in PATH; add it to run tmup directly\n",
                destination.display()
            )
        );
    }
}

#[test]
fn terminal_download_progress_is_visible_before_completion_with_piped_stdin_and_stdout() {
    for use_wget in [false, true] {
        let test = InstallerTest::new();
        if use_wget {
            test.remove_tool("curl");
        }
        test.add_latest_release("1.2.3", TARGET, "new tmup\n");
        let destination = test.root.path().join("destination");
        let mut command = test.command(&["--to", destination.to_str().unwrap()]);
        command.env("TMUP_TEST_WAIT_FOR_PROGRESS", "1");
        let output = run_with_terminal_stderr(&mut command, true);
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{stderr}");
        assert!(output.stdout.is_empty());
        assert!(stderr.contains("\x1b[1;32m[info]\x1b[0m Resolving"), "{stderr}");
        assert!(stderr.contains("\x1b[1;33m[warn]\x1b[0m"), "{stderr}");
        assert_eq!(stderr.matches("download progress:").count(), 1, "{stderr}");
        let progress =
            stderr.find(&format!("download progress: tmup-v1.2.3-{TARGET}.tar.gz")).unwrap();
        assert!(progress < stderr.find("Extracting").unwrap());
        assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "new tmup\n");
        test.assert_temporary_storage_is_empty();
    }
}

#[test]
fn terminal_messages_respect_no_color_and_dumb_term() {
    for (variable, value) in [("NO_COLOR", "1"), ("TERM", "dumb")] {
        let test = InstallerTest::new();
        test.add_release("1.2.3", TARGET, "new tmup\n");
        let destination = test.root.path().join("destination");
        let mut command =
            test.command(&["--version", "1.2.3", "--to", destination.to_str().unwrap()]);
        command.env(variable, value);
        let output = run_with_terminal_stderr(&mut command, false);
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(output.status.success(), "{stderr}");
        assert!(!stderr.contains('\x1b'), "{stderr}");
        assert!(stderr.contains("[info] Installed"), "{stderr}");
        assert!(stderr.contains("[warn]"), "{stderr}");
        assert!(!stderr.contains("Resolving"), "{stderr}");

        let output =
            run_with_terminal_stderr(test.command(&["--unknown"]).env(variable, value), false);
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(!output.status.success());
        assert_eq!(stderr, "[erro] unknown argument: --unknown\r\n");
    }
}

#[test]
fn errors_use_the_error_prefix_and_terminal_color() {
    let test = InstallerTest::new();
    let output = test.run(&["--unknown"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(String::from_utf8(output.stderr).unwrap(), "[erro] unknown argument: --unknown\n");

    let output = run_with_terminal_stderr(&mut test.command(&["--unknown"]), false);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "\x1b[1;31m[erro]\x1b[0m unknown argument: --unknown\r\n"
    );
}

#[test]
fn terminal_downloads_only_fall_back_for_http_404_and_preserve_failed_installs() {
    for use_wget in [false, true] {
        for (http_status, exit_status, fallback) in
            [("404", None, true), ("403", None, false), ("404", Some("4"), false)]
        {
            let test = InstallerTest::new();
            test.link_command("xz");
            if use_wget {
                test.remove_tool("curl");
            }
            test.add_release("1.2.3", TARGET, "new tmup\n");
            let name = format!("tmup-v1.2.3-{TARGET}.tar.xz");
            fs::write(test.fixtures_dir.join(format!("{name}.status")), http_status).unwrap();
            if let Some(exit_status) = exit_status {
                fs::write(test.fixtures_dir.join(format!("{name}.exit")), exit_status).unwrap();
            }
            let destination = test.root.path().join("destination");
            fs::create_dir(&destination).unwrap();
            test.write_executable(&destination.join("tmup"), "existing tmup\n");
            let output = run_with_terminal_stderr(
                &mut test.command(&[
                    "--version",
                    "1.2.3",
                    "--to",
                    destination.to_str().unwrap(),
                    "--force",
                ]),
                false,
            );
            let stderr = String::from_utf8(output.stderr).unwrap();
            assert_eq!(output.status.success(), fallback, "{stderr}");
            assert_eq!(stderr.contains("xz asset not found; downloading"), fallback, "{stderr}");
            assert_eq!(stderr.contains("Installed tmup"), fallback, "{stderr}");
            assert_eq!(stderr.contains("failed to download"), !fallback, "{stderr}");
            assert_eq!(
                fs::read_to_string(destination.join("tmup")).unwrap(),
                if fallback { "new tmup\n" } else { "existing tmup\n" }
            );
            let downloads = fs::read_to_string(&test.download_log).unwrap();
            assert_eq!(downloads.contains(".tar.gz"), fallback);
            test.assert_temporary_storage_is_empty();
        }
    }
}

#[test]
fn installs_an_explicit_release_without_checksum_tools_or_manifest() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let installed = destination.join("tmup");
    assert_eq!(fs::read_to_string(&installed).unwrap(), "new tmup\n");
    assert_ne!(fs::metadata(installed).unwrap().permissions().mode() & 0o111, 0);
    assert_eq!(
        fs::read_to_string(&test.download_log).unwrap(),
        concat!(
            "curl https://github.com/wfxr/tmup/releases/download/v1.2.3/",
            "tmup-v1.2.3-x86_64-unknown-linux-musl.tar.gz\n",
        )
    );
}

#[test]
fn installs_githubs_latest_stable_release_when_version_is_omitted() {
    let test = InstallerTest::new();
    test.add_latest_release("1.2.3", TARGET, "latest tmup\n");
    let destination = test.root.path().join("destination");

    let output = test.run(&["--target", TARGET, "--to", destination.to_str().unwrap()]);

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "latest tmup\n");
    let downloads = fs::read_to_string(&test.download_log).unwrap();
    assert!(downloads.starts_with("curl https://api.github.com/repos/wfxr/tmup/releases/latest\n"));
    assert!(downloads.contains("/releases/download/v1.2.3/tmup-v1.2.3-"));
    assert!(!test.authorization_log.exists());
}

#[test]
fn installs_the_latest_published_release_when_prereleases_are_included() {
    let test = InstallerTest::new();
    test.add_release("2.0.0-rc.1", TARGET, "prerelease tmup\n");
    test.write_release_list(
        r#"[
  {
    "tag_name": "v2.0.0-rc.1",
    "draft": false,
    "prerelease": true
  },
  {
    "tag_name": "v1.2.3",
    "draft": false,
    "prerelease": false
  }
]
"#,
    );
    let destination = test.root.path().join("destination");

    let output = test.run(&[
        "--include-prerelease",
        "--target",
        TARGET,
        "--to",
        destination.to_str().unwrap(),
    ]);

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "prerelease tmup\n");
    let downloads = fs::read_to_string(&test.download_log).unwrap();
    assert!(
        downloads
            .starts_with("curl https://api.github.com/repos/wfxr/tmup/releases?per_page=100\n")
    );
    assert!(downloads.contains("/releases/download/v2.0.0-rc.1/tmup-v2.0.0-rc.1-"));
}

#[test]
fn pre_is_an_alias_for_including_prereleases() {
    let test = InstallerTest::new();
    test.add_release("2.0.0-rc.1", TARGET, "prerelease tmup\n");
    test.write_release_list(
        r#"[
  {
    "tag_name": "v2.0.0-rc.1",
    "draft": false,
    "prerelease": true
  }
]
"#,
    );
    let destination = test.root.path().join("destination");

    let output = test.run(&["--pre", "--target", TARGET, "--to", destination.to_str().unwrap()]);

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "prerelease tmup\n");
}

#[test]
fn latest_published_release_selection_skips_drafts_visible_to_authenticated_users() {
    let test = InstallerTest::new();
    test.add_release("2.0.0-rc.1", TARGET, "published tmup\n");
    test.write_release_list(
        r#"[
  {
    "tag_name": "v2.1.0-rc.1",
    "draft": true,
    "prerelease": true
  },
  {
    "tag_name": "v2.0.0-rc.1",
    "draft": false,
    "prerelease": true
  }
]
"#,
    );
    let destination = test.root.path().join("destination");

    let output = test
        .command(&["--pre", "--target", TARGET, "--to", destination.to_str().unwrap()])
        .env("GITHUB_TOKEN", "test-token")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "published tmup\n");
    assert_eq!(
        fs::read_to_string(&test.authorization_log).unwrap(),
        "Authorization: Bearer test-token\n"
    );
}

#[test]
fn rejects_combining_an_exact_version_with_prerelease_selection() {
    for prerelease_option in ["--include-prerelease", "--pre"] {
        let test = InstallerTest::new();
        let destination = test.root.path().join("destination");

        let output = test.run(&[
            "--version",
            "2.0.0-rc.1",
            prerelease_option,
            "--target",
            TARGET,
            "--to",
            destination.to_str().unwrap(),
        ]);

        assert!(!output.status.success());
        assert!(
            String::from_utf8(output.stderr).unwrap().contains("cannot be combined with --version")
        );
        assert!(!test.download_log.exists());
        assert!(!destination.exists());
    }
}

#[test]
fn installs_to_home_local_bin_by_default_and_warns_when_it_is_not_in_path() {
    let test = InstallerTest::new();
    test.add_latest_release("1.2.3", TARGET, "default install tmup\n");

    let output = test.run(&[]);

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let default_destination = test.root.path().join("home/.local/bin");
    assert_eq!(
        fs::read_to_string(default_destination.join("tmup")).unwrap(),
        "default install tmup\n"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains(default_destination.to_str().unwrap()));
    assert!(stderr.contains("not in PATH"));
}

#[test]
fn does_not_warn_when_the_destination_is_already_in_path() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "path tmup\n");
    let destination = test.root.path().join("destination");
    let path = env::join_paths([test.bin_dir.as_path(), destination.as_path()]).unwrap();
    let output = test
        .command(&["--version", "1.2.3", "--target", TARGET, "--to", destination.to_str().unwrap()])
        .env("PATH", path)
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!String::from_utf8(output.stderr).unwrap().contains("not in PATH"));
}

#[test]
fn authenticates_latest_release_lookups_when_github_token_is_present() {
    for downloader in ["curl", "wget"] {
        let test = InstallerTest::new();
        if downloader == "wget" {
            test.remove_tool("curl");
        }
        test.add_latest_release("1.2.3", TARGET, "authenticated tmup\n");
        let destination = test.root.path().join("destination");
        let output = test
            .command(&["--target", TARGET, "--to", destination.to_str().unwrap()])
            .env("GITHUB_TOKEN", "test-token")
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "{downloader} installer failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            fs::read_to_string(&test.authorization_log).unwrap(),
            "Authorization: Bearer test-token\n"
        );
    }
}

#[test]
fn refuses_a_prerelease_returned_by_the_latest_stable_endpoint() {
    let test = InstallerTest::new();
    test.write_latest_release("2.0.0-rc.1");
    let destination = test.root.path().join("destination");

    let output = test.run(&["--target", TARGET, "--to", destination.to_str().unwrap()]);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("not stable"));
    assert_eq!(fs::read_to_string(&test.download_log).unwrap().lines().count(), 1);
    assert!(!destination.exists());
    test.assert_temporary_storage_is_empty();
}

#[test]
fn latest_release_lookup_failure_does_not_install_or_leave_temporary_state() {
    let test = InstallerTest::new();
    let destination = test.root.path().join("destination");

    let output = test.run(&["--target", TARGET, "--to", destination.to_str().unwrap()]);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("failed to resolve"));
    assert!(!destination.exists());
    test.assert_temporary_storage_is_empty();
}

#[test]
fn help_describes_installer_options_and_defaults() {
    let test = InstallerTest::new();

    let output = test.run(&["--help"]);

    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for option in
        ["--version", "--include-prerelease", "--pre", "--target", "--to", "--force", "--help"]
    {
        assert!(stdout.contains(option), "help omitted {option}: {stdout}");
    }
    for default in ["latest stable", "native host target", "~/.local/bin"] {
        assert!(stdout.contains(default), "help omitted {default}: {stdout}");
    }
}

#[test]
fn rejects_unknown_installer_options() {
    let test = InstallerTest::new();
    let destination = test.root.path().join("destination");

    let output = test.run(&[
        "--version",
        "1.2.3",
        "--target",
        TARGET,
        "--to",
        destination.to_str().unwrap(),
        "--no-verify",
    ]);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("unknown argument"));
    assert!(!test.download_log.exists());
    assert!(!destination.exists());
}

#[test]
fn normalizes_a_leading_v_on_prerelease_versions() {
    let test = InstallerTest::new();
    test.add_release("2.0.0-rc.1", TARGET, "prerelease tmup\n");
    let destination = test.root.path().join("destination");

    let output = test.install("v2.0.0-rc.1", TARGET, &destination);

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "prerelease tmup\n");
    assert!(
        fs::read_to_string(&test.download_log)
            .unwrap()
            .contains("/download/v2.0.0-rc.1/tmup-v2.0.0-rc.1-")
    );
}

#[test]
fn rejects_versions_outside_the_release_semver_contract() {
    for version in [
        "1.2",
        "01.2.3",
        "1.02.3",
        "1.2.03",
        "1.2.3-",
        "1.2.3-alpha..1",
        "1.2.3-01",
        "1.2.3+build.1",
        "1.2.3\njunk",
        "junk\n1.2.3",
        "v",
        "vv1.2.3",
    ] {
        let test = InstallerTest::new();
        let destination = test.root.path().join("destination");

        let output = test.install(version, TARGET, &destination);

        assert!(!output.status.success(), "invalid version {version} was accepted");
        assert!(!test.download_log.exists(), "invalid version {version} reached the downloader");
        assert!(!destination.exists());
    }
}

#[test]
fn rejects_empty_explicit_version_and_target_values_before_download() {
    let cases = [
        ("--version", ["--version", "", "--target", TARGET]),
        ("--target", ["--version", "1.2.3", "--target", ""]),
    ];

    for (option, args) in cases {
        let test = InstallerTest::new();
        let destination = test.root.path().join("destination");
        let mut args = args.to_vec();
        args.extend(["--to", destination.to_str().unwrap()]);

        let output = test.run(&args);

        assert!(!output.status.success(), "empty {option} value was accepted");
        assert!(
            String::from_utf8(output.stderr)
                .unwrap()
                .contains(&format!("{option} requires a non-empty value"))
        );
        assert!(!test.download_log.exists());
        assert!(!test.host_log.exists());
        assert!(!destination.exists());
    }
}

#[test]
fn rejects_unsupported_explicit_targets_with_the_supported_list() {
    let test = InstallerTest::new();
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", "x86_64-unknown-linux-gnu", &destination);

    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    for target in [
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
    ] {
        assert!(stderr.contains(target), "error omitted {target}: {stderr}");
    }
    assert!(!test.download_log.exists());
    assert!(!destination.exists());
}

#[test]
fn rejects_multiple_supported_targets_as_one_override() {
    let test = InstallerTest::new();
    let destination = test.root.path().join("destination");
    let combined_target = "x86_64-unknown-linux-musl aarch64-unknown-linux-musl";

    let output = test.install("1.2.3", combined_target, &destination);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("unsupported target"));
    assert!(!test.download_log.exists());
    assert!(!destination.exists());
}

#[test]
fn accepts_each_supported_explicit_target() {
    for target in [
        "x86_64-unknown-linux-musl",
        "aarch64-unknown-linux-musl",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
    ] {
        let test = InstallerTest::new();
        test.add_release("1.2.3", target, target);
        let destination = test.root.path().join("destination");

        let output = test.install("1.2.3", target, &destination);

        assert!(
            output.status.success(),
            "target {target} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), target);
    }
}

#[test]
fn detects_each_supported_physical_host_target() {
    let cases = [
        ("Linux", "x86_64", "0", "x86_64-unknown-linux-musl"),
        ("Linux", "aarch64", "0", "aarch64-unknown-linux-musl"),
        ("Darwin", "x86_64", "0", "x86_64-apple-darwin"),
        ("Darwin", "arm64", "0", "aarch64-apple-darwin"),
        ("Darwin", "x86_64", "1", "aarch64-apple-darwin"),
    ];

    for (os, arch, rosetta, expected_target) in cases {
        let test = InstallerTest::new();
        test.add_release("1.2.3", expected_target, expected_target);
        let destination = test.root.path().join("destination");
        let output = test
            .command(&["--version", "1.2.3", "--to", destination.to_str().unwrap()])
            .env("TMUP_TEST_HOST_OS", os)
            .env("TMUP_TEST_HOST_ARCH", arch)
            .env("TMUP_TEST_ROSETTA", rosetta)
            .output()
            .unwrap();

        assert!(
            output.status.success(),
            "{os}/{arch} (Rosetta {rosetta}) failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), expected_target);
    }
}

#[test]
fn explicit_target_bypasses_host_detection() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "explicit target tmup\n");
    let destination = test.root.path().join("destination");
    let output = test
        .command(&["--version", "1.2.3", "--target", TARGET, "--to", destination.to_str().unwrap()])
        .env("TMUP_TEST_HOST_OS", "unsupported-os")
        .env("TMUP_TEST_HOST_ARCH", "unsupported-arch")
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "explicit target tmup\n");
    assert!(!test.host_log.exists());
}

#[test]
fn rejects_unsupported_hosts_without_downloading_or_invoking_cargo() {
    for (os, arch) in [("FreeBSD", "x86_64"), ("Linux", "riscv64"), ("Darwin", "powerpc")] {
        let test = InstallerTest::new();
        let destination = test.root.path().join("destination");
        let output = test
            .command(&["--version", "1.2.3", "--to", destination.to_str().unwrap()])
            .env("TMUP_TEST_HOST_OS", os)
            .env("TMUP_TEST_HOST_ARCH", arch)
            .output()
            .unwrap();

        assert!(!output.status.success(), "unsupported host {os}/{arch} was accepted");
        let stderr = String::from_utf8(output.stderr).unwrap();
        for target in [
            "x86_64-unknown-linux-musl",
            "aarch64-unknown-linux-musl",
            "x86_64-apple-darwin",
            "aarch64-apple-darwin",
        ] {
            assert!(stderr.contains(target), "error omitted {target}: {stderr}");
        }
        assert!(!test.download_log.exists());
        assert!(!test.cargo_log.exists());
        assert!(!destination.exists());
    }
}

#[test]
fn falls_back_to_wget() {
    let test = InstallerTest::new();
    test.remove_tool("curl");
    test.add_release("1.2.3", TARGET, "fallback tmup\n");
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        fs::read_to_string(&test.download_log)
            .unwrap()
            .lines()
            .all(|line| line.starts_with("wget "))
    );
}

#[test]
fn fails_when_no_supported_downloader_is_available() {
    let test = InstallerTest::new();
    test.remove_tool("curl");
    test.remove_tool("wget");
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("curl or wget"));
    assert!(!destination.exists());
    test.assert_temporary_storage_is_empty();
}

#[test]
fn refuses_to_replace_an_existing_binary_without_force() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    let destination = test.root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    test.write_executable(&destination.join("tmup"), "existing tmup\n");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("--force"));
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "existing tmup\n");
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
    test.assert_temporary_storage_is_empty();
}

#[test]
fn force_replaces_an_existing_binary() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "replacement tmup\n");
    let destination = test.root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    test.write_executable(&destination.join("tmup"), "existing tmup\n");

    let output = test.force_install("1.2.3", TARGET, &destination);

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "replacement tmup\n");
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
    test.assert_temporary_storage_is_empty();
}

#[test]
fn force_replaces_a_symlink_to_a_directory() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "replacement tmup\n");
    let destination = test.root.path().join("destination");
    let symlink_target = test.root.path().join("symlink-target");
    fs::create_dir(&destination).unwrap();
    fs::create_dir(&symlink_target).unwrap();
    symlink(&symlink_target, destination.join("tmup")).unwrap();

    let output = test.force_install("1.2.3", TARGET, &destination);

    assert!(
        output.status.success(),
        "installer failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let installed = destination.join("tmup");
    assert!(!fs::symlink_metadata(&installed).unwrap().file_type().is_symlink());
    assert_eq!(fs::read_to_string(installed).unwrap(), "replacement tmup\n");
    assert_eq!(fs::read_dir(&symlink_target).unwrap().count(), 0);
    test.assert_temporary_storage_is_empty();
}

#[test]
fn archive_download_failure_preserves_an_existing_binary() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    fs::remove_file(test.archive_path("1.2.3", TARGET)).unwrap();
    let destination = test.root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    test.write_executable(&destination.join("tmup"), "existing tmup\n");

    let output = test.force_install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("failed to download"));
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "existing tmup\n");
    test.assert_temporary_storage_is_empty();
}

#[test]
fn malformed_archive_preserves_an_existing_binary() {
    let test = InstallerTest::new();
    let archive_name = format!("tmup-v1.2.3-{TARGET}.tar.gz");
    fs::write(test.fixtures_dir.join(&archive_name), "not a tar archive\n").unwrap();
    let destination = test.root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    test.write_executable(&destination.join("tmup"), "existing tmup\n");

    let output = test.force_install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "existing tmup\n");
    test.assert_temporary_storage_is_empty();
}

#[test]
fn extraction_failure_preserves_an_existing_binary() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    test.fail_archive_extraction();
    let destination = test.root.path().join("destination");
    fs::create_dir(&destination).unwrap();
    test.write_executable(&destination.join("tmup"), "existing tmup\n");

    let output = test.force_install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(String::from_utf8(output.stderr).unwrap().contains("could not extract"));
    assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "existing tmup\n");
    assert_eq!(fs::read_dir(&destination).unwrap().count(), 1);
    test.assert_temporary_storage_is_empty();
}

#[test]
fn archive_with_extra_payload_is_rejected() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    fs::write(test.payload_dir("1.2.3", TARGET).join("README"), "extra\n").unwrap();
    test.package_payload("1.2.3", TARGET, &format!("tmup-v1.2.3-{TARGET}"));
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(!destination.exists());
    test.assert_temporary_storage_is_empty();
}

#[test]
fn archive_with_a_symlink_instead_of_the_binary_is_rejected() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    let binary = test.payload_dir("1.2.3", TARGET).join("tmup");
    fs::remove_file(&binary).unwrap();
    symlink("/bin/sh", &binary).unwrap();
    test.package_payload("1.2.3", TARGET, &format!("tmup-v1.2.3-{TARGET}"));
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(!destination.exists());
    test.assert_temporary_storage_is_empty();
}

#[test]
fn archive_with_a_non_executable_binary_is_rejected() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    let binary = test.payload_dir("1.2.3", TARGET).join("tmup");
    let mut permissions = fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(&binary, permissions).unwrap();
    test.package_payload("1.2.3", TARGET, &format!("tmup-v1.2.3-{TARGET}"));
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(!destination.exists());
    test.assert_temporary_storage_is_empty();
}

#[test]
fn archive_with_the_binary_outside_the_versioned_directory_is_rejected() {
    let test = InstallerTest::new();
    test.add_release("1.2.3", TARGET, "new tmup\n");
    let wrong_dir = test.root.path().join("payload/wrong-directory");
    fs::rename(test.payload_dir("1.2.3", TARGET), &wrong_dir).unwrap();
    test.package_payload("1.2.3", TARGET, "wrong-directory");
    let destination = test.root.path().join("destination");

    let output = test.install("1.2.3", TARGET, &destination);

    assert!(!output.status.success());
    assert!(!destination.exists());
    test.assert_temporary_storage_is_empty();
}

#[test]
fn prefers_xz_without_downloading_gzip_with_curl_or_wget() {
    for use_wget in [false, true] {
        let test = InstallerTest::new();
        test.link_command("xz");
        let tar = find_command("tar");
        test.remove_tool("tar");
        test.write_executable(
            &test.bin_dir.join("tar"),
            &format!(
                "#!/bin/sh\ncase \"$1\" in *J*) exit 99;; esac\nexec '{}' \"$@\"\n",
                tar.display()
            ),
        );
        if use_wget {
            test.remove_tool("curl");
        }
        test.add_release("1.2.3", TARGET, "xz tmup\n");
        test.add_xz_archive("1.2.3", TARGET);
        let destination = test.root.path().join("destination");
        let output = test.install("1.2.3", TARGET, &destination);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "xz tmup\n");
        let downloads = fs::read_to_string(&test.download_log).unwrap();
        assert_eq!(downloads.lines().count(), 1);
        assert!(downloads.contains(".tar.xz"));
        assert!(!downloads.contains(".tar.gz"));
        test.assert_temporary_storage_is_empty();
    }
}

#[test]
fn uses_gzip_when_xz_is_missing_or_unusable() {
    for broken_xz in [false, true] {
        let test = InstallerTest::new();
        if broken_xz {
            test.write_executable(&test.bin_dir.join("xz"), "#!/bin/sh\nexit 1\n");
        }
        test.add_release("1.2.3", TARGET, "gzip tmup\n");
        test.add_xz_archive("1.2.3", TARGET);
        let destination = test.root.path().join("destination");
        let output = test.install("1.2.3", TARGET, &destination);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let downloads = fs::read_to_string(&test.download_log).unwrap();
        assert!(downloads.contains(".tar.gz"));
        assert!(!downloads.contains(".tar.xz"));
    }
}

#[test]
fn uses_gzip_for_older_releases_even_when_xz_is_available() {
    for use_wget in [false, true] {
        let test = InstallerTest::new();
        test.link_command("xz");
        if use_wget {
            test.remove_tool("curl");
        }
        test.add_release("1.2.3", TARGET, "old tmup\n");
        let destination = test.root.path().join("destination");
        let output = test.install("1.2.3", TARGET, &destination);
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "old tmup\n");
        let downloads = fs::read_to_string(&test.download_log).unwrap();
        let downloads = downloads.lines().collect::<Vec<_>>();
        assert_eq!(downloads.len(), 2);
        assert!(downloads[0].ends_with(".tar.xz"));
        assert!(downloads[1].ends_with(".tar.gz"));
        test.assert_temporary_storage_is_empty();
    }
}

#[test]
fn xz_failures_preserve_existing_binary_without_downloading_gzip() {
    for failure in ["decompression", "layout", "extraction"] {
        let test = InstallerTest::new();
        test.link_command("xz");
        test.add_release("1.2.3", TARGET, "replacement tmup\n");
        if failure == "layout" {
            fs::write(test.payload_dir("1.2.3", TARGET).join("extra"), "unexpected").unwrap();
        }
        test.add_xz_archive("1.2.3", TARGET);
        let name = format!("tmup-v1.2.3-{TARGET}.tar.xz");
        let archive = test.fixtures_dir.join(&name);
        match failure {
            "decompression" => {
                fs::write(&archive, "not xz").unwrap();
            }
            "extraction" => {
                let tar = find_command("tar");
                test.remove_tool("tar");
                test.write_executable(
                    &test.bin_dir.join("tar"),
                    &format!(
                        "#!/bin/sh\nif [ \"$1\" = -xf ]; then exit 1; fi\nexec '{}' \"$@\"\n",
                        tar.display()
                    ),
                );
            }
            _ => {}
        }
        let destination = test.root.path().join("destination");
        fs::create_dir(&destination).unwrap();
        test.write_executable(&destination.join("tmup"), "existing tmup\n");
        let output = test.force_install("1.2.3", TARGET, &destination);
        assert!(!output.status.success(), "accepted {failure} failure");
        assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "existing tmup\n");
        assert!(!fs::read_to_string(&test.download_log).unwrap().contains(".tar.gz"));
        test.assert_temporary_storage_is_empty();
    }
}

#[test]
fn xz_download_errors_other_than_http_404_preserve_the_existing_binary() {
    for use_wget in [false, true] {
        // Include HTTP failures, transport failures, and a non-HTTP failure with a stale 404.
        for (http_status, exit_status) in [
            ("403", None),
            ("500", None),
            ("000", Some("4")),
            ("200", Some("18")),
            ("200", Some("44")),
            ("404", Some("4")),
        ] {
            let test = InstallerTest::new();
            test.link_command("xz");
            if use_wget {
                test.remove_tool("curl");
            }
            test.add_release("1.2.3", TARGET, "replacement tmup\n");
            test.add_xz_archive("1.2.3", TARGET);
            let name = format!("tmup-v1.2.3-{TARGET}.tar.xz");
            fs::write(test.fixtures_dir.join(format!("{name}.status")), http_status).unwrap();
            if let Some(exit_status) = exit_status {
                fs::write(test.fixtures_dir.join(format!("{name}.exit")), exit_status).unwrap();
            }
            let destination = test.root.path().join("destination");
            fs::create_dir(&destination).unwrap();
            test.write_executable(&destination.join("tmup"), "existing tmup\n");

            let output = test.force_install("1.2.3", TARGET, &destination);

            assert!(
                !output.status.success(),
                "accepted HTTP {http_status}, exit {exit_status:?}, wget={use_wget}"
            );
            assert!(String::from_utf8_lossy(&output.stderr).contains("failed to download"));
            assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "existing tmup\n");
            assert!(!fs::read_to_string(&test.download_log).unwrap().contains(".tar.gz"));
            test.assert_temporary_storage_is_empty();
        }
    }
}

#[test]
fn missing_gzip_after_xz_404_preserves_the_existing_binary() {
    for use_wget in [false, true] {
        let test = InstallerTest::new();
        test.link_command("xz");
        if use_wget {
            test.remove_tool("curl");
        }
        let destination = test.root.path().join("destination");
        fs::create_dir(&destination).unwrap();
        test.write_executable(&destination.join("tmup"), "existing tmup\n");

        let output = test.force_install("1.2.3", TARGET, &destination);

        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("failed to download"));
        assert_eq!(fs::read_to_string(destination.join("tmup")).unwrap(), "existing tmup\n");
        let downloads = fs::read_to_string(&test.download_log).unwrap();
        assert_eq!(downloads.lines().count(), 2);
        assert!(downloads.lines().last().unwrap().ends_with(".tar.gz"));
        test.assert_temporary_storage_is_empty();
    }
}
