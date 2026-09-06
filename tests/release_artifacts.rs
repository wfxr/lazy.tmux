#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

use assert_cmd::Command;
use predicates::prelude::*;

fn release_script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/release").join(name)
}

fn release_scripts_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/release")
}

fn package_tag() -> String {
    format!("v{}", env!("CARGO_PKG_VERSION"))
}

fn release_targets_from(scripts: &std::path::Path) -> Vec<String> {
    let output = std::process::Command::new(scripts.join("release-targets.sh")).output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().lines().map(str::to_owned).collect()
}

fn release_targets() -> Vec<String> {
    release_targets_from(&release_scripts_dir())
}

fn fake_release_binary_for_version(path: &std::path::Path, version: &str) {
    std::fs::write(path, format!("#!/bin/sh\nprintf 'tmup {version}\\n'\n")).unwrap();
    let mut permissions = std::fs::metadata(path).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).unwrap();
}

fn package_release_archives(root: &std::path::Path) -> PathBuf {
    package_release_archives_with(root, &release_scripts_dir(), env!("CARGO_PKG_VERSION"))
}

fn package_unrunnable_release_archives(root: &std::path::Path) -> PathBuf {
    let downloads = root.join("downloads");
    let payloads = root.join("payloads");
    std::fs::create_dir_all(&downloads).unwrap();

    for target in release_targets() {
        let package_name = format!("tmup-v{}-{target}", env!("CARGO_PKG_VERSION"));
        let package_dir = payloads.join(&package_name);
        std::fs::create_dir_all(&package_dir).unwrap();
        let binary = package_dir.join("tmup");
        std::fs::write(&binary, "#!/bin/sh\nexit 91\n").unwrap();
        let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).unwrap();

        for (format, flag) in [("gz", "-czf"), ("xz", "-cJf")] {
            let status = std::process::Command::new("tar")
                .args([flag])
                .arg(downloads.join(format!("{package_name}.tar.{format}")))
                .arg("-C")
                .arg(&payloads)
                .arg(&package_name)
                .status()
                .unwrap();
            assert!(status.success());
        }
    }

    downloads
}

fn package_release_archives_with(
    root: &std::path::Path,
    scripts: &std::path::Path,
    version: &str,
) -> PathBuf {
    let downloads = root.join("downloads");
    let binary = root.join("tmup");
    fake_release_binary_for_version(&binary, version);

    for target in release_targets_from(scripts) {
        Command::new(scripts.join("package.sh"))
            .arg(format!("v{version}"))
            .arg(&target)
            .arg(&binary)
            .arg(&downloads)
            .assert()
            .success();
    }

    downloads
}

fn isolated_release_scripts(root: &std::path::Path, version: &str) -> PathBuf {
    let project = root.join("isolated-project");
    let scripts = project.join("scripts/release");
    std::fs::create_dir_all(&scripts).unwrap();
    std::fs::write(
        project.join("Cargo.toml"),
        format!("[package]\nname = \"tmup\"\nversion = \"{version}\"\nedition = \"2024\"\n"),
    )
    .unwrap();

    for entry in std::fs::read_dir(release_scripts_dir()).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), scripts.join(entry.file_name())).unwrap();
    }

    scripts
}

fn prepare_release_assets(root: &std::path::Path) -> PathBuf {
    let downloads = package_release_archives(root);
    let release = root.join("release");
    Command::new(release_script("prepare-assets.sh"))
        .arg(package_tag())
        .arg(downloads)
        .arg(&release)
        .assert()
        .success();
    release
}

fn prepare_unrunnable_release_assets(root: &std::path::Path) -> PathBuf {
    let downloads = package_unrunnable_release_archives(root);
    let release = root.join("release");
    Command::new(release_script("prepare-assets.sh"))
        .arg(package_tag())
        .arg(downloads)
        .arg(&release)
        .assert()
        .success();
    release
}

fn git(root: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git").args(args).current_dir(root).output().unwrap();
    assert!(
        output.status.success(),
        "git {} failed: {}",
        args.join(" "),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap().trim().to_string()
}

fn release_tag_repo(tag_kind: &str) -> tempfile::TempDir {
    let repo = tempfile::tempdir().unwrap();
    git(repo.path(), &["init", "--quiet"]);
    git(repo.path(), &["config", "user.email", "tmup@example.com"]);
    git(repo.path(), &["config", "user.name", "tmup tests"]);
    git(repo.path(), &["config", "commit.gpgSign", "false"]);
    git(repo.path(), &["config", "tag.gpgSign", "false"]);
    git(repo.path(), &["config", "core.hooksPath", ".git/no-hooks"]);
    std::fs::write(repo.path().join("README"), "release\n").unwrap();
    git(repo.path(), &["add", "README"]);
    git(repo.path(), &["commit", "--quiet", "-m", "release"]);
    git(repo.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]);

    let tag = package_tag();
    match tag_kind {
        "lightweight" => {
            git(repo.path(), &["tag", &tag]);
        }
        "annotated" => {
            git(repo.path(), &["tag", "--annotate", &tag, "--message", "release"]);
        }
        _ => panic!("unknown tag kind: {tag_kind}"),
    }

    repo
}

