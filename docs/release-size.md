# Linux musl release-size experiment

On this source snapshot, `opt-level = "z"` plus Zopfli reduces gzip download bytes by 27.57% on x86_64 and 24.49% on ARM64. Combining the same build setting with plain xz reduces download bytes by about 42% on both architectures, but requires an available xz decoder. The experiment changes no dependencies, application code, or production release configuration. Runtime performance has not been benchmarked, so the build candidates need performance validation before release.

## Scope and reproducibility

Measurements were collected on September 6, 2026, from commit `829132c033ab936a43962359416fd184f9273f12` (tmup 0.3.1), with the existing lockfile. These are local builds of that source snapshot, not downloaded GitHub release artifacts.

- Rust: installed `stable`, rustc 1.92.0 (`ded5c06cf`, LLVM 21.1.3). This is the locally installed toolchain, not a claim about the latest stable release. CI installs a moving stable toolchain, so future CI bytes can differ.
- Targets: `x86_64-unknown-linux-musl` and `aarch64-unknown-linux-musl`. Both use the release profile from the measured commit: full LTO, one codegen unit, and stripped symbols. Baseline optimization is level 3 with unwinding.
- Host: x86_64 Linux. x86_64 uses the default linker. ARM64 uses `CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld` for cross-compilation; production CI builds ARM64 natively. ARM64 execution was not tested because this host has neither an ARM64 runner nor QEMU.
- Compression: GNU tar 1.35, gzip 1.14-modified, xz 5.8.3, zstd 1.5.7, and Python Zopfli 0.4.1 with 15 iterations. Local gzip is not an unmodified upstream build; verify final byte counts on release runners before adoption.
- Lockfile SHA-256: `7ead85d988cf0d8f8b729a441974a3fa706612e903a064d7bdbebc91b12101f4`.

Each archive contains the same versioned directory and one executable as the release package. The comparison normalizes tar timestamps, ownership, and ordering, then sends identical tar bytes to each encoder. The actual x86_64 packaging script produced 1,822,176 bytes; the normalized baseline is 1,822,166 bytes. That ten-byte difference is packaging metadata, not an optimization. ARM64 uses the same normalized layout and structural validation, without the native smoke test.

All sizes below are exact bytes. Savings are relative to the same architecture's normalized baseline gzip package. The recorded timings are single local observations with mixed build-cache states and concurrent activity; they are not controlled performance benchmarks.

## Build settings

The table holds compression constant at `gzip -6`. Each configuration retains full LTO, one codegen unit, symbol stripping, and the existing dependency features.

| Architecture | Build | Binary bytes | gzip bytes | Bytes saved | Saved |
| --- | --- | --- | --- | --- | --- |
| x86_64 | baseline | 4,264,832 | 1,822,166 | 0 | 0.00% |
| x86_64 | opt2 | 4,150,144 | 1,770,247 | 51,919 | 2.85% |
| x86_64 | opts | 3,412,864 | 1,466,452 | 355,714 | 19.52% |
| x86_64 | optz | 3,228,544 | 1,378,727 | 443,439 | 24.34% |
| x86_64 | abort | 3,842,944 | 1,611,719 | 210,447 | 11.55% |
| x86_64 | opts-abort | 3,191,680 | 1,340,250 | 481,916 | 26.45% |
| x86_64 | optz-abort | 2,982,784 | 1,243,331 | 578,835 | 31.77% |
| aarch64 | baseline | 3,438,432 | 1,637,158 | 0 | 0.00% |
| aarch64 | opt2 | 3,285,256 | 1,581,872 | 55,286 | 3.38% |
| aarch64 | opts | 2,814,856 | 1,337,842 | 299,316 | 18.28% |
| aarch64 | optz | 2,602,032 | 1,292,681 | 344,477 | 21.04% |
| aarch64 | abort | 3,040,760 | 1,449,818 | 187,340 | 11.44% |
| aarch64 | opts-abort | 2,550,920 | 1,213,396 | 423,762 | 25.88% |
| aarch64 | optz-abort | 2,346,680 | 1,154,805 | 482,353 | 29.46% |

