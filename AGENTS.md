# Agent Instructions

- Do not run repo-level formatters such as `cargo fmt` across the full workspace unless the user explicitly asks for it.
- If formatting is needed, prefer the narrowest command or manual edit that only touches files already in scope for the current task.
- When creating or updating releases, always write full release notes with a changelog in the GitHub Release body; do not leave releases with only terse or auto-generated notes unless the user explicitly asks for that.
- When cutting macOS releases, do not wait for the DMG build workflow to finish unless the user explicitly asks you to; it can take hours. Trigger the release, provide the tag and workflow/release follow-up details, then stop.
