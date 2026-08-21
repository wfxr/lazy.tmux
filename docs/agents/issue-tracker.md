# Issue tracker: GitHub

Issues and specs for this repository live in GitHub Issues. Use the `gh` CLI for all operations.

## Conventions

Use these commands when interacting with issues:

- **Create an issue:** `gh issue create --title "..." --body "..."`. Use a heredoc for multi-line bodies.
- **Read an issue:** `gh issue view <number> --comments`, filtering comments with `jq` and fetching labels when needed.
- **List issues:** `gh issue list --state open --json number,title,body,labels,comments --jq '[.[] | {number, title, body, labels: [.labels[].name], comments: [.comments[].body]}]'` with appropriate `--label` and `--state` filters.
- **Comment on an issue:** `gh issue comment <number> --body "..."`
- **Apply or remove labels:** `gh issue edit <number> --add-label "..."` or `gh issue edit <number> --remove-label "..."`
- **Close an issue:** `gh issue close <number> --comment "..."`

Infer the repository from `git remote -v`; `gh` does this automatically when run inside the clone.

## Pull requests as a triage surface

**PRs as a request surface: no.** Set this value to `yes` if the repository later treats external pull requests as feature requests.

When enabled, pull requests use the same labels and states as issues:

- **Read a pull request:** Run `gh pr view <number> --comments` and `gh pr diff <number>`.
- **List external pull requests:** Run `gh pr list --state open --json number,title,body,labels,author,authorAssociation,comments`, then keep only `CONTRIBUTOR`, `FIRST_TIME_CONTRIBUTOR`, or `NONE` author associations.
- **Comment, label, or close:** Use `gh pr comment`, `gh pr edit --add-label`, `gh pr edit --remove-label`, or `gh pr close`.

GitHub shares one number space across issues and pull requests. Resolve an ambiguous reference such as `#42` with `gh pr view 42`, then fall back to `gh issue view 42`.

## Publishing to the issue tracker

When a skill says “publish to the issue tracker,” create a GitHub issue.

## Fetching a ticket

When a skill says “fetch the relevant ticket,” run `gh issue view <number> --comments`.

## Wayfinding operations

The `/wayfinder` skill represents a map as one GitHub issue and its tickets as child issues.

- **Map:** Create one issue labeled `wayfinder:map` that contains the Notes, Decisions-so-far, and Fog sections.
- **Child ticket:** Link an issue to the map as a GitHub sub-issue. If sub-issues aren't enabled, add the child to a task list in the map body and put `Part of #<map>` at the top of the child body. Apply a `wayfinder:<type>` label, where the type is `research`, `prototype`, `grilling`, or `task`.
- **Blocking:** Use GitHub's native issue dependencies. If dependencies aren't available, add a `Blocked by: #<n>, #<n>` line at the top of the child body.
- **Frontier query:** List the map's open children, exclude assigned tickets and tickets with open blockers, and select the first remaining ticket in map order.
- **Claim:** Run `gh issue edit <number> --add-assignee @me`.
- **Resolve:** Comment with the answer, close the ticket, and append a context pointer to the map's Decisions-so-far section.
