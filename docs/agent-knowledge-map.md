# Agent Knowledge Map

This map points agents to the smallest useful source of truth. Start here when
the task is unfamiliar, then open only the referenced files that match the work.

## Primary Entry Points

- `AGENTS.md` - durable agent instructions and the top-level navigation map.
- `README.md` - Slipstream user-facing overview, install path, and fork
  positioning.
- `LOCAL_DEVELOPMENT.md` - local backend, no-cloud defaults, provider
  environment variables, build commands, and troubleshooting.
- `WARP.md` - engineering guide for build/test commands, architecture, Rust
  style, terminal locking rules, feature flags, and PR expectations.
- `CONTRIBUTING.md` - public issue/spec/PR workflow, readiness labels, manual
  testing requirements, and review flow.
- `PLANS.md` - ExecPlan convention for restartable long-running agent work.
- `SECURITY.md` - private vulnerability reporting path.

## Product and Technical Specs

- `specs/` is the versioned home for product specs, tech specs, and durable plan
  artifacts.
- Public GitHub issue work generally uses `specs/GH<issue-number>/product.md`
  and `specs/GH<issue-number>/tech.md`.
- Internal task specs generally use `specs/<ticket-or-topic>/PRODUCT.md` and
  `specs/<ticket-or-topic>/TECH.md`.
- Existing examples include `specs/GH1063/`, `specs/GH1066/`,
  `specs/codex-warp-plugin/TECH.md`, and `specs/BYOK-local-multi-agent/PLAN.md`.

## Repo-Scoped Skills

Repo skills live under `.agents/skills/`. Read a skill's full `SKILL.md` before
using it.

- `add-feature-flag` - adding a new Warp feature flag.
- `promote-feature` - promoting a feature flag to Dogfood, Preview, or Stable.
- `remove-feature-flag` - removing a stabilized feature flag.
- `add-telemetry` - designing and adding telemetry events.
- `create-launch-modal` - one-time launch modal implementation.
- `rust-unit-tests` - focused Rust unit test guidance.
- `warp-integration-test` - integration tests under `crates/integration/`.
- `warp-ui-guidelines` - required reading for UI work.
- `review-pr-local` - Warp-specific PR review guidance.
- `triage-issue-local` - Warp-specific issue triage guidance.
- `dedupe-issue-local` - Warp-specific duplicate issue guidance.
- `reproduce-bug-report-local` - logged-out UI bug reproduction guidance.
- `onboarding-verification-skill` - cloud Linux onboarding verification.
- `changelog-draft` and `classify-changelog-pr` - release changelog drafting.

## Common Work Areas

- Desktop app behavior: start in `app/src/`, then narrow by feature directory.
- Terminal behavior: `app/src/terminal/` and terminal-related tests.
- Agent and harness behavior: `app/src/ai/`, `app/src/ai_assistant/`,
  `app/src/local_multi_agent/`, and
  `app/src/ai/agent_sdk/driver/harness/`.
- Local no-cloud backend: `crates/local_multi_agent_service/README.md`,
  `crates/local_multi_agent_service/src/`, and `LOCAL_DEVELOPMENT.md`.
- MCP and tools: `crates/mcp/`, `app/src/ai/mcp/`, and related specs under
  `specs/`.
- UI framework and shared components: `crates/warpui/`,
  `crates/warpui_core/`, `app/src/ui_components/`, and
  `app/src/view_components/`.
- Persistence and migrations: `crates/persistence/` and
  `crates/persistence/migrations/`.
- Integration tests: `crates/integration/` plus the
  `warp-integration-test` skill.
- Release and changelog automation: `Makefile`, `.github/workflows/`,
  `.agents/skills/changelog-draft/`, and release scripts under `script/`.

## Lookup Recipes

- For a UI change, read `WARP.md`, then
  `.agents/skills/warp-ui-guidelines/SKILL.md`, then the relevant view files.
- For a feature flag change, use `add-feature-flag`, `promote-feature`, or
  `remove-feature-flag` before editing code.
- For telemetry, start with `add-telemetry` and confirm event purpose and fields
  before implementation.
- For a new or changed integration test, use `warp-integration-test`.
- For local inference or no-cloud behavior, start with `LOCAL_DEVELOPMENT.md`
  and `crates/local_multi_agent_service/README.md`.
- For Codex or third-party harness work, inspect
  `app/src/ai/agent_sdk/driver/harness/` and relevant specs such as
  `specs/codex-warp-plugin/TECH.md`.
- For specs or long-running work, read `CONTRIBUTING.md` and `PLANS.md`, then
  place durable artifacts under `specs/`.

## Maintenance

Update this map when adding a new top-level guide, repo-scoped skill, major spec
family, or durable workflow that agents should discover early. Keep entries
short and navigational; detailed rules belong in the linked source files.
