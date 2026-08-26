# Repository Guidelines

`tmup` is a config-driven Rust CLI for managing tmux plugins. Favor reproducible, rollback-safe, and script-friendly behavior over implementation simplicity.

## Behavioral Invariants

- Reconcile `tmup.kdl` into `tmup.lock` before follow-up mutations that depend on remote state, so desired and resolved state remain aligned.
- Keep `init` safe under concurrent execution. It may install or repair configured state, but it must not advance revisions beyond the declared config because startup must remain deterministic.
- Publish revision changes through staging. If preparation or a build fails, preserve the previously installed and locked revision so an unsuccessful update cannot break a working setup.
- Use the canonical remote plugin ID consistently for repository-cache paths, lock keys, install paths, and targeted CLI selectors. Treat display names as presentation only.
- Keep config, lockfile, and managed on-disk state consistent. Treat lockfile corruption or parse failure as a hard error rather than silently resetting state.
- Preserve script-friendly CLI semantics and exit codes. Partial per-plugin failures must produce a non-zero exit status.
- For changes to sync, init, lockfile behavior, remote identity, publish or rollback, or managed-state boundaries, read `docs/design.md`; it is the source of truth for the complete contract and intentional non-goals.

## Verification

A behavior change is incomplete until its tests and validation cover the affected contract.

- Add or update tests for every behavior change. Prefer integration tests for CLI behavior, sync semantics, repository-cache behavior, publish or rollback, and lockfile interactions because these contracts span module boundaries.
- Preserve regression coverage for partial-failure reporting, failure markers, and targeted operations.
- Run `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` before finishing work.
- Manually verify affected tmux flows when changing `init`, popup or split behavior, loader ordering, or other tmux-facing runtime behavior.

## Agent Skills

### Disabled plugins

Treat the `superpowers` plugin as disabled in this repository. Do not invoke, read, or follow any `superpowers:*` skill; use other applicable skills instead.

### Issue tracker

Issues and specs are tracked in GitHub Issues. See `docs/agents/issue-tracker.md`.

### Triage labels

This repository uses the five default triage labels. See `docs/agents/triage-labels.md`.

### Domain docs

This repository uses a single-context domain documentation layout. See `docs/agents/domain.md`.
