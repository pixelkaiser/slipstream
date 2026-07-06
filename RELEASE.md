# Slipstream Release Flow

Slipstream production releases are anchored on annotated semver git tags. A
GitHub Release is created from the tag, and the release workflow builds, signs,
notarizes, validates, and uploads the macOS `.dmg`, Linux SSH extension
tarball, and checksums.

## Release Anchor

The release identity is the git tag, not a branch name.

- Tags must match `vMAJOR.MINOR.PATCH`, for example `v0.2.0`.
- The tag points at the exact commit being released.
- The GitHub Release, release notes, `Slipstream.dmg`, Linux SSH extension
  tarball, and `SHA256SUMS` are all attached to that tag.

Use a commit that has already been reviewed and tested. Prefer an immutable
commit SHA when cutting the final release, even if you use a branch name while
checking what will be released.

## Branch Roles

- `byok` is the maintained Slipstream integration branch in this fork and is the
  normal source branch for Slipstream release dry runs and semver release tags.
- Slipstream production publishing is tag-based, not branch-based. The published
  release is whatever exact commit the pushed `vMAJOR.MINOR.PATCH` tag points
  at.
- `origin/master` is also present because this fork tracks upstream Warp. Public
  upstream contribution guidance and inherited Warp release workflows refer to
  `master`, but that is not automatically the right Slipstream release source.
- Inherited upstream release machinery uses `master` for new channel releases
  and `*_release/*` branches for release candidates. Keep those workflows
  separate from Slipstream's `release-macos.yml` flow unless the task explicitly
  targets upstream release automation.

## Prerequisites

The production Slipstream release workflow is
`.github/workflows/release-macos.yml`.

The repository must have these GitHub Actions secrets configured:

- `WARP_DEVELOPER_ID_CERT`
- `WARP_DEVELOPER_ID_CERT_PASSWORD`
- `WARP_CODESIGN_KEYCHAIN_PASSWORD`
- `WARP_NOTARIZATION_APPLE_ID`
- `WARP_NOTARIZATION_PASSWORD`
- `WARP_APPLE_TEAM_ID`

The local user cutting a release needs permission to push tags to the
`pixelkaiser/slipstream` repository.

## Dry Run

Before publishing a real release, run the workflow manually from GitHub Actions
or with the GitHub CLI. A `workflow_dispatch` run builds, signs, notarizes, and
validates the DMG and Linux SSH extension, but uploads them only as workflow
artifacts and does not create a GitHub Release.

```bash
gh workflow run release-macos.yml \
  --repo pixelkaiser/slipstream \
  --ref byok \
  -f tag=v0.2.0
```

Watch the run:

```bash
gh run list \
  --repo pixelkaiser/slipstream \
  --workflow release-macos.yml \
  --limit 5
```

```bash
gh run watch <run-id> \
  --repo pixelkaiser/slipstream \
  --exit-status
```

The dry run should produce:

- `slipstream-macos-<tag>` with `Slipstream.dmg` and `SHA256SUMS`.
- `slipstream-remote-server-<tag>` with
  `slipstream-remote-server-linux-x86_64.tar.gz`.

## Cut a Release

Use the helper target from the repository root:

```bash
make release-macos TAG=v0.2.0 REF=<commit-sha>
```

For a no-op check:

```bash
make release-macos TAG=v0.2.0 REF=<commit-sha> DRY_RUN=1
```

The target:

- validates that `TAG` matches `vMAJOR.MINOR.PATCH`;
- fetches existing tags from `origin`;
- resolves `REF` to an exact commit SHA;
- refuses to overwrite an existing local or remote tag;
- creates an annotated tag with the message `Slipstream <tag>`;
- pushes only `refs/tags/<tag>`.

Pushing the tag triggers the real release workflow. On a tag push, the workflow
sets `should_publish=true`, so the final job creates or updates the GitHub
Release.

After pushing the tag, inspect the launched workflow:

