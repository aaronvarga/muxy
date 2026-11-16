# Validation

Validation covers the working checkout, including the pre-existing uncommitted
Claude/UI changes. No baseline changes were reset or committed.

| Check | Result |
| --- | --- |
| cargo fmt --all --check | Passed |
| cargo clippy --workspace --all-targets --all-features -- -D warnings | Passed |
| cargo test --workspace --all-features | Passed: 1,710 tests; three intentionally ignored |
| RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps | Passed |
| cargo build -p muxy-app -p muxy-server --all-features | Passed |
| Localization extraction and tests | Passed |
| git diff --check | Passed |
| Probe self-checks | Passed: 10 tests |
| Installed Codex native probe with --rust-native-test | Passed, including both isolated Rust integration tests |
| GPUI chat interaction regressions | Passed: 25 tests |

The three ignored workspace tests are the append-only fixture generator and two
native Codex integration tests that require the isolated fixture. The native
probe explicitly runs both native tests. Fixtures were generated additively.

Cargo used cached Zig 0.17.0 via ZIG. Per-check commands, exit codes and durations
are recorded in /tmp/muxy-codex-validation.json; command output is in the matching
/tmp/muxy-codex-*.log files. No new third-party dependency was introduced.

The local desktop/server binaries were prepared without installing or restarting
the live app/server. A separate temporary desktop profile was launched for visual
QA, but the Computer Use native pipe and browser host were unavailable. That
instance and its server were stopped. The installed Muxy Local app and Muxy Dev
server retained their original running PIDs. Signed-in visual acceptance and
live activation remain unverified; automated results do not imply either.
