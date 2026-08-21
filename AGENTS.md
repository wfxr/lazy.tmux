# Repository Guidelines

## Project Overview

`tmup` is a Rust CLI tmux plugin manager inspired by `lazy.nvim`.
It is built around config-driven sync from `tmup.kdl`, lockfile-backed reproducibility with `tmup.lock`, safe publish and rollback semantics, and a persistent bare repo cache for remote plugins.

Keep these behavioral invariants intact when making changes:

- `sync` reconciles config into the lock snapshot before follow-up mutation.
- `init` must remain safe under concurrent execution and must not perform implicit updates beyond declared config.
- Revision changes publish through staging with rollback on build failure.
- Remote plugin IDs, lock keys, install paths, and targeted CLI selectors stay aligned.

## Repository Structure

- `src/main.rs`: CLI entrypoint and command dispatch.
- `src/sync.rs`: reconciliation logic and sync policy handling.
- `src/plugin.rs`: install, update, restore, clean, and list behavior.
- `src/git.rs` and `src/repo.rs`: git operations, repo preparation, and bare cache handling.
- `src/state.rs`: runtime paths, lock coordination, and failure marker state.
- `src/loader.rs` and `src/tmux.rs`: tmux-facing init and loading behavior.
- Other modules in `src/` cover config parsing, lockfile handling, data models, sync planning, terminal UI, and progress display.
- `tests/`: integration-heavy coverage grouped by behavior, including CLI commands, sync semantics, repo cache behavior, restore/publish flows, and tmux/init regressions.
- `docs/design.md`: source of truth for architecture, invariants, and intentional non-goals.

## Development Commands

Run these checks before finishing work:

- `cargo fmt --check`
- `cargo clippy -- -D warnings`
- `cargo test`

## Testing Expectations

- Add or update tests for every behavior change.
- Prefer integration tests in `tests/` when changing CLI behavior, sync semantics, repo-cache behavior, publish/rollback, or lockfile interactions.
- Keep existing regression coverage intact for partial-failure reporting, failure markers, and targeted operations.
- Do manual tmux verification when changes affect `init`, popup/split flow, loader ordering, or other tmux-facing runtime behavior.

## Coding Guidance

- Follow the existing Rust style and keep modules focused on their current responsibilities.
- Preserve CLI semantics and exit-code behavior; this project is intended to be script-friendly.
- Do not weaken lockfile, repo-cache, rollback, or concurrency guarantees just to simplify an implementation.
- Keep config, lockfile, and on-disk managed state consistent with the design documented in `docs/design.md`.
- When changing remote plugin handling, verify that canonical IDs, cache paths, lock entries, and install directories still move together.

## Commit And PR Guidance

Commit messages must follow the Conventional Commits specification:

- Use the imperative mood in the subject line.
- Do not end the subject line with a period.
- Limit the subject line to 72 characters.
- Wrap the body at 72 characters.
- Use the body to explain what and why rather than how.

PRs should:

- Include a short summary of the behavioral change.
- Report the verification you ran.
- Call out manual tmux testing when the change affects tmux-facing behavior.

## Agent skills

### Issue tracker

Issues and specs are tracked in GitHub Issues. See `docs/agents/issue-tracker.md`.

### Triage labels

This repository uses the five default triage labels. See `docs/agents/triage-labels.md`.

### Domain docs

This repository uses a single-context domain documentation layout. See `docs/agents/domain.md`.