struct FakeGh {
    root: PathBuf,
    bin_dir: PathBuf,
    state: PathBuf,
    assets: PathBuf,
    lock_label: PathBuf,
    run_status: PathBuf,
    after_lock: PathBuf,
    after_archive: PathBuf,
    create_visibility_lag: PathBuf,
    label_cleanup: PathBuf,
    log: PathBuf,
}

impl FakeGh {
    fn new(root: &std::path::Path, initial_state: &str) -> Self {
        let bin_dir = root.join("bin");
        let state = root.join("gh-state");
        let assets = root.join("gh-assets");
        let lock_label = root.join("gh-lock-label");
        let run_status = root.join("gh-run-status");
        let after_lock = root.join("gh-after-lock");
        let after_archive = root.join("gh-after-archive");
        let create_visibility_lag = root.join("gh-create-visibility-lag");
        let label_cleanup = root.join("gh-label-cleanup");
        let log = root.join("gh.log");
        std::fs::create_dir(&bin_dir).unwrap();
        std::fs::write(&state, format!("{initial_state}\n")).unwrap();
        std::fs::write(root.join("gh-prerelease"), "false\n").unwrap();
        std::fs::write(&lock_label, "\n").unwrap();
        std::fs::write(&run_status, "in_progress\n").unwrap();
        std::fs::write(&after_lock, "none\n").unwrap();
        std::fs::write(&after_archive, "none\n").unwrap();
        std::fs::write(&create_visibility_lag, "0\n").unwrap();
        std::fs::write(&label_cleanup, "success\n").unwrap();

        let gh = bin_dir.join("gh");
        std::fs::write(
        &gh,
r###"#!/bin/sh
set -eu

has_slurp=false
has_jq=false
for argument in "$@"; do
    case "$argument" in
        --slurp) has_slurp=true ;;
        --jq) has_jq=true ;;
    esac
done
if [ "$has_slurp" = true ] && [ "$has_jq" = true ]; then
    echo 'the `--slurp` option is not supported with `--jq` or `--template`' >&2
    exit 1
fi

printf '%s\n' "$*" >> "$FAKE_GH_LOG"

case "$1:$2" in
    api:*)
        if [ "$2:$3" = "--method:PATCH" ]; then
            [ "$4" = "repos/{owner}/{repo}/releases/assets/1" ] || exit 2
            [ "$(cat "$FAKE_GH_STATE")" = public ] || exit 1
            [ "$(cat "$FAKE_GH_LABEL_CLEANUP")" = success ] || exit 1
            echo > "$FAKE_GH_LOCK_LABEL"
            printf '{}\n'
            exit 0
        fi
        case "$2" in
            */releases/tags/*)
                [ "$(cat "$FAKE_GH_STATE")" = public ] || exit 1
                ;;
        esac
        if [ "$has_slurp" = true ] && [ "$has_jq" = false ]; then
            visibility_lag=$(cat "$FAKE_GH_CREATE_VISIBILITY_LAG")
            if [ "$(cat "$FAKE_GH_STATE")" = draft ] && [ "$visibility_lag" -gt 0 ]; then
                printf '%s\n' "$((visibility_lag - 1))" > "$FAKE_GH_CREATE_VISIBILITY_LAG"
                printf '[[]]\n'
                exit 0
            fi
            case "$(cat "$FAKE_GH_STATE")" in
                missing)
                    printf '[[]]\n'
                    ;;
                draft) draft=true ;;
                public) draft=false ;;
                *) exit 2 ;;
            esac
            if [ "$(cat "$FAKE_GH_STATE")" != missing ]; then
                printf '[[{"tag_name":"%s","draft":%s,"prerelease":%s,"assets":[' \
                    "$FAKE_GH_TAG" "$draft" "$(cat "$FAKE_GH_PRERELEASE")"
                separator=
                if [ -f "$FAKE_GH_ASSETS" ]; then
                    while IFS="$(printf '\t')" read -r name asset_state digest; do
                        [ -n "$name" ] || continue
                        label=
                        if [ "$name" = SHA256SUMS ]; then
                            label=$(cat "$FAKE_GH_LOCK_LABEL")
                        fi
                        printf '%s{"id":1,"name":"%s","state":"%s","digest":"%s","label":"%s"}' \
                            "$separator" "$name" "$asset_state" "$digest" "$label"
                        separator=,
                    done < "$FAKE_GH_ASSETS"
                fi
                printf ']}]]\n'
            fi
            exit 0
        fi
        query=
        previous=
        for argument in "$@"; do
            if [ "$previous" = --jq ]; then
                query=$argument
                break
            fi
            previous=$argument
        done
        case "$query" in
            *'if length == 0'*)
                cat "$FAKE_GH_STATE"
                ;;
            *'.[0].prerelease'*)
                cat "$FAKE_GH_PRERELEASE"
                ;;
            *'select(.name == "SHA256SUMS")'*)
                if [ -f "$FAKE_GH_ASSETS" ] &&
                    awk -F '\t' '$1 == "SHA256SUMS" { found = 1 } END { exit !found }' "$FAKE_GH_ASSETS"; then
                    digest=$(awk -F '\t' '$1 == "SHA256SUMS" { print $3 }' "$FAKE_GH_ASSETS")
                    printf '%s\t%s\n' "$(cat "$FAKE_GH_LOCK_LABEL")" "$digest"
                fi
                ;;
            *'.assets[].name')
                if [ -f "$FAKE_GH_ASSETS" ]; then
                    cut -f 1 "$FAKE_GH_ASSETS"
                fi
                ;;
            *'.assets[] | [.name, .state, .digest] | @tsv')
                if [ -f "$FAKE_GH_ASSETS" ]; then
                    cat "$FAKE_GH_ASSETS"
                fi
                ;;
            .status)
                cat "$FAKE_GH_RUN_STATUS"
                ;;
            *)
                echo "unsupported fake gh api query: $query" >&2
                exit 2
                ;;
        esac
        ;;
    release:create)
        [ "$(cat "$FAKE_GH_STATE")" = missing ] || exit 1
        echo draft > "$FAKE_GH_STATE"
        echo false > "$FAKE_GH_PRERELEASE"
        for argument in "$@"; do
            if [ "$argument" = --prerelease ]; then
                echo true > "$FAKE_GH_PRERELEASE"
            fi
        done
        ;;
    release:delete-asset)
        asset_name=$4
        temporary="$FAKE_GH_ASSETS.tmp"
        awk -F '\t' -v name="$asset_name" '$1 != name' "$FAKE_GH_ASSETS" > "$temporary"
        mv "$temporary" "$FAKE_GH_ASSETS"
        if [ "$asset_name" = SHA256SUMS ]; then
            echo > "$FAKE_GH_LOCK_LABEL"
        fi
        ;;
    release:upload)
        touch "$FAKE_GH_ASSETS"
        for argument in "$@"; do
            path=${argument%%#*}
            if [ -f "$path" ]; then
                name=$(basename "$path")
                temporary="$FAKE_GH_ASSETS.tmp"
                awk -F '\t' -v name="$name" '$1 != name' "$FAKE_GH_ASSETS" > "$temporary"
                mv "$temporary" "$FAKE_GH_ASSETS"
                digest=$(sha256sum "$path" | awk '{ print $1 }')
                printf '%s\tuploaded\tsha256:%s\n' "$name" "$digest" >> "$FAKE_GH_ASSETS"
                if [ "$name" = SHA256SUMS ]; then
                    case "$argument" in
                        *'#'*) printf '%s\n' "${argument#*#}" > "$FAKE_GH_LOCK_LABEL" ;;
                        *) echo > "$FAKE_GH_LOCK_LABEL" ;;
                    esac
                    case "$(cat "$FAKE_GH_AFTER_LOCK")" in
                        corrupt-lock) echo malformed-owner > "$FAKE_GH_LOCK_LABEL" ;;
                        publish) echo public > "$FAKE_GH_STATE" ;;
                    esac
                else
                    case "$(cat "$FAKE_GH_AFTER_ARCHIVE")" in
                        corrupt-lock) echo tmup-publication-run-456-attempt-1 > "$FAKE_GH_LOCK_LABEL" ;;
                    esac
                fi
            fi
        done
        ;;
    release:edit)
        for argument in "$@"; do
            case "$argument" in
                --prerelease)
                    echo true > "$FAKE_GH_PRERELEASE"
                    ;;
                --prerelease=false)
                    echo false > "$FAKE_GH_PRERELEASE"
                    ;;
                --draft=false)
                    echo public > "$FAKE_GH_STATE"
                    ;;
            esac
        done
        ;;
    *)
        echo "unsupported fake gh command: $*" >&2
        exit 2
        ;;
esac
"###,
    )
    .unwrap();
        let mut permissions = std::fs::metadata(&gh).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&gh, permissions).unwrap();

        Self {
            root: root.to_path_buf(),
            bin_dir,
            state,
            assets,
            lock_label,
            run_status,
            after_lock,
            after_archive,
            create_visibility_lag,
            label_cleanup,
            log,
        }
    }

    fn publish_command(
        &self,
        script: &std::path::Path,
        tag: &str,
        release: &std::path::Path,
    ) -> Command {
        let mut command = Command::new(script);
        command
            .arg(tag)
            .arg(release)
            .env("PATH", format!("{}:{}", self.bin_dir.display(), std::env::var("PATH").unwrap()))
            .env("FAKE_GH_STATE", &self.state)
            .env("FAKE_GH_TAG", tag)
            .env("FAKE_GH_PRERELEASE", self.root.join("gh-prerelease"))
            .env("FAKE_GH_ASSETS", &self.assets)
            .env("FAKE_GH_LOG", &self.log)
            .env("FAKE_GH_LOCK_LABEL", &self.lock_label)
            .env("FAKE_GH_RUN_STATUS", &self.run_status)
            .env("FAKE_GH_AFTER_LOCK", &self.after_lock)
            .env("FAKE_GH_AFTER_ARCHIVE", &self.after_archive)
            .env("FAKE_GH_CREATE_VISIBILITY_LAG", &self.create_visibility_lag)
            .env("FAKE_GH_LABEL_CLEANUP", &self.label_cleanup)
            .env("GITHUB_RUN_ID", "123")
            .env("GITHUB_RUN_ATTEMPT", "1");
        command
    }

    fn publish(&self, release: &std::path::Path) -> Command {
        self.publish_command(&release_script("publish-release.sh"), &package_tag(), release)
    }

    fn state(&self) -> String {
        std::fs::read_to_string(&self.state).unwrap()
    }

    fn calls(&self) -> String {
        std::fs::read_to_string(&self.log).unwrap()
    }

    fn set_lock(&self, label: &str, digest: &str) {
        std::fs::write(&self.assets, format!("SHA256SUMS\tuploaded\tsha256:{digest}\n")).unwrap();
        std::fs::write(&self.lock_label, format!("{label}\n")).unwrap();
    }

    fn set_run_status(&self, status: &str) {
        std::fs::write(&self.run_status, format!("{status}\n")).unwrap();
    }

    fn after_lock(&self, action: &str) {
        std::fs::write(&self.after_lock, format!("{action}\n")).unwrap();
    }

    fn after_archive(&self, action: &str) {
        std::fs::write(&self.after_archive, format!("{action}\n")).unwrap();
    }

    fn delay_created_release_visibility(&self, queries: u32) {
        std::fs::write(&self.create_visibility_lag, format!("{queries}\n")).unwrap();
    }

    fn fail_label_cleanup(&self) {
        std::fs::write(&self.label_cleanup, "failure\n").unwrap();
    }
}

fn sha256(path: &std::path::Path) -> String {
    let output = std::process::Command::new("sha256sum").arg(path).output().unwrap();
    assert!(output.status.success());
    String::from_utf8(output.stdout).unwrap().split_whitespace().next().unwrap().to_string()
}

#[test]
fn version_validation_accepts_the_package_version() {
    Command::new(release_script("validate-version.sh"))
        .arg(package_tag())
        .assert()
        .success()
        .stdout(format!("{}\n", env!("CARGO_PKG_VERSION")));
}

#[test]
fn version_validation_accepts_a_prerelease_package_version() {
    let temp = tempfile::tempdir().unwrap();
    let manifest = temp.path().join("Cargo.toml");
    std::fs::write(
        &manifest,
        "[package]\nname = \"tmup\"\nversion = \"1.2.3-rc.1\"\nedition = \"2024\"\n",
    )
    .unwrap();

    Command::new(release_script("validate-version.sh"))
        .args(["v1.2.3-rc.1", manifest.to_str().unwrap()])
        .assert()
        .success()
        .stdout("1.2.3-rc.1\n");
}

#[test]
fn version_validation_rejects_malformed_or_build_metadata_tags() {
    for tag in ["0.1.0", "v01.2.3", "v1.2", "v1.2.3-01", "v1.2.3-rc..1", "v0.1.0+build.1"] {
        Command::new(release_script("validate-version.sh"))
            .arg(tag)
            .assert()
            .failure()
            .stderr(predicate::str::contains("v-prefixed SemVer without build metadata"));
    }
}

#[test]
fn version_validation_rejects_a_package_version_mismatch() {
    Command::new(release_script("validate-version.sh")).arg("v999.0.0").assert().failure().stderr(
        predicate::str::contains(format!(
            "does not match Cargo package version v{}",
            env!("CARGO_PKG_VERSION")
        )),
    );
}

#[test]
fn target_validation_rejects_unknown_well_formed_targets() {
    Command::new(release_script("validate-target.sh"))
        .arg("x86_64-unknown-linux-gnu")
        .assert()
        .failure()
        .stderr(predicate::str::contains("unsupported release target"))
        .stderr(predicate::str::contains("x86_64-unknown-linux-musl"))
        .stderr(predicate::str::contains("aarch64-unknown-linux-musl"))
        .stderr(predicate::str::contains("x86_64-apple-darwin"))
        .stderr(predicate::str::contains("aarch64-apple-darwin"));
}

#[test]
fn release_target_contract_exposes_the_native_runner_matrix() {
    Command::new(release_script("release-targets.sh"))
        .arg("--github-matrix")
        .assert()
        .success()
        .stdout(concat!(
            r#"{"include":["#,
            r#"{"target":"x86_64-unknown-linux-musl","runner":"ubuntu-24.04","macos_deployment_target":""},"#,
            r#"{"target":"aarch64-unknown-linux-musl","runner":"ubuntu-24.04-arm","macos_deployment_target":""},"#,
            r#"{"target":"x86_64-apple-darwin","runner":"macos-15-intel","macos_deployment_target":"10.12"},"#,
            r#"{"target":"aarch64-apple-darwin","runner":"macos-15","macos_deployment_target":"11.0"}"#,
            "]}\n",
        ));
}

#[test]
fn installer_target_contract_is_generated_from_release_targets() {
    Command::new(release_script("sync-installer-targets.sh")).arg("--check").assert().success();
}

#[test]
fn readme_documents_the_hardened_remote_installer_command() {
    let readme =
        std::fs::read_to_string(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md"))
            .unwrap();

    assert!(readme.contains(concat!(
        "curl --proto '=https' --tlsv1.2 -LsSf ",
        "https://raw.githubusercontent.com/wfxr/tmup/main/install.sh | sh",
    )));
}

#[test]
fn local_packaging_produces_the_single_binary_archive_contract() {
    let temp = tempfile::tempdir().unwrap();
    let output_dir = temp.path().join("dist");
    let target = "x86_64-unknown-linux-musl";
    let package_name = format!("tmup-v{}-{target}", env!("CARGO_PKG_VERSION"));
    let archive = output_dir.join(format!("{package_name}.tar.gz"));

    Command::new(release_script("package.sh"))
        .arg(package_tag())
        .arg(target)
        .arg(assert_cmd::cargo::cargo_bin!("tmup"))
        .arg(&output_dir)
        .assert()
        .success()
        .stdout(format!(
            "{}\n{}\n",
            archive.display(),
            output_dir.join(format!("{package_name}.tar.xz")).display()
        ));

    for (format, list_flag, extract_flag) in [("gz", "-tzf", "-xzf"), ("xz", "-tJf", "-xJf")] {
        let archive = output_dir.join(format!("{package_name}.tar.{format}"));
        let listing = std::process::Command::new("tar")
            .args([list_flag, archive.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(listing.status.success());
        assert_eq!(
            String::from_utf8(listing.stdout).unwrap(),
            format!("{package_name}/\n{package_name}/tmup\n")
        );

        let extracted = temp.path().join(format!("extracted-{format}"));
        std::fs::create_dir(&extracted).unwrap();
        let extraction = std::process::Command::new("tar")
            .args([extract_flag, archive.to_str().unwrap(), "-C", extracted.to_str().unwrap()])
            .output()
            .unwrap();
        assert!(extraction.status.success());

        Command::new(extracted.join(&package_name).join("tmup"))
            .arg("--version")
            .assert()
            .success()
            .stdout(format!("tmup {}\n", env!("CARGO_PKG_VERSION")));
    }
}

#[test]
fn archive_validation_rejects_extra_payload_files() {
    let temp = tempfile::tempdir().unwrap();
    let target = "x86_64-unknown-linux-musl";
    let package_name = format!("tmup-v{}-{target}", env!("CARGO_PKG_VERSION"));
    let package_dir = temp.path().join(&package_name);
    let archive = temp.path().join(format!("{package_name}.tar.gz"));
    std::fs::create_dir(&package_dir).unwrap();
    std::fs::copy(assert_cmd::cargo::cargo_bin!("tmup"), package_dir.join("tmup")).unwrap();
    std::fs::write(package_dir.join("README.md"), "unexpected payload\n").unwrap();

    let packaging = std::process::Command::new("tar")
        .args([
            "-czf",
            archive.to_str().unwrap(),
            "-C",
            temp.path().to_str().unwrap(),
            &package_name,
        ])
        .output()
        .unwrap();
    assert!(packaging.status.success());

    Command::new(release_script("validate-archive.sh"))
        .arg(package_tag())
        .arg(target)
        .arg(&archive)
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!("expected only {package_name}/tmup")));
}

#[test]
fn packaging_rejects_a_binary_with_a_different_version() {
    let temp = tempfile::tempdir().unwrap();
    let output_dir = temp.path().join("dist");
    let binary = temp.path().join("tmup");
    std::fs::write(&binary, "#!/bin/sh\nprintf 'tmup 9.9.9\\n'\n").unwrap();
    let mut permissions = std::fs::metadata(&binary).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&binary, permissions).unwrap();

    Command::new(release_script("package.sh"))
        .arg(package_tag())
        .arg("x86_64-unknown-linux-musl")
        .arg(&binary)
        .arg(&output_dir)
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!(
            "binary reported 'tmup 9.9.9', expected 'tmup {}'",
            env!("CARGO_PKG_VERSION")
        )));

    assert!(
        !output_dir
            .join(format!("tmup-v{}-x86_64-unknown-linux-musl.tar.gz", env!("CARGO_PKG_VERSION")))
            .exists()
    );
}

#[test]
fn release_asset_preparation_generates_the_complete_checksum_manifest() {
    let temp = tempfile::tempdir().unwrap();
    let downloads = package_release_archives(temp.path());
    let release = temp.path().join("release");

    Command::new(release_script("prepare-assets.sh"))
        .arg(package_tag())
        .arg(&downloads)
        .arg(&release)
        .assert()
        .success();

    let mut asset_names = std::fs::read_dir(&release)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    asset_names.sort();

    let version = env!("CARGO_PKG_VERSION");
    let targets = release_targets();
    let mut expected_names = targets
        .iter()
        .flat_map(|target| {
            ["gz", "xz"].map(|format| format!("tmup-v{version}-{target}.tar.{format}"))
        })
        .chain(std::iter::once("SHA256SUMS".to_string()))
        .collect::<Vec<_>>();
    expected_names.sort();
    assert_eq!(asset_names, expected_names);

    let checksum_manifest = std::fs::read_to_string(release.join("SHA256SUMS")).unwrap();
    let checksum_names = checksum_manifest
        .lines()
        .map(|line| line.split_whitespace().nth(1).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        checksum_names,
        targets
            .iter()
            .flat_map(|target| ["gz", "xz"]
                .map(|format| format!("tmup-v{version}-{target}.tar.{format}")))
            .collect::<Vec<_>>()
    );

    let verification = std::process::Command::new("sha256sum")
        .arg("--check")
        .arg("SHA256SUMS")
        .current_dir(&release)
        .output()
        .unwrap();
    assert!(
        verification.status.success(),
        "generated checksum manifest should verify: {}",
        String::from_utf8_lossy(&verification.stderr)
    );
}

#[test]
fn release_asset_preparation_does_not_execute_native_binaries() {
    let temp = tempfile::tempdir().unwrap();
    let downloads = package_unrunnable_release_archives(temp.path());
    let release = temp.path().join("release");

    Command::new(release_script("prepare-assets.sh"))
        .arg(package_tag())
        .arg(downloads)
        .arg(&release)
        .assert()
        .success();

    assert!(release.join("SHA256SUMS").is_file());
}

#[test]
fn release_asset_preparation_rejects_an_incomplete_archive_set() {
    let temp = tempfile::tempdir().unwrap();
    let downloads = package_release_archives(temp.path());
    let missing_name = format!("tmup-v{}-aarch64-apple-darwin.tar.gz", env!("CARGO_PKG_VERSION"));
    std::fs::remove_file(downloads.join(&missing_name)).unwrap();
    let release = temp.path().join("release");

    Command::new(release_script("prepare-assets.sh"))
        .arg(package_tag())
        .arg(&downloads)
        .arg(&release)
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!("missing asset: {missing_name}")));

    assert!(!release.exists(), "failed preparation must not leave a partial asset directory");
}

#[test]
fn release_asset_preparation_rejects_a_missing_xz_archive() {
    let temp = tempfile::tempdir().unwrap();
    let downloads = package_release_archives(temp.path());
    let missing_name = format!("tmup-v{}-aarch64-apple-darwin.tar.xz", env!("CARGO_PKG_VERSION"));
    std::fs::remove_file(downloads.join(&missing_name)).unwrap();
    let release = temp.path().join("release");

    Command::new(release_script("prepare-assets.sh"))
        .arg(package_tag())
        .arg(&downloads)
        .arg(&release)
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!("missing asset: {missing_name}")));

    assert!(!release.exists(), "failed preparation must not leave a partial asset directory");
}

#[test]
fn release_tag_validation_accepts_lightweight_and_annotated_tags() {
    for tag_kind in ["lightweight", "annotated"] {
        let repo = release_tag_repo(tag_kind);
        let commit = git(repo.path(), &["rev-parse", "HEAD"]);

        Command::new(release_script("validate-release-tag.sh"))
            .arg(package_tag())
            .arg(&commit)
            .current_dir(repo.path())
            .assert()
            .success()
            .stdout(format!("{}\n", env!("CARGO_PKG_VERSION")));
    }
}

#[test]
fn release_tag_validation_rejects_a_commit_outside_origin_main() {
    let repo = release_tag_repo("lightweight");
    let tag = package_tag();
    git(repo.path(), &["tag", "--delete", &tag]);
    std::fs::write(repo.path().join("branch-only"), "unmerged\n").unwrap();
    git(repo.path(), &["add", "branch-only"]);
    git(repo.path(), &["commit", "--quiet", "-m", "unmerged release"]);
    git(repo.path(), &["tag", &tag]);
    let commit = git(repo.path(), &["rev-parse", "HEAD"]);

    Command::new(release_script("validate-release-tag.sh"))
        .arg(&tag)
        .arg(&commit)
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!("{tag} is not reachable from origin/main")));
}

#[test]
fn release_tag_validation_rejects_a_tag_for_a_different_workflow_commit() {
    let repo = release_tag_repo("lightweight");
    std::fs::write(repo.path().join("later-main"), "later\n").unwrap();
    git(repo.path(), &["add", "later-main"]);
    git(repo.path(), &["commit", "--quiet", "-m", "advance main"]);
    git(repo.path(), &["update-ref", "refs/remotes/origin/main", "HEAD"]);
    let workflow_commit = git(repo.path(), &["rev-parse", "HEAD"]);

    Command::new(release_script("validate-release-tag.sh"))
        .arg(package_tag())
        .arg(&workflow_commit)
        .current_dir(repo.path())
        .assert()
        .failure()
        .stderr(predicate::str::contains(format!("instead of workflow commit {workflow_commit}")));
}

#[test]
fn release_publication_stages_assets_before_making_the_release_public() {
    let temp = tempfile::tempdir().unwrap();
    let release = prepare_release_assets(temp.path());
    let fake_gh = FakeGh::new(temp.path(), "missing");

    fake_gh.publish(&release).assert().success();

    assert_eq!(fake_gh.state(), "public\n");
    let calls = fake_gh.calls();
    let create = calls.find("release create").expect("release should be created as a draft");
    let upload = calls.find("release upload").expect("release assets should be uploaded");
    let publish =
        calls.rfind("release edit").expect("draft should be published only after verification");
    assert!(create < upload && upload < publish, "unexpected publication order:\n{calls}");
    assert!(calls[create..upload].contains("--draft"));
    assert!(calls[create..upload].contains("--generate-notes"));
    assert!(calls[publish..].contains("--draft=false"));
    for target in release_targets() {
        for format in ["gz", "xz"] {
            let name = format!("tmup-v{}-{target}.tar.{format}", env!("CARGO_PKG_VERSION"));
            assert!(calls[upload..publish].contains(&name), "archive was not uploaded: {name}");
        }
    }
}

#[test]
fn release_publication_clears_the_internal_lock_label_after_publishing() {
    let temp = tempfile::tempdir().unwrap();
    let release = prepare_release_assets(temp.path());
    let fake_gh = FakeGh::new(temp.path(), "missing");

    fake_gh.publish(&release).assert().success();

    assert_eq!(std::fs::read_to_string(&fake_gh.lock_label).unwrap(), "\n");
    let calls = fake_gh.calls();
    let publish = calls.find("release edit").expect("draft should be published");
    let cleanup = calls
        .find("api --method PATCH repos/{owner}/{repo}/releases/assets/1 --raw-field label=")
        .expect("internal lock label should be cleared");
    assert!(publish < cleanup, "label was cleared before publication:\n{calls}");
}

#[test]
fn release_publication_warns_if_the_public_lock_label_cannot_be_cleared() {
    let temp = tempfile::tempdir().unwrap();
    let release = prepare_release_assets(temp.path());
    let fake_gh = FakeGh::new(temp.path(), "missing");
    fake_gh.fail_label_cleanup();

    fake_gh.publish(&release).assert().success().stderr(predicate::str::contains(format!(
        "warning: published {}, but SHA256SUMS retains its internal publication label",
        package_tag()
    )));

    assert_eq!(fake_gh.state(), "public\n");
    assert_eq!(
        std::fs::read_to_string(&fake_gh.lock_label).unwrap(),
        "tmup-publication-run-123-attempt-1\n"
    );
}

#[test]
fn release_publication_waits_for_a_created_draft_to_become_visible() {
    let temp = tempfile::tempdir().unwrap();
    let release = prepare_release_assets(temp.path());
    let fake_gh = FakeGh::new(temp.path(), "missing");
    fake_gh.delay_created_release_visibility(1);

    fake_gh.publish(&release).assert().success();

    assert_eq!(fake_gh.state(), "public\n");
}

#[test]
fn release_publication_does_not_execute_native_binaries() {
    let temp = tempfile::tempdir().unwrap();
    let release = prepare_unrunnable_release_assets(temp.path());
    let fake_gh = FakeGh::new(temp.path(), "missing");

    fake_gh.publish(&release).assert().success();

    assert_eq!(fake_gh.state(), "public\n");
}

#[test]
fn release_publication_repairs_the_existing_draft() {
    let temp = tempfile::tempdir().unwrap();
    let release = prepare_release_assets(temp.path());
    let fake_gh = FakeGh::new(temp.path(), "draft");
    std::fs::write(
        temp.path().join("gh-assets"),
        "stale.txt\tuploaded\tsha256:0000000000000000000000000000000000000000000000000000000000000000\n",
    )
    .unwrap();

    fake_gh.publish(&release).assert().success();

    assert_eq!(fake_gh.state(), "public\n");
    let calls = fake_gh.calls();
    assert!(!calls.contains("release create"), "rerun should reuse the draft:\n{calls}");
    assert!(calls.contains("release delete-asset"));
    assert!(calls.contains("release upload"));
    assert!(calls.contains("--clobber"));
}

#[test]
fn release_publication_refuses_to_mutate_a_public_release() {
    let temp = tempfile::tempdir().unwrap();
    let release = prepare_release_assets(temp.path());
    let fake_gh = FakeGh::new(temp.path(), "public");

    fake_gh.publish(&release).assert().failure().stderr(predicate::str::contains(format!(
        "a public release already exists for {}",
        package_tag()
    )));

    assert_eq!(fake_gh.state(), "public\n");
    let calls = fake_gh.calls();
    assert!(!calls.contains("release upload"));
    assert!(!calls.contains("release edit"));
}

#[test]
fn release_publication_refuses_to_race_an_active_tag_run() {
    let temp = tempfile::tempdir().unwrap();
    let release = prepare_release_assets(temp.path());
    let fake_gh = FakeGh::new(temp.path(), "draft");
    fake_gh.set_lock(
        "tmup-publication-run-999-attempt-1",
        "0000000000000000000000000000000000000000000000000000000000000000",
    );

    fake_gh
        .publish(&release)
        .assert()
        .failure()
        .stderr(predicate::str::contains("publication run 999 is still active"));

    assert_eq!(fake_gh.state(), "draft\n");
    let calls = fake_gh.calls();
    assert!(!calls.contains("release upload"));
    assert!(!calls.contains("--draft=false"));
}

#[test]
fn release_publication_takes_over_a_completed_run_lock() {
    let temp = tempfile::tempdir().unwrap();
    let release = prepare_release_assets(temp.path());
    let fake_gh = FakeGh::new(temp.path(), "draft");
    fake_gh.set_lock(
        "tmup-publication-run-999-attempt-1",
        "0000000000000000000000000000000000000000000000000000000000000000",
    );
    fake_gh.set_run_status("completed");

    fake_gh.publish(&release).assert().success();

    assert_eq!(fake_gh.state(), "public\n");
    let calls = fake_gh.calls();
    assert!(calls.contains("actions/runs/999"));
    assert!(calls.contains("release delete-asset"));
    assert!(calls.contains("release upload"));
}

#[test]
fn release_publication_resumes_an_owned_attempt_without_replacing_its_lock() {
    let temp = tempfile::tempdir().unwrap();
    let release = prepare_release_assets(temp.path());
    let fake_gh = FakeGh::new(temp.path(), "draft");
    fake_gh.set_lock("tmup-publication-run-123-attempt-1", &sha256(&release.join("SHA256SUMS")));

    fake_gh.publish(&release).assert().success();

    assert_eq!(fake_gh.state(), "public\n");
    let calls = fake_gh.calls();
    assert!(!calls.contains(&format!("release delete-asset {} SHA256SUMS", package_tag())));
    assert!(!calls.contains("SHA256SUMS#tmup-publication"));
}

#[test]
fn release_publication_reacquires_a_lock_from_an_earlier_attempt() {
    let temp = tempfile::tempdir().unwrap();
    let release = prepare_release_assets(temp.path());
    let fake_gh = FakeGh::new(temp.path(), "draft");
    fake_gh.set_lock(
        "tmup-publication-run-123-attempt-0",
        "0000000000000000000000000000000000000000000000000000000000000000",
    );

    fake_gh.publish(&release).assert().success();

    assert_eq!(fake_gh.state(), "public\n");
    let calls = fake_gh.calls();
    assert!(calls.contains(&format!("release delete-asset {} SHA256SUMS", package_tag())));
    assert!(!calls.contains("actions/runs/123"));
    assert!(calls.contains("SHA256SUMS#tmup-publication-run-123-attempt-1"));
}

#[test]
fn release_publication_rejects_a_malformed_draft_owner() {
    let temp = tempfile::tempdir().unwrap();
    let release = prepare_release_assets(temp.path());
    let fake_gh = FakeGh::new(temp.path(), "draft");
    fake_gh.set_lock(
        "manually-uploaded",
        "0000000000000000000000000000000000000000000000000000000000000000",
    );

    fake_gh
        .publish(&release)
        .assert()
        .failure()
        .stderr(predicate::str::contains("no valid publication owner"));

    assert_eq!(fake_gh.state(), "draft\n");
    let calls = fake_gh.calls();
    assert!(!calls.contains("release upload"));
    assert!(!calls.contains("--draft=false"));
}

#[test]
fn release_publication_fails_if_atomic_lock_acquisition_is_lost() {
    let temp = tempfile::tempdir().unwrap();
    let release = prepare_release_assets(temp.path());
    let fake_gh = FakeGh::new(temp.path(), "draft");
    fake_gh.after_lock("corrupt-lock");

    fake_gh
        .publish(&release)
        .assert()
        .failure()
        .stderr(predicate::str::contains("failed to acquire draft publication ownership"));

    assert_eq!(fake_gh.state(), "draft\n");
    let calls = fake_gh.calls();
    assert!(!calls.contains("--clobber"));
    assert!(!calls.contains("--draft=false"));
}

#[test]
fn release_publication_stops_if_the_draft_disappears_after_locking() {
    let temp = tempfile::tempdir().unwrap();
    let release = prepare_release_assets(temp.path());
    let fake_gh = FakeGh::new(temp.path(), "draft");
    fake_gh.after_lock("publish");

    fake_gh.publish(&release).assert().failure().stderr(predicate::str::contains(format!(
        "release {} is no longer a draft",
        package_tag()
    )));

    assert_eq!(fake_gh.state(), "public\n");
    let calls = fake_gh.calls();
    assert!(!calls.contains("--clobber"));
}

#[test]
fn release_publication_stops_if_ownership_changes_before_publish() {
    let temp = tempfile::tempdir().unwrap();
    let release = prepare_release_assets(temp.path());
    let fake_gh = FakeGh::new(temp.path(), "draft");
    fake_gh.after_archive("corrupt-lock");

    fake_gh
        .publish(&release)
        .assert()
        .failure()
        .stderr(predicate::str::contains("draft publication ownership changed before publish"));

    assert_eq!(fake_gh.state(), "draft\n");
    let calls = fake_gh.calls();
    assert!(calls.contains("--clobber"));
    assert!(!calls.contains("--draft=false"));
}

#[test]
fn release_publication_marks_prereleases_without_replacing_latest() {
    let temp = tempfile::tempdir().unwrap();
    let version = "0.1.0-rc.1";
    let tag = format!("v{version}");
    let scripts = isolated_release_scripts(temp.path(), version);
    let downloads = package_release_archives_with(temp.path(), &scripts, version);
    let release = temp.path().join("release");
    Command::new(scripts.join("prepare-assets.sh"))
        .arg(&tag)
        .arg(&downloads)
        .arg(&release)
        .assert()
        .success();
    let fake_gh = FakeGh::new(temp.path(), "missing");

    fake_gh.publish_command(&scripts.join("publish-release.sh"), &tag, &release).assert().success();

    assert_eq!(std::fs::read_to_string(temp.path().join("gh-prerelease")).unwrap(), "true\n");
    let calls = fake_gh.calls();
    let create = calls.find("release create").unwrap();
    let upload = calls.find("release upload").unwrap();
    let publish = calls.rfind("release edit").unwrap();
    assert!(calls[create..upload].contains("--prerelease"));
    assert!(calls[create..upload].contains("--latest=false"));
    assert!(calls[publish..].contains("--prerelease"));
    assert!(calls[publish..].contains("--latest=false"));
}
