use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use semver::Version;

use super::{Options, process, release_version};

const SCRIPT_URL: &str = "https://raw.githubusercontent.com/wfxr/tmup/main/install.sh";

pub(super) fn download(workspace: &Path, timeout: Duration) -> Result<PathBuf> {
    let downloader = find_downloader()?;
    download_with(&downloader, workspace, timeout, Duration::from_secs(1))
}

fn find_downloader() -> Result<PathBuf> {
    for name in ["curl", "wget"] {
        if let Some(path) = std::env::var_os("PATH").and_then(|paths| {
            std::env::split_paths(&paths).map(|dir| dir.join(name)).find(|path| {
                path.metadata()
                    .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
            })
        }) {
            return Ok(path);
        }
    }
    bail!("tmup upgrade requires curl or wget on PATH")
}

fn download_with(
    downloader: &Path,
    workspace: &Path,
    timeout: Duration,
    backoff: Duration,
) -> Result<PathBuf> {
    let script = workspace.join("install.sh");
    let deadline = Instant::now() + timeout;
    let curl = downloader.file_name().is_some_and(|name| name == "curl");
    for attempt in 0..3 {
        let mut url = SCRIPT_URL.to_owned();
        let mut redirects = 0;
        let output = loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            ensure!(!remaining.is_zero(), "installer download timed out");
            fs::write(&script, []).context("cannot reset installer download")?;
            let mut command = Command::new(downloader);
            if curl {
                command
                    .args([
                        "--disable",
                        "--fail",
                        "--silent",
                        "--show-error",
                        "--location",
                        "--proto",
                        "=https",
                        "--proto-redir",
                        "=https",
                        "--retry",
                        "0",
                        "--connect-timeout",
                        "10",
                        "--max-time",
                    ])
                    .arg(remaining.as_secs_f64().min(60.0).to_string())
                    .args(["--write-out", "%{http_code}", "--output"])
                    .arg(&script)
                    .arg(&url);
            } else {
                command
                    .args([
                        "--no-config",
                        "--https-only",
                        "--server-response",
                        "--max-redirect=0",
                        "--tries=1",
                        "--dns-timeout=10",
                        "--connect-timeout=10",
                        "--read-timeout=60",
                        "--no-hsts",
                        "--output-document",
                    ])
                    .arg(&script)
                    .arg(&url)
                    .env("LC_ALL", "C");
            }
            let output = process::run(&mut command, workspace, remaining, "installer download")?;
            let status = http_status(&output, curl);
            if !curl
                && output.status.code() == Some(8)
                && matches!(status, Some(301 | 302 | 303 | 307 | 308))
            {
                ensure!(redirects < 10, "installer download exceeded redirect limit");
                url = redirect_url(&url, &output.stderr)?;
                redirects += 1;
                continue;
            }
            break output;
        };
        if output.status.success() {
            ensure!(fs::metadata(&script)?.len() > 0, "downloaded installer is empty");
            return Ok(script);
        }
        let retry = retryable(curl, output.status.code(), http_status(&output, curl));
        if !retry || attempt == 2 {
            bail!("installer download failed ({}): {}", output.status, output.stderr.trim());
        }
        let delay = backoff * (attempt + 1);
        ensure!(
            deadline.saturating_duration_since(Instant::now()) > delay,
            "installer download timed out during retries"
        );
        std::thread::sleep(delay);
    }
    unreachable!()
}

fn http_status(output: &process::Output, curl: bool) -> Option<u16> {
    if curl {
        output.stdout.trim().parse().ok()
    } else {
        output
            .stderr
            .lines()
            .filter_map(|line| {
                let mut fields = line.split_whitespace();
                fields.next().filter(|field| field.starts_with("HTTP/"))?;
                fields.next()?.parse().ok()
            })
            .next_back()
    }
}

fn retryable(curl: bool, code: Option<i32>, http: Option<u16>) -> bool {
    if (curl && code == Some(22)) || (!curl && code == Some(8)) {
        return matches!(http, Some(408 | 429 | 500 | 502 | 503 | 504));
    }
    if curl {
        matches!(code, Some(5 | 6 | 7 | 18 | 28 | 52 | 55 | 56 | 92))
    } else {
        code == Some(4)
    }
}

fn redirect_url(current: &str, headers: &str) -> Result<String> {
    let location = headers
        .lines()
        .filter_map(|line| {
            let (name, value) = line.trim().split_once(':')?;
            name.eq_ignore_ascii_case("location")
                .then(|| value.trim().trim_end_matches(" [following]").trim())
        })
        .next_back()
        .context("installer redirect has no Location")?;
    ensure!(!location.chars().any(char::is_whitespace), "invalid installer redirect");
    if location.starts_with("https://") {
        return Ok(location.to_owned());
    }
    if location.starts_with('/') && !location.starts_with("//") {
        let authority = current.strip_prefix("https://").unwrap().split('/').next().unwrap();
        return Ok(format!("https://{authority}{location}"));
    }
    bail!("installer redirect must use HTTPS: {location}")
}

