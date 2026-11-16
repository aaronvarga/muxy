# Implementation status

Implemented locally: provider-aware local drafts, promptless model discovery,
per-project provider/model/effort preferences, native Codex parsing and approval
encoding, shared transcript retention, additive protocol capabilities, and
server-issued submission tickets with bounded deduplication and history
reconciliation. Initial Claude prompts wait for model/effort acknowledgments;
Codex includes those choices in initial turn/start. Rejected prompts retain the
composer while retries use the already-started provider.

Native Codex discovery lists exact app-server targets without requiring rollout
paths. Targets carry canonical home, executable, endpoint and engine PID/start
identity. The terminal resolver verifies both ends of its real Darwin socket
connection before detach. Shared-engine Console handoff never interrupts the
thread or terminates the daemon. Owned-engine shutdown checks loaded session-tree
threads and confirmed idle state; uncertain detach or recovery keeps input gated.

The isolated gate passes on Codex CLI 0.167.0, including the real Rust transport
and session owner: idle-shell resume, Console recovery, native TUI takeover,
two-client approvals, external resolution, legacy and paginated history,
unrelated-thread preservation, owned stdio shutdown and standalone recovery.
The fixture also verifies that native history preserves operation correlation IDs.
A distinct thread.id/sessionId fixture was not produced; the implementation uses
thread.id for every target and sessionId only to guard owned session-tree shutdown.

The temporary desktop smoke instance used a separate HOME and MUXY_DIR. It was
closed along with its test server. The installed Muxy Local app and Muxy Dev
server remained running and were not installed over or restarted.

Visual acceptance remains unverified: the Computer Use native pipe and browser
host were unavailable. GPUI regression tests exercise provider switching, text and
image preservation, delayed acknowledgment edits, rejected-first-prompt retry,
Console reachability, rendering, selection and the existing Claude fixtures.
No signed-in paid provider prompt, commit, push, installation or live-session
handoff was performed. Unsupported launch contexts stay in Console.
