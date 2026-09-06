# Install tmup

This guide covers the supported installation methods, release targets, and installer options. The remote installer is the shortest path for most users; crates.io and manual asset installation are available when you need a different workflow.

tmup requires `tmux` and `git` at runtime. Configurations that use shell predicates, remote build commands, or plugin-scoped bindings also require `/bin/sh`. The remote installer additionally requires `tar` and either `curl` or `wget`.

## Install the latest stable release

The repository-owned installer downloads the latest stable GitHub release for your host. It prefers `.tar.xz` when a working `xz` command is available. If the xz asset returns HTTP 404, it downloads `.tar.gz` instead, supporting older releases that only provide gzip. Without a working `xz` command, it downloads `.tar.gz` directly. You don't need to install an extra decompression tool.

The installer checks the archive layout and installs the executable to `~/.local/bin` by default. Other download errors, including connection failures and HTTP errors other than the xz asset returning 404, stop installation. Decompression, layout, or extraction failures also stop installation without trying another format or replacing an existing binary.

Run the installer over HTTPS:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/wfxr/tmup/main/install.sh | sh
```

The installer doesn't replace an existing `tmup` executable unless you pass `--force`. Make sure the installation directory is in the `PATH` inherited by both your shell and the tmux server.

## Choose an installer release

Without a version option, the installer follows GitHub's latest stable release. You can instead select one exact version or include prereleases when choosing the latest published release.

Install one exact stable or prerelease version:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/wfxr/tmup/main/install.sh |
  sh -s -- --version 0.3.0
```

Install the latest published release, including a prerelease when it is newer:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/wfxr/tmup/main/install.sh |
  sh -s -- --pre
```

`--version` accepts a version with or without a leading `v`. `--include-prerelease` and its `--pre` alias can't be combined with an exact version.

## Installer options

Pass installer options after `sh -s --` when you use the piped command. These options let you choose a release, target, or destination without editing the script.

| Option | Behavior |
|--------|----------|
| `--version <VERSION>` | Install an exact stable or prerelease version; omit it to select the latest stable release |
| `--include-prerelease`, `--pre` | Select the latest published release, including prereleases |
| `--target <TARGET>` | Override host detection with a supported target triple |
| `--to <DIRECTORY>` | Install to a directory other than `~/.local/bin` |
| `--force` | Replace an existing `tmup` executable at the destination |
| `--help` | Print usage and exit without installing |

For example, install to a private bin directory and replace an existing copy:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://raw.githubusercontent.com/wfxr/tmup/main/install.sh |
  sh -s -- --to "$HOME/bin" --force
```

The installer downloads and extracts into a temporary directory. It stages the executable at the destination and replaces an existing binary only after all archive checks and staging succeed.

## Supported targets

Pre-built releases cover four 64-bit Linux and macOS targets. Each target is published as both `.tar.gz` and `.tar.xz`. The xz archives use ordinary LZMA2 compression without architecture-specific filters. Linux archives use MUSL so the same target works across common distributions.

| Host | Release target |
|------|----------------|
| Linux x86-64 | `x86_64-unknown-linux-musl` |
| Linux ARM64 | `aarch64-unknown-linux-musl` |
| macOS Intel | `x86_64-apple-darwin` |
| macOS Apple Silicon | `aarch64-apple-darwin` |

An unsupported host is a hard error. The installer prints the supported targets and doesn't fall back to building from source.

## Install from crates.io

The crates.io package is useful when a Rust toolchain is already installed. It builds tmup locally rather than downloading a pre-built release archive.

Install the latest stable package and use its published lockfile:

```sh
cargo install tmup --locked
```

To install one exact package version, add `--version`:

```sh
cargo install tmup --version 0.3.0 --locked
```

## Install from a source checkout

A source checkout is useful for development or for testing an unreleased commit. It requires the Rust toolchain and the repository's build dependencies.

From the repository root, run:

```sh
cargo install --path . --locked
```

This command installs the checked-out source, not the latest published release.

## Install a release asset manually

Manual installation lets you download and extract a release archive yourself. The example below uses tmup `0.3.0` for Linux x86-64; change both variables for the release and host you need.

### Download the archive

Choose the archive name, then download it:

```sh
version=0.3.0
target=x86_64-unknown-linux-musl
archive_dir="tmup-v${version}-${target}"
archive="${archive_dir}.tar.gz"
release_url="https://github.com/wfxr/tmup/releases/download/v${version}"

curl --fail --location --remote-name "${release_url}/${archive}"
```

### Install the executable

Extract the versioned directory and copy its single executable to a directory in your `PATH`:

```sh
tar -xzf "${archive}"
mkdir -p "${HOME}/.local/bin"
install -m 755 "${archive_dir}/tmup" "${HOME}/.local/bin/tmup"
```

Confirm the installed version:

```sh
tmup --version
```

## Next steps

After installation, follow the [quick start](../README.md#quick-start) to add `tmup init` to tmux. Use the [command reference](commands.md) when you need to inspect paths, logs, or a failed operation.