```bash
gh run list \
  --repo pixelkaiser/slipstream \
  --workflow release-macos.yml \
  --branch <tag> \
  --limit 1
```

## What CI Builds

The Slipstream release workflow currently builds the `oss` channel for Apple
Silicon macOS plus the Linux SSH extension.

The workflow:

1. validates the release tag and signing secrets;
2. builds the unsigned Apple Silicon app bundle;
3. packages the build inputs as a workflow artifact;
4. signs the app and bundled helper binaries;
5. creates `Slipstream.dmg`;
6. submits the DMG to Apple notarization;
7. staples the notarization ticket;
8. validates with `codesign`, `xcrun stapler validate`, `spctl`, and
   `hdiutil verify`;
9. generates `SHA256SUMS`;
10. builds and packages `slipstream-remote-server-linux-x86_64.tar.gz`;
11. uploads the DMG, Linux SSH extension tarball, and checksums to the GitHub
    Release.

## Other Published Artifacts

- `.github/workflows/release-remote-server-latest.yml` runs on pushes to
  `byok`, `main`, and `master`, plus manual dispatch. It builds the Linux SSH
  extension, force-updates the `remote-server-latest` tag, and creates or
  updates the `remote-server-latest` prerelease with
  `slipstream-remote-server-linux-x86_64.tar.gz` and `SHA256SUMS`.
- `.github/workflows/local-multi-agent-image.yml` runs for pull requests that
  touch the local backend image inputs and on path-filtered pushes to `byok` or
  `master`. Push runs publish
  `ghcr.io/<owner>/warp-local-multi-agent` with branch, SHA, and `latest` tags.
- `.github/workflows/create_release.yml`, `cut_new_releases.yml`, and
  `cut_new_release_candidate.yml` are inherited upstream Warp release workflows.
  They build broader Warp channel artifacts and publish to upstream-style
  GitHub Release/GCS/Sentry targets when called with publishing enabled. Do not
  use them for Slipstream production publishing without first confirming that
  the task is intentionally targeting upstream release machinery.

## Release Notes

The workflow asks GitHub to generate notes when it creates a new release, but
agent-driven release follow-through must still verify the body and replace
terse or incomplete generated text with full release notes and a changelog
unless the user explicitly asks to keep generated notes.

When creating a new release, the publish job runs:

```bash
gh release create "$TAG" Slipstream.dmg slipstream-remote-server-linux-x86_64.tar.gz SHA256SUMS \
  --verify-tag \
  --title "Slipstream $TAG" \
  --generate-notes
```

GitHub compares the tag with the previous release tag and writes the changelog
body. The first release may only contain a full-changelog link because there is
no earlier Slipstream release to compare against.

If a release already exists, a rerun uploads `Slipstream.dmg`,
`slipstream-remote-server-linux-x86_64.tar.gz`, and `SHA256SUMS` with
`--clobber`. It does not regenerate the release notes, so reruns do not rewrite
the release body unexpectedly.

## Verify a Published Release

After the workflow completes, check the release:

```bash
gh release view v0.2.0 \
  --repo pixelkaiser/slipstream
```

Expected assets:

- `Slipstream.dmg`
- `slipstream-remote-server-linux-x86_64.tar.gz`
- `SHA256SUMS`

The release should be published, not a draft, and should be attached to the tag
you pushed.

For moving SSH extension releases, check:

```bash
gh release view remote-server-latest \
  --repo pixelkaiser/slipstream
```

For local backend image publishing, check the `Local Multi-Agent Image`
workflow run and GHCR package tags for the branch, SHA, and `latest` tags.

## Failure Handling

Do not move or replace a public release tag after users may have seen it. If a
release is bad, cut a new patch version.

If packaging or notarization fails, inspect the package job logs first. The
workflow is designed to surface Apple notarization errors before stapling, so
the failing nested binary or signature issue should be visible in the job log.

If the publish job fails after packaging succeeds, rerun the workflow for the
same tag. Existing release assets are uploaded with `--clobber`.
