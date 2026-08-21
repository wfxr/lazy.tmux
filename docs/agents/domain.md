# Domain docs

Engineering skills use this repository's domain documentation when exploring the codebase. This repository follows a single-context layout.

## Layout

Domain documentation lives at the repository root and under `docs/adr/`.

```text
/
├── CONTEXT.md
├── docs/
│   └── adr/
└── src/
```

## Before exploring

Before exploring the codebase, read these resources when they exist:

- `CONTEXT.md` at the repository root.
- Relevant architecture decision records under `docs/adr/`.

If these resources don't exist, proceed silently. Don't report their absence or propose creating them before work begins. The `/domain-modeling` skill creates them when terminology or architectural decisions need to be recorded.

## Use glossary vocabulary

When output names a domain concept in an issue title, refactoring proposal, hypothesis, or test name, use the term defined in `CONTEXT.md`. Don't replace defined terms with synonyms that the glossary explicitly avoids.

If a required concept isn't in the glossary, reconsider whether the new language matches the project. Record genuine terminology gaps for `/domain-modeling`.

## Flag ADR conflicts

If proposed work contradicts an existing ADR, identify the conflict explicitly instead of silently overriding the decision.
