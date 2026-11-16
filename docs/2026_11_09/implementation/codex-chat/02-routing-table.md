# Implementation routing

| Step | Scope | Verification | Dependency |
| --- | --- | --- | --- |
| 1. Compatibility gate | Isolated native CLI harness and regression checks in `scripts/` | Harness self-checks and installed CLI execution | None |
| 2. Draft picker and submission | App/core preferences, chat model, composer/menu/splits, client/server/protocol submission tracking | Draft generation, acknowledgments, reconciliation and edit-preservation regressions | Gate passes |
| 3. Shared provider state | Server chat host/state/normalizers, protocol capabilities/snapshots/fixtures, client/mobile/app labels | Existing Claude fixtures, protocol evolution and shared-client approvals | Gate passes |
| 4. Launch/history/recovery | Server context/supervisor/recovery, native discovery and app session picker | Identity, bounded pagination, hydration races, detach/recovery/rollback | Gate passes |
| 5. Validation/rollout | Workspace, localization and compatible desktop/server builds | Required fmt, Clippy, tests, docs, localization, diff checks; isolated visual QA | Implemented steps |

No production provider or UI change is enabled before step 1 succeeds.