`opt2`, `opts`, and `optz` set optimization to `2`, `s`, and `z`. `abort` changes only the panic strategy; combined names apply both changes. Size-focused optimization is not guaranteed to produce the smallest binary on every program or toolchain, which is why both `s` and `z` were measured. See [Cargo profile settings](https://doc.rust-lang.org/cargo/reference/profiles.html).

`panic = "abort"` is a behavior tradeoff: panic terminates the process without unwinding destructors. In this repository, `LiveRenderer::drop` in [the live progress renderer](../src/progress/live.rs) attempts to restore a hidden cursor on panic cleanup. Do not adopt abort solely from the size result; its extra savings require a separate decision about panic cleanup and process behavior.

## Archive encoders

This table holds the binary constant at the baseline build, isolating archive-format and compression-level effects. `xz-bcj` uses the matching x86 or ARM64 instruction filter before LZMA2 preset 9e. Zopfli still produces a gzip stream.

| Architecture | Encoder | Archive bytes | Bytes saved | Saved | Encode seconds |
| --- | --- | --- | --- | --- | --- |
| x86_64 | gzip6 | 1,822,166 | 0 | 0.00% | 0.169 |
| x86_64 | gzip9 | 1,812,227 | 9,939 | 0.55% | 0.398 |
| x86_64 | xz6 | 1,357,572 | 464,594 | 25.50% | 0.667 |
| x86_64 | xz9e | 1,358,108 | 464,058 | 25.47% | 0.717 |
| x86_64 | xz-bcj | 1,308,164 | 514,002 | 28.21% | 0.805 |
| x86_64 | zstd19 | 1,473,953 | 348,213 | 19.11% | 0.500 |
| x86_64 | zopfli15 | 1,746,412 | 75,754 | 4.16% | 7.396 |
| aarch64 | gzip6 | 1,637,158 | 0 | 0.00% | 0.100 |
| aarch64 | gzip9 | 1,633,008 | 4,150 | 0.25% | 0.192 |
| aarch64 | xz6 | 1,140,920 | 496,238 | 30.31% | 0.450 |
| aarch64 | xz9e | 1,140,740 | 496,418 | 30.32% | 0.486 |
| aarch64 | xz-bcj | 1,061,808 | 575,350 | 35.14% | 0.514 |
| aarch64 | zstd19 | 1,328,180 | 308,978 | 18.87% | 0.376 |
| aarch64 | zopfli15 | 1,575,259 | 61,899 | 3.78% | 4.838 |

Increasing gzip from its default level 6 to level 9 provides a small improvement here. Higher compression settings are not universally smaller: xz preset 9e does not consistently beat preset 6 on these payloads. See the [gzip manual](https://www.gnu.org/s/gzip/manual/gzip.html) and [xz manual](https://tukaani.org/xz/man/xz.1.html).

Zopfli improves gzip compression without changing the client decoder, at the cost of additional packaging time and a build-side tool dependency. Its output was decoded and compared byte-for-byte with the input tar. See [Zopfli's gzip compatibility](https://github.com/google/zopfli).

## Combined candidates

The combinations below retain panic unwinding and all current dependencies. They show the total improvement over the measured baseline build profile and default gzip, rather than adding separate percentage savings.

| Architecture | Candidate | Archive bytes | Bytes saved | Saved | Time saved at 100 KiB/s |
| --- | --- | --- | --- | --- | --- |
| x86_64 | optz + gzip9 | 1,371,726 | 450,440 | 24.72% | 4.40 s |
| x86_64 | optz + zopfli15 | 1,319,728 | 502,438 | 27.57% | 4.91 s |
| x86_64 | optz + xz6 | 1,056,464 | 765,702 | 42.02% | 7.48 s |
| x86_64 | optz + xz-bcj | 1,003,936 | 818,230 | 44.90% | 7.99 s |
| x86_64 | optz + zstd19 | 1,153,416 | 668,750 | 36.70% | 6.53 s |
| aarch64 | optz + gzip9 | 1,290,130 | 347,028 | 21.20% | 3.39 s |
| aarch64 | optz + zopfli15 | 1,236,187 | 400,971 | 24.49% | 3.92 s |
| aarch64 | optz + xz6 | 949,804 | 687,354 | 41.98% | 6.71 s |
| aarch64 | optz + xz-bcj | 886,764 | 750,394 | 45.84% | 7.33 s |
| aarch64 | optz + zstd19 | 1,110,532 | 526,626 | 32.17% | 5.14 s |

Download time is a payload-only calculation at a constant 100 KiB/s. It excludes DNS, TLS, redirects, retries, and extraction. It illustrates the magnitude of the byte savings rather than predicting end-to-end installation time.

## Compatibility and recommendation

Start by evaluating `opt-level = "z"` with the existing panic strategy. It gives substantial gzip savings on both architectures without replacing dependencies. Before shipping, compare representative parsing, reconciliation, and startup workloads against the baseline and rerun on native release runners. This experiment does not establish runtime-performance equivalence.

For unchanged client requirements, evaluate Zopfli packaging next. It produces ordinary `.tar.gz` archives and the extra work happens during release packaging. If the added packaging tool is not worthwhile, `gzip -9` is a smaller, simpler improvement.

Plain xz provides substantially larger archive savings, but the current [installer](../install.sh) and [installation requirements](installation.md) do not guarantee that an xz decoder exists. Do not replace gzip unconditionally under the agreed requirement of no additional client dependencies. A future optional xz asset could be selected before downloading when the decoder is available, with gzip retained for other hosts. That would require coordinated installer, archive-validator, release-asset, checksum, and documentation changes; none are made here. The same client-availability question applies to zstd.

Keep instruction-filtered xz separate from plain xz. ARM64 BCJ requires XZ Utils 5.4.0 or a compatible decoder; the local xz NEWS documents this requirement. A generic check for an `xz` executable is insufficient for that format. The BCJ figures are exploratory, not a compatibility-approved recommendation. Plain xz is the simpler candidate for any future optional-format work.

Leave panic strategy changes, dependency replacement, and internal rewrites for later. This experiment also does not test nightly standard-library rebuilding, executable packers, PGO, or CPU-specific code generation. Those approaches introduce additional runtime, toolchain, portability, or workflow tradeoffs beyond the first-pass comparison.

## Dependency attribution

A separate baseline `cargo bloat --crates` run identifies where later investigation could start. Its diagnostic binary retains symbols and debug information and reports a 6.8 MiB file, so that file size must not be used as the stripped release baseline. Attribution under LTO is approximate and does not predict compressed bytes saved by removing a dependency.

| Crate or group | Approximate .text size | Share of .text |
| --- | --- | --- |
| std | 883.0 KiB | 30.1% |
| tmup | 501.0 KiB | 17.1% |
| regex_automata + regex_syntax + aho_corasick + regex | 604.6 KiB | 20.6% |
| clap_builder | 225.6 KiB | 7.7% |
| winnow + kdl | 183.1 KiB | 6.2% |
| tabled + papergrid | 148.5 KiB | 5.1% |
| tokio | 84.0 KiB | 2.9% |

The regex group is a candidate for later inspection because the TPM declaration matcher uses a small fixed pattern. This is a prioritization clue, not evidence that the full group can be removed or that doing so saves 604.6 KiB in the download. No dependencies or features were changed.

## Reproduce the measurements

Use the source commit and tool versions recorded above for a historical comparison. From that checkout, install Rust 1.92.0 with both musl targets and populate the Cargo cache. The commands below use GNU tar and put build and archive outputs under `target/`:

```sh
rustup toolchain install 1.92.0 --profile minimal
rustup target add --toolchain 1.92.0 x86_64-unknown-linux-musl aarch64-unknown-linux-musl
cargo +1.92.0 fetch --locked

arch=x86_64
opt=3
panic=unwind
release_target="$arch-unknown-linux-musl"
package="tmup-v0.3.1-$release_target"
output="$PWD/target/release-size/$arch/$opt-$panic"

CARGO_PROFILE_RELEASE_OPT_LEVEL="$opt" \
CARGO_PROFILE_RELEASE_PANIC="$panic" \
CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_LINKER=rust-lld \
    cargo +1.92.0 build --release --locked --offline --target "$release_target"
mkdir -p "$output/$package"
cp "target/$release_target/release/tmup" "$output/$package/tmup"
tar --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner \
    -cf "$output/package.tar" -C "$output" "$package"
gzip -n -6 -c < "$output/package.tar" > "$output/package.tar.gz"
xz -T1 -6 -c < "$output/package.tar" > "$output/package.tar.xz"
wc -c "$output/$package/tmup" "$output/package.tar.gz" "$output/package.tar.xz"
gzip -dc "$output/package.tar.gz" > "$output/gzip-restored.tar"
xz -dc "$output/package.tar.xz" > "$output/xz-restored.tar"
cmp "$output/package.tar" "$output/gzip-restored.tar"
cmp "$output/package.tar" "$output/xz-restored.tar"
```

Repeat with `arch=aarch64`, optimization levels `2`, `s`, and `z`, and `panic=abort` for the configurations in the build table. These are command-scoped Cargo profile overrides, so the baseline remains level 3 even when the checkout defaults to `z`. Use a separate output directory for each configuration and avoid concurrent builds in the shared Cargo target directory.

For the other encoders, compress the same tar stream with `gzip -n -9 -c`, `xz -T1 -9e -c`, or `zstd -q -T1 -19 -c`. The BCJ experiment used `xz -T1 --x86 --lzma2=preset=9e -c` on x86_64 and replaced `--x86` with `--arm64` on ARM64. Feed tar bytes through standard input so gzip does not record an input filename or timestamp.

The Zopfli comparison used Python package `zopfli==0.4.1`, calling `zopfli.gzip.compress(tar_bytes, numiterations=15)`. Verify its output with a gzip decoder and compare the restored bytes with the same source tar. Zopfli is an optional experiment dependency, not a release packaging requirement.

To inspect baseline code attribution, run:

```sh
CARGO_PROFILE_RELEASE_OPT_LEVEL=3 CARGO_PROFILE_RELEASE_PANIC=unwind \
    cargo +1.92.0 bloat --release --locked --target x86_64-unknown-linux-musl \
    --target-dir target/release-size/bloat --crates -n 20
```

## Validation

All 88 compressed streams in the matrix and Zopfli supplement were decompressed and compared byte-for-byte with their source tar. Every x86_64 build passed `--version` and `--help`; all 14 baseline and variant gzip packages passed the production archive validator, using structure-only validation for ARM64. Baseline and `optz` packages encoded with gzip level 9 and Zopfli also passed that validator. The original x86_64 baseline passed the production package script and its archive smoke test. ARM64 artifacts were cross-built and structurally checked but not executed. Performance and native ARM64 behavior remain unverified.

Repository checks passed: `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` (507 passed, one ignored). These used the default installed toolchain, rustc 1.94.0-nightly; the measured binaries used stable 1.92.0. The default test suite passing does not validate panic-abort cleanup or establish performance equivalence for size-focused builds.
