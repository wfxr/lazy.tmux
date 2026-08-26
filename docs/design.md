# tmup Stable Design Invariants

This document records durable repository-level constraints for tmup. It is
intentionally not an implementation walkthrough. If a detail is likely to
change with refactors (internal module boundaries, exact command flow, file
format fields, or UI/progress behavior), it does not belong here.

## Durable Goals

- tmup is config-driven and automation-friendly.
- The same Effective Plugin Specification and lock snapshot yield reproducible
  remote plugin revisions.
- Startup (`init`) is safe under concurrent execution.
- Runtime behavior is explicit: partial per-plugin failures are surfaced as
  command failure via non-zero exit status.
- Compatibility targets common TPM plugin loading behavior, not TPM internals.

## Hard Invariants

### 1) Config-Driven Sync Before Mutation

- Remote declarations whose Enable Conditions are true form the desired state
  for an Execution Host.
- `tmup.lock` is the resolved state used by mutating workflows.
- Any mutating workflow that depends on remote state must reconcile the
  Effective Plugin Specification into lock state before applying follow-up
  mutation.
- Reconciliation is by canonical remote plugin identity, not display metadata.

### 2) Lockfile-Backed Reproducibility

- Lock entries are keyed by canonical remote plugin ID.
- Restore-like behavior targets lock-recorded revisions.
- Remote plugins participate in lock-backed lifecycle; local plugins do not.
- Load Conditions do not change lock membership or lock fingerprints.
- Lockfile corruption or parse failure is a hard error, not a silent reset.

### 3) Staged Publish and Rollback Safety

- Revision changes are prepared in staging and published only after preparation
  succeeds.
- A failed build in staging must not replace an already-working installed
  revision.
- Successful publishes are reflected in lock state; failed publishes preserve
  previous lock state for affected plugins.

### 4) Init Lock-Through-Load

- `init` holds the operation lock from entry through plugin loading.
- `init` must not allow concurrent writers to mutate managed plugin state while
  init is reconciling and loading.
- `init` may install or repair missing/drifted managed state when configured,
  but must not perform implicit version advancement beyond declared config.

### 5) Selector, ID, and Install-Path Alignment

- Remote plugin identity is canonical and URL-derived.
- The same canonical ID is used consistently as:
  - lock key
  - target selector for CLI operations
  - managed install path identity
- `name` is display-only and must never be treated as persistent identity.

### 6) Explicit Managed-State Boundary

- tmup only guarantees behavior for tmup-managed plugin state.
- Out-of-band filesystem edits inside the managed root are outside contract.
- `clean` only handles undeclared managed remote repos; it is not a generic
  filesystem sanitizer.

### 7) Conditional Inclusion and Loading

Conditions separate host-specific managed state from tmux loading without
weakening lock-backed lifecycle guarantees. See
[ADR 0001](adr/0001-separate-plugin-inclusion-from-loading.md) for the trade-offs
behind this boundary.

- Enable Conditions project declarations into one Effective Plugin
  Specification per Execution Host.
- Load Conditions only control whether an Init Session sets a plugin's options
  and runs its scripts; they do not change managed state.
- Conditional loading is scoped to the tmux server's Execution Host, not to
  individual clients.
- A false Load Condition does not undo effects from an earlier load.

## TPM Compatibility Contract (Stable Surface)

- Compatibility is defined as: for plugins with Load Eligibility, set tmux
  options and load plugin `*.tmux` scripts in effective declaration order.
- tmup does not promise TPM's internal repository layout or helper-script
  behavior.
- Plugins that depend on TPM internals are outside tmup's compatibility target.

## Non-Goals

- Being a TPM implementation clone.
- Preserving TPM's flat install-layout assumptions.
- Implicitly updating existing plugin revisions during `init`.
- Treating local plugin paths as lock-managed remote plugins.
- Guaranteeing behavior for manual, in-place mutation of tmup-managed repos.
- Providing client-specific plugin loading within one tmux server.
- Unloading or reversing arbitrary plugin effects when a Load Condition changes.

## Change Discipline

When behavior changes, this document should only change if repository-level
invariants changed. Command internals, progress/reporting mechanics, exact
layout examples, and roadmap/status tracking belong in operational docs or
code-level documentation, not here.

For implementation-level progress internals, see module docs and comments in
`src/progress/` (especially `mod.rs`, `reducer.rs`, and `reporter.rs`).