pub(super) fn resolve(
    script: &Path,
    workspace: &Path,
    options: &Options,
    timeout: Duration,
) -> Result<Version> {
    let mut command = Command::new("/bin/sh");
    command.arg(script).arg("--resolve-version").env("TMPDIR", workspace);
    if let Some(version) = &options.version {
        command.arg("--version").arg(version);
    } else if options.pre {
        command.arg("--pre");
    }
    let output = process::run(&mut command, workspace, timeout, "installer version query")?
        .require_success("installer version query")?;
    parse_resolution(&output.stdout)
}

fn parse_resolution(output: &str) -> Result<Version> {
    let line = output
        .strip_suffix('\n')
        .context("installer version query must return one version line")?;
    let version = release_version(line)?;
    ensure!(
        line == version.to_string(),
        "installer version query did not return a normalized version"
    );
    Ok(version)
}

pub(super) fn prepare(
    script: &Path,
    workspace: &Path,
    version: &Version,
    target: &str,
    timeout: Duration,
) -> Result<()> {
    let directory = workspace.join("prepared");
    fs::create_dir(&directory)?;
    let mut command = Command::new("/bin/sh");
    command
        .arg(script)
        .args(["--version", &version.to_string(), "--target", target, "--to"])
        .arg(directory)
        .arg("--quiet")
        .env("TMPDIR", workspace);
    process::run(&mut command, workspace, timeout, "installer preparation")?
        .require_success("installer preparation")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn bootstrap_retries_reset_partial_output_and_stop_at_budget() {
        for (name, failure_code, status_text) in
            [("curl", 28, "printf 000"), ("wget", 4, "printf 'network failed\\n' >&2")]
        {
            for (failures, expected_attempts, succeeds) in [(2, 3, true), (3, 3, false)] {
                let dir = tempfile::tempdir().unwrap();
                let downloader = dir.path().join(name);
                let counter = dir.path().join("count");
                fs::write(&downloader, format!(r#"#!/bin/sh
count=$(cat '{counter}' 2>/dev/null || printf 0)
count=$((count + 1))
printf '%s' "$count" > '{counter}'
for arg do
  if [ "${{previous:-}}" = --output ] || [ "${{previous:-}}" = --output-document ]; then out=$arg; fi
  previous=$arg
done
if [ "$count" -le {failures} ]; then
  printf 'partial must never execute' > "$out"
  {status_text}
  exit {failure_code}
fi
[ ! -s "$out" ] || exit 23
printf '#!/bin/sh\nexit 0\n' > "$out"
[ '{name}' != curl ] || printf 200
"#, counter=counter.display())).unwrap();
                fs::set_permissions(&downloader, fs::Permissions::from_mode(0o755)).unwrap();
                let result = download_with(
                    &downloader,
                    dir.path(),
                    Duration::from_secs(2),
                    Duration::from_millis(5),
                );
                assert_eq!(result.is_ok(), succeeds, "{name}: {result:?}");
                assert_eq!(fs::read_to_string(counter).unwrap(), expected_attempts.to_string());
                if let Ok(script) = result {
                    assert_eq!(fs::read_to_string(script).unwrap(), "#!/bin/sh\nexit 0\n");
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let downloader = dir.path().join("curl");
        fs::write(&downloader, "#!/bin/sh\nsleep 10\n").unwrap();
        fs::set_permissions(&downloader, fs::Permissions::from_mode(0o755)).unwrap();
        let error = download_with(
            &downloader,
            dir.path(),
            Duration::from_millis(30),
            Duration::from_millis(1),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        fs::write(&downloader, "#!/bin/sh\nprintf 000\nexit 28\n").unwrap();
        let error = download_with(
            &downloader,
            dir.path(),
            Duration::from_millis(30),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(error.to_string().contains("timed out during retries"));
    }

    #[test]
    fn strict_version_output_and_network_errors() {
        for invalid in
            ["", "0.3.1", "v0.3.1\n", "0.3.1\n\n", " 0.3.1\n", "0.3.1+build\n", "0.3.1\r\n"]
        {
            assert!(parse_resolution(invalid).is_err(), "{invalid:?}");
        }
        assert_eq!(parse_resolution("0.3.1-rc.1\n").unwrap().to_string(), "0.3.1-rc.1");
        for status in [408, 429, 500, 502, 503, 504] {
            assert!(retryable(true, Some(22), Some(status)));
        }
        for code in [23, 26, 60, 77] {
            assert!(!retryable(true, Some(code), None));
        }
        assert!(!retryable(true, Some(22), Some(404)));
        assert!(retryable(false, Some(4), None));
        assert!(!retryable(false, Some(5), None));
        assert!(redirect_url(SCRIPT_URL, "Location: http://example.com").is_err());
        assert_eq!(
            redirect_url(SCRIPT_URL, "Location: /other").unwrap(),
            "https://raw.githubusercontent.com/other"
        );
    }
}
