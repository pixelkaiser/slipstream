# Agent Knowledge Map

This map points agents to the smallest useful source of truth. Start here when
the task is unfamiliar, then open only the referenced files that match the work.

## Primary Entry Points

- `AGENTS.md` - durable agent instructions and the top-level navigation map.
- `README.md` - Slipstream user-facing overview, install path, and fork
  positioning.
- `LOCAL_DEVELOPMENT.md` - local backend, no-cloud defaults, provider
  environment variables, build commands, and troubleshooting.
- `Makefile` - local build, check, package, signing utility, and release helper
  entrypoints.
- `RELEASE.md` - Slipstream release, package publishing, branch/tag, asset, and
  GitHub Actions verification runbook.
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

## Build, Package, and Publishing

- Local macOS app build: `make warp-build`.
- Optimized local macOS bundle: `make warp-build-optimized`, which produces
  `target/release-lto/bundle/osx/Slipstream.app`.
- Local package dispatcher: `script/bundle`, which routes to platform-specific
  bundle scripts under `script/macos/`, `script/linux/`, or `script/windows/`.
- Slipstream production release: push a `vMAJOR.MINOR.PATCH` tag to `origin`;
  `.github/workflows/release-macos.yml` builds and publishes the signed
  `Slipstream.dmg`, Linux SSH extension tarball, and `SHA256SUMS` to the
  GitHub Release.
- Slipstream release source branch: usually `byok` in this fork, but the
  release identity is the tag and exact commit. Verify with `git branch -vv`,
  `git rev-parse <tag>`, and the workflow run before publishing.
- Moving SSH-extension release: `.github/workflows/release-remote-server-latest.yml`
  runs on `byok`, `main`, and `master` pushes and updates the
  `remote-server-latest` prerelease asset.
- Local backend container image: `.github/workflows/local-multi-agent-image.yml`
  publishes `ghcr.io/<owner>/warp-local-multi-agent` on path-filtered pushes to
  `byok` and `master`, with branch, SHA, and `latest` tags.
- Inherited upstream Warp release automation:
  `.github/workflows/cut_new_releases.yml`,
  `.github/workflows/cut_new_release_candidate.yml`, and
  `.github/workflows/create_release.yml` target upstream-style `master` and
  `*_release/*` release flows. Do not use them for Slipstream production
  publishing unless the task explicitly says to.
- Release inspection: prefer `gh run list --workflow release-macos.yml`,
  `gh run watch <run-id> --exit-status`, and
  `gh release view <tag> --repo pixelkaiser/slipstream`.

## Mechanical Verification

- Run `make verify-agent-docs` after changing `AGENTS.md`, `PLANS.md`,
  `docs/agent-knowledge-map.md`, `RELEASE.md`, Makefile release targets, or the
  agent-facing workflow guidance.
- The verifier checks required routing files, local Markdown links, ExecPlan
  required-section coverage, repo-scoped skill coverage in this map, and
  release/publishing strings that should not drift silently.

## Lookup Recipes

- For a UI change, read `CONTRIBUTING.md`, then
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
- For release/package changes, read `RELEASE.md`, `Makefile`, the relevant
  workflow under `.github/workflows/`, and the invoked `script/` helper before
  changing behavior.

## Maintenance

Update this map when adding a new top-level guide, repo-scoped skill, major spec
family, release/publishing path, or durable workflow that agents should
discover early. Keep entries short and navigational; detailed rules belong in
the linked source files.
