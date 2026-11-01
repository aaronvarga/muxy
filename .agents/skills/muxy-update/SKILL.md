---
name: muxy-update
description: Rebase this personal Muxy fork onto a requested upstream release and validate its customizations. Use for Muxy version upgrades, keeping source updates separate from installing or restarting the running app and server.
---

# Muxy Update

Read the repository's current `AGENTS.md` and referenced RTK instructions. This
workflow updates the fork; it does not itself authorize installation, app/server
restarts, pushes, or upstream interaction beyond the user's existing instructions.

## Establish the upgrade boundary

- Inspect the current branch, dirty/staged/untracked files, remotes, and worktrees.
  Commit pending work in logical groups when requested. Preserve unrelated work.
- Verify remote URLs: this fork normally uses `origin` for `muxy-app/muxy`
  (read-only) and `fork` for the personal repository. Do not assume the names
  still identify those repositories.
- Fetch refs and tags. Resolve the requested release tag to its commit and inspect
  `BETA_VERSION`. `Cargo.toml` can retain a placeholder package version. A release
  request targets that release, even when upstream `main` has moved beyond it.
- Capture the original local and remote branch OIDs, merge base, and ordered list
  of fork-only commits. Create a uniquely named backup branch before rewriting.
  Do not rebase other branches or worktrees merely because they exist.

## Replay and reconcile

Rebase the fork-only range onto the chosen release, keeping the individual commits.
Use `--no-update-refs` so backup and unrelated branch refs remain unchanged. Compare
the original and rebased ranges with `git range-diff`; account for every original
commit. Do not skip a conflicting commit merely to finish the rebase.

Resolve conflicts by adapting custom behavior to the upstream implementation:

- Keep upstream functionality as well as the fork's commands bar, split/move
  controls, sidebar settings/dividers, extra folders, Command Center, menu icons,
  and Dock attention badge when present in the original range.
- Check protocol capability numbers and CBOR field/variant tags. Upstream pairing
  and this fork's extra folders previously collided at feature `3`; folder support
  now uses `0x8001`. Preserve distinct meanings and test capability negotiation.
  Do not assume a matching protocol version means an older server supports every
  fork feature. Explain any upgrade requirements for older fork servers.
- Preserve saved-state defaults and settings migrations. Missing new fields must
  not discard existing projects, folders, dividers, or appearance choices.
- When upstream removes test harnesses, port meaningful fork regression coverage
  to current fixtures instead of reviving entire obsolete harnesses.
- Check new attachment fields, picker purposes, native view APIs, toolbar sizing,
  and geometry contracts. A drag preview covers a pane frame, while terminal-body
  geometry can exclude the fork's header. Keep tests for the actual intended UI.

## Build and validate

Use the toolchain versions pinned by the target release's CI. For macOS, check
`xcode-select -p` and `xcrun metal --version`; Command Line Tools alone may lack
Metal. Select an installed full Xcode with command-local `DEVELOPER_DIR` rather
than changing the system developer directory. If necessary, install its Metal
component with `xcodebuild -downloadComponent MetalToolchain`.

Set `ZIG` to the pinned Zig executable. Prefer a separate verified download/cache
over replacing another project's global Zig. Use the same environment for Cargo
checks, tests, documentation, and release builds.

Run the checks required by `AGENTS.md`, focused fork regressions, and a locked
release build of `muxy-app`, `muxy-cli`, and `muxy-server`. Include pagination and
attached/detached inventories, folder access isolation, layouts/collapse/reorder,
native icon orientation, and attention-count transitions when those areas change.
Regenerate English localization when needed with
`MUXY_UPDATE_LOCALIZATION=1 cargo test -p muxy-app --test localization`, then verify
it normally. Fix compatibility failures and rerun the affected checks; distinguish
pre-existing failures from new ones rather than declaring the workspace green.

## Deliver without disrupting sessions

- Commit compatibility fixes separately from the replayed history. Verify the
  target release is an ancestor and review the final diff against it.
- When publishing is authorized, preserve the original remote history with a
  backup ref and update only the personal fork branch using an explicit
  `--force-with-lease=refs/heads/<branch>:<captured-remote-oid>`. If the lease fails,
  inspect the remote changes; do not replace the lease with an unconditional force.
- Leave the running app, server, terminals, and installed bundle untouched unless
  the user authorized a local update. If installation is requested, discover live
  identities afresh; never reuse old hardcoded PIDs or overwrite mapped binaries.
- Launch from a clean environment: automation can inject `NO_COLOR=1`, which
  propagates through the app and server into every terminal. Remove `NO_COLOR`
  when launching and verify it is absent from the new server's environment.
- Report the release/commit, preserved commit count, backup ref, checks and remaining
  limitations. Keep source validation, signed installation, app restart, server
  compatibility, and live visual acceptance as separate claims.
