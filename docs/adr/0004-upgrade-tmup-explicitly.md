---
status: accepted
---

# Upgrade tmup through an explicit command

Users need one command to replace an installed tmup with a published release. A tmup Upgrade is separate from a Plugin Update: `tmup upgrade` replaces the program, while `tmup update [id]` continues to advance remote plugin revisions. This decision records the behavior of the explicit upgrade command.

## Decision

Only an explicit `tmup upgrade` checks for or installs a tmup release. Ordinary commands and Init Sessions never perform this work. The command does not read plugin configuration, reconcile `tmup.lock`, or mutate managed plugin state. Invalid or missing plugin configuration cannot prevent a tmup Upgrade.

The command accepts `--pre`, `--version <VERSION>`, and `--force`. `--pre` and `--version` are mutually exclusive. Version selection and permission to replace an installation are separate decisions:

| Invocation | Release selection | Replacement behavior |
| --- | --- | --- |
| `tmup upgrade` | Latest stable release | Replace only when the selected version is newer |
| `tmup upgrade --pre` | Include prereleases in release selection | Replace only when the selected version is newer |
| `tmup upgrade --version VERSION` | Exact published version | Allow an upgrade or downgrade; skip the same version |
| Any selection with `--force` | Preserve the selected version and channel | Also replace the same version and permit takeover of a non-official build |

Latest follows the installer's GitHub release selection: the latest stable endpoint by default, or the first non-draft published release when `--pre` is set. It does not mean scanning every release for the highest SemVer. Version comparison determines whether the selected release can replace the installed version.

An implicit selection never downgrades, including with `--force`. A user must name the lower version with `--version`. A same-version forced replacement downloads the release again, so it also handles a release binary being replaced without a version change. A normal no-op succeeds and explains why no replacement occurred.

Official release builds embed an origin marker. An unmarked build refuses replacement, reports the reason, and points to `--force`. A forced takeover warns that the official binary replaces custom build choices and can disagree with another installer's records. It does not prompt for confirmation. The marker prevents accidental takeover; it is not an authenticity proof and cannot identify package managers that distribute the official binary unchanged.

The destination is the real filesystem path of the running executable. Existing symbolic links to it remain intact. The command does not select a destination by searching `PATH` or by assuming the installer's default directory. Unsupported platforms and insufficient filesystem permissions are errors; `--force` does not override these checks or invoke privilege escalation.

Fetch `https://raw.githubusercontent.com/wfxr/tmup/main/install.sh` once into a private temporary file and execute it only after the transfer succeeds. Reuse that script snapshot for version resolution and installation into a private staging directory. Rust owns origin checks, version policy, destination locking, candidate checks, and final publication. Installer fixes can therefore reach existing clients without a new tmup release. Exact version selection pins the target binary, while the installer helper always comes from current `main`.

The command downloads through HTTPS with normal certificate validation. It does not fetch or verify `SHA256SUMS`, compare release digests, or verify artifact attestations. This keeps upgrade aligned with the release and installer workflows, which also omit checksum manifests. Archive decoding, expected layout, entry-type checks, and safe file handling remain required.

Network requests retry only transient failures, with at most three attempts and one- and two-second waits. Retries remain inside finite phase deadlines; version queries and staging installation cannot hold the destination lock indefinitely. Timeouts terminate the helper process group before cleanup. Do not retry whole installer invocations or final publication.

Downloads and extraction use a private system temporary workspace, honoring `TMPDIR`. Once preparation succeeds, copy the candidate into a temporary file beside the real executable, check that copied candidate, and atomically replace the destination on the same filesystem. Never copy directly over the installed executable or depend on renaming from `/tmp` across filesystems. Failures before replacement preserve the old program. Updates of the same destination use an exclusive lock; concurrent attempts fail instead of waiting or overwriting each other. The command keeps no version history or dedicated rollback command. A user can request an older published release through `--version`.

The temporary workspace may be mounted `noexec`. Installer validation there checks file type and executable permission bits, while actual execution is tested only on the copied candidate beside the destination. Neither the script invocation nor extraction requires directly executing a file from the temporary mount.

The operation is noninteractive and reports failures through a non-zero exit status. Successful output identifies the installed version and destination. A successful replacement affects later invocations; it does not restart tmux or reconfigure plugins.

## Considered options

The `self upgrade` command hierarchy adds a level without another self-management command to group with it. The shorter `upgrade` command keeps the existing plugin-specific `update` contract and requires help text to name the object being upgraded explicitly.

Automatic checks during `init` would add release-service availability and mutable program state to deterministic startup. They are excluded even if background update checks become convenient later.

Rejecting every non-official installation would prevent intentional migration from Cargo or source builds. Unconditional takeover would discard custom builds unexpectedly. The origin marker and `--force` make that choice explicit without claiming universal package-manager integration.

Checksum manifests were considered and deliberately omitted at the user's request. Do not reintroduce them as an implicit dependency of the upgrade path. Existing uses of SHA-256 for plugin identities, fingerprints, and failure markers are unrelated to this decision.

## Consequences

The implementation needs its own destination lock and staging lifecycle. It must not reuse plugin configuration discovery or a plugin-state lock to guard executable replacement. Statements about validating native configuration before lifecycle work apply to commands that read plugin configuration; they do not require `upgrade` to load it.

The first release containing this command must also carry the official-build marker in tag-release artifacts. Source builds remain unmarked by default, and the existing `tmup X.Y.Z` version output remains unchanged. Later dependency choices must preserve the published Linux MUSL and macOS targets.

The installer needs a small stable interface for normalized version queries and silent staging. Add that interface to `main` before releasing the first upgrade-capable client, and preserve it through focused regression tests. There is no protocol negotiation, historical-client test matrix, embedded fallback, or persistent helper cache.

Only retained published releases can be installed. A deleted release or missing platform asset is an error when installation is attempted; tmup does not reconstruct removed releases, search historical scripts, or fall back to another version. An exact same-version request without force remains a successful no-op without checking asset availability. The maintainer has removed releases before v0.3.0, so those versions require no compatibility implementation. Availability, rather than a hardcoded minimum version, determines whether an exact version can be installed.

See the [command reference](../commands.md#upgrade) for usage and operational limits.
