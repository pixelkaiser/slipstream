# Agent Instructions

Treat this file as the repository entry point for coding agents. Keep it short:
open the linked source of truth that matches the task, then work from current
code and live command output.

## Start Here

- For the repo map, read `docs/agent-knowledge-map.md`.
- For Slipstream no-cloud defaults, local backend setup, provider environment
  variables, and local validation commands, read `LOCAL_DEVELOPMENT.md`.
- For app build, local package build, and release publishing commands, read
  `Makefile` and `RELEASE.md`.
- For public contribution flow, readiness labels, specs, review, and manual
  testing expectations, read `CONTRIBUTING.md`.
- For long-running feature work, broad refactors, migrations, or highly
  ambiguous tasks, use an ExecPlan as described in `PLANS.md`.
- For task-specific workflows, check `.agents/skills/` and read the full
  `SKILL.md` before following a skill.
- For doc-routing changes, run `make verify-agent-docs` before finishing.

## Repository Shape

- `app/` contains the main desktop application and most product behavior.
- `crates/` contains shared Rust crates, including UI, integration testing,
  local agent services, MCP, terminal, and persistence support.
- `specs/` contains product specs, tech specs, and existing plan artifacts.
- `.agents/skills/` contains repo-scoped workflows for agents.
- `.github/workflows/` contains CI, release, changelog, and maintenance jobs.
- `script/` contains bootstrap, format, lint, build, and release helpers.
- `RELEASE.md` is the source of truth for Slipstream package publishing and
  release verification.

## Working Rules

- Start from live evidence: inspect the relevant codepath, spec, logs, rendered
  template, or command output before proposing or applying a fix.
- Prefer narrow, interface-preserving changes that match existing patterns.
- Use `rg`/`rg --files` for search when available.
- Do not run repo-level formatters such as `cargo fmt` across the full
  workspace unless the user explicitly asks for it.
- If formatting is needed, prefer the narrowest command or manual edit that
  only touches files already in scope for the current task.
- Do not revert unrelated dirty-worktree changes. Stage or commit only when the
  user asks, and include only the intended files.

## Validation

- Match validation to the risk and blast radius of the change.
- Documentation-only changes do not require code tests, but do require
  proofreading links, paths, and command names.
- Agent-routing or repo-operational doc changes require
  `make verify-agent-docs`.
- Rust logic changes usually need focused unit tests or an existing targeted
  `cargo test` / `cargo nextest` command.
- User-facing workflows should be validated with integration tests or manual
  app evidence when feasible; use `.agents/skills/warp-integration-test/` for
  integration test work.
- UI work must follow `.agents/skills/warp-ui-guidelines/SKILL.md`.
- Before opening or updating a reviewed PR, follow `CONTRIBUTING.md`, the
  PR-template validation expectations, and any task-specific skill guidance.

## Releases and Security

- Slipstream production publishing is tag-based: semver tags pushed to `origin`
  trigger `.github/workflows/release-macos.yml`, which publishes GitHub Release
  assets after signing, notarization, and package validation.
- The maintained Slipstream integration branch in this fork is `byok`; do not
  assume `master` is the release source for Slipstream artifacts without
  checking the target tag, branch, and workflow.
- Inherited upstream Warp release automation still uses `master` and
  `*_release/*` branches; keep that separate from Slipstream publishing unless
  the task explicitly targets upstream release machinery.
- When creating or updating releases, always write full release notes with a
  changelog in the GitHub Release body; do not leave releases with only terse
  or auto-generated notes unless the user explicitly asks for that.
- When cutting macOS releases, do not wait for the DMG build workflow to finish
  unless the user explicitly asks you to; it can take hours. Trigger the
  release, provide the tag and workflow/release follow-up details, then stop.
- Do not open public pull requests or issues that disclose a non-public
  security vulnerability; use the private disclosure path in `SECURITY.md`.
