# Agents

## Repository

- Before finishing: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets --all-features -- -D warnings`, `cargo test --workspace --all-features`, `cargo doc --workspace --no-deps` with `RUSTDOCFLAGS=-D warnings`.

## Main Guides

- Only test real scenarios and risks (data loss, security, compatibility, concurrency, regressions). Don't test obvious code or defaults.

## Third-party dependencies

When adding new third-party dependencies, provide user with their github url and stars and metrics if they are being maintained so user can approve adding it.

## Performance

- Do not ignore performance and memory efficiency. with GPUI it is easy to cause high CPU and memory usage.

## Fork Workflow

This is a personal fork (`fork` remote). AI agents may commit and push here when asked.

Anything aimed at upstream (`origin`, muxy-app/muxy) stays human-only: opening PRs or issues there, or interacting with them (read-only allowed).
