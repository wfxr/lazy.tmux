---
status: accepted
---

# Reject unsupported native configuration syntax

tmup configuration controls plugin lifecycle work and server-global tmux behavior. Silently ignoring unsupported native KDL can turn a misspelled property, option, or binding into a valid-looking configuration that does not express the user's intent. A closed grammar makes configuration validity explicit and gives every command the same fail-fast boundary before conditions or state changes.

## Decision

Native `tmup.kdl` is a closed, fail-fast grammar. The document root contains at most one `options` node and any number of `plug` nodes in any order. Unknown root nodes and duplicate `options` nodes are errors.

An `options` node requires a child block and has no arguments, properties, or KDL type annotations. Its child block contains at most one `auto-install` node and at most one `concurrency` node. Each option child has exactly one argument of its documented type and has no properties, child block, or KDL type annotation. Missing nodes retain their defaults.

A `plug` node has exactly one untyped, non-empty source string. It accepts only documented properties, each at most once and with its documented type. Unknown properties, extra positional arguments, and unknown child nodes are errors. `build` is available only as a plugin property; the former `build` child form is unsupported.

Every recognized node and entry has an exact documented shape. Unsupported arguments, properties, child blocks, and KDL type annotations are errors throughout the document. Plugin sources, explicit names, tracking selectors, build commands, option keys, environment names, binding keys, binding shell commands, and binding option strings must contain a non-whitespace character. Option values and environment values may be empty. `opt-prefix` may also be empty.

Every command validates the complete native document before evaluating predicates or mutating managed or runtime state. Validation remains fail-fast: tmup reports the first error with semantic context such as `options.concurrency`, a plugin source, a node, or a property rather than introducing an aggregated diagnostic system.

This decision applies only to native `tmup.kdl`. The TPM-compatible scanner continues to extract declarations it recognizes from `.tmux.conf` without treating unrelated tmux syntax as errors. Operational warnings remain available for decisions and status such as a KDL declaration replacing a TPM declaration, skipped TPM discovery, or stale lock metadata. These warnings describe valid configuration processing; they do not provide syntax compatibility.

## Considered options

Keeping warnings for unknown plugin parameters was rejected because it makes typographical errors indistinguishable from intentional forward compatibility and lets commands continue with incomplete runtime behavior.

Strictness only inside Runtime Configuration Branches was rejected because the same runtime nodes would have different validity depending on nesting. Closing the whole native grammar gives top-level and branched declarations one contract.

Collecting every error in one parse was deferred. Fail-fast validation keeps the parser and diagnostics small while still preventing all condition evaluation and state mutation for an invalid document.

## Consequences

Configurations that relied on ignored nodes, properties, extra arguments, type annotations, or the `build` child form must be corrected before upgrading. Adding future native syntax requires a tmup release that explicitly parses and validates it; older versions fail instead of guessing how to interpret it.

The parser no longer produces syntax warnings for native KDL. Operational warnings remain part of configuration loading and command output.

The closed grammar makes shared parsing of unconditional and branched runtime declarations practical because both positions enforce the same node shapes. That internal refactor is independent of this user-visible decision.
