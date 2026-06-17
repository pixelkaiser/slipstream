# Codex Execution Plans

An ExecPlan is a checked-in, self-contained plan for work that is too broad or
stateful to rely on chat context alone. Use it to make long-running work
restartable by a fresh agent with only the current working tree and the plan
file.

## When to Use an ExecPlan

Use an ExecPlan for:

- Multi-hour features, large refactors, migrations, or cross-module changes.
- Work with substantial unknowns that need staged discovery or prototypes.
- Changes that need multiple independently verifiable milestones.
- Tasks where the user explicitly asks for a plan, execution plan, or durable
  handoff artifact.

Do not create an ExecPlan for small bug fixes, focused tests, documentation
edits, simple configuration changes, or ordinary single-module work unless the
user asks for one.

## Where Plans Live

- Put task plans near the work they govern, usually
  `specs/<issue-or-topic>/PLAN.md`.
- If the plan belongs to an existing spec folder, add it there rather than
  creating a parallel location.
- Keep ephemeral chat-only plans out of the repository unless they need to be
  restartable across agents or reviewed as an artifact.

## Requirements

Every ExecPlan must be:

- Self-contained: define non-obvious terms, name all relevant files, and include
  the exact commands a new agent should run.
- Outcome-focused: describe what should work when the plan is complete and how
  to observe it.
- Incremental: split work into milestones that can each be validated.
- Current: update progress, discoveries, decisions, and outcomes as work
  proceeds.
- Safe to resume: explain idempotence, retry steps, and cleanup expectations.

Commit only when the user or the surrounding workflow asks for a commit. The
plan should still record enough state that another agent can continue without
the commit history.

## Required Sections

Each ExecPlan should contain these sections in this order:

1. `Purpose / Big Picture`
2. `Progress`
3. `Surprises & Discoveries`
4. `Decision Log`
5. `Context and Orientation`
6. `Plan of Work`
7. `Concrete Steps`
8. `Validation and Acceptance`
9. `Idempotence and Recovery`
10. `Outcomes & Retrospective`

Use checkbox entries in `Progress`. Use dated bullets in `Decision Log` and
`Surprises & Discoveries` when a choice or observation affects future work.

## Writing Guidance

Write for a capable engineer who has never seen this repository. Prefer prose
that explains why each step exists. Use repository-relative paths, exact
function or module names, and exact commands with the working directory. Avoid
outsourcing essential context to chat history or external documents; if a
source is required, summarize the needed facts in the plan and link the source
for verification.

Acceptance should be behavior a person can observe, not just internal structure.
For example, say which test fails before and passes after, which command returns
which response, or which UI state should be visible after launching the app.

## Updating an ExecPlan

When implementing from an ExecPlan:

- Read the full plan before editing code.
- Keep `Progress` accurate at every stopping point.
- Add discoveries when code, tests, tooling, or runtime behavior differs from
  the plan.
- Record design choices in `Decision Log` instead of leaving them implicit.
- Update validation steps when the real command or acceptance signal changes.
- Finish with an `Outcomes & Retrospective` entry describing what landed, what
  remains, and how it was verified.
