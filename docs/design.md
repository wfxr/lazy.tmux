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
behind plugin inclusion and loading, and see
[ADR 0002](adr/0002-select-runtime-configuration-during-init.md) for runtime
configuration selection.

- Enable Conditions project declarations into one Effective Plugin
  Specification per Execution Host.
- Load Conditions only control whether an Init Session applies a plugin's
  environment operations and options, runs its scripts, and registers its
  bindings; they do not change managed state.
- Runtime Configuration Branches select plugin runtime declarations without
  changing the Effective Plugin Specification, Load Eligibility, lock
  membership, or lock fingerprints.
- An authoritative Init Session evaluates branches only for plugins with Load
  Eligibility and freezes the selection before managed-state mutation.
- Conditional loading and branch selection are scoped to the tmux server's
  Execution Host, not to individual clients.
- A false Load Condition or changed branch selection does not reconcile or undo
  effects from an earlier load.

### 8) Strict Native Configuration

Native `tmup.kdl` uses a closed, fail-fast grammar. See
[ADR 0003](adr/0003-reject-unsupported-native-configuration-syntax.md) for the
syntax boundary and compatibility trade-offs.

- Every command validates the complete native configuration before evaluating
  predicates or mutating managed or runtime state.
- Unknown nodes, properties, extra arguments, duplicate scalar declarations,
  unsupported child blocks, and KDL type annotations are hard errors rather
  than compatibility warnings.
- The TPM-compatible scanner remains permissive because `.tmux.conf` contains
  tmux syntax outside tmup's native grammar.
- Operational warnings for valid configuration processing remain distinct from
  syntax errors.

## TPM Compatibility Contract (Stable Surface)

- Compatibility is defined as: initialize the plugin manager path, then apply
  each plugin's ordered, projected tmux environment operations and options for
  all plugins with Load Eligibility that were not excluded by a reconciliation
  failure, then load their `*.tmux` scripts in effective declaration order and
  register all projected explicit bindings in plugin and node declaration
  order.
- Runtime Configuration Branches flatten into the existing load phases. Source
  order remains authoritative within the environment and option phase and the
  binding phase; plugin scripts remain between those phases.
- A plugin reconciliation failure omits every runtime command for that plugin
  from the current Init Session, even when staged-publish rollback preserves an
  older installed revision and lock entry.
- The first plugin-attributable tmux command failure skips that plugin's
  remaining commands in the current and later phases. Independent plugins
  continue, and all plugin failures produce a non-zero final outcome.
- Failure of the initial tmup-owned plugin-manager-path command is an
  operation-level error and aborts loading because it is not attributable to a
  plugin.
- Runtime loading is not transactional. Environment values, options, script
  effects, and bindings that completed before a later failure remain applied.
- Runtime declarations, including branch contents, are replayed only by
  `init`. They do not affect lock fingerprints, and removing a declaration or
  selecting another branch does not clean up an effect from an earlier Init
  Session.
- Binding shell actions run through `/bin/sh` from the resolved plugin
  directory. This contract does not require native `run-shell -c`, tmux version
  detection, duplicate binding checks, or automatic unbinding.
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
- Reconciling or reversing arbitrary plugin effects when a Load Condition or
  Runtime Configuration Branch selection changes.

## Change Discipline

When behavior changes, this document should only change if repository-level
invariants changed. Command internals, progress/reporting mechanics, exact
layout examples, and roadmap/status tracking belong in operational docs or
code-level documentation, not here.

For implementation-level progress internals, see module docs and comments in
`src/progress/` (especially `mod.rs`, `reducer.rs`, and `reporter.rs`).
