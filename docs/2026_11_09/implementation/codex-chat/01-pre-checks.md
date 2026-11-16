# Codex chat implementation

Branch: `local`. The checkout already contains extensive uncommitted Claude chat,
composer, protocol, and UI changes. Preserve those changes and the running app,
server, and terminals. No new dependencies, commits, installation, or activation.

The supplied revised plan is authoritative. Its first gate precedes UI changes:
prove endpoint/home identification, two cooperating approval clients, native
history, and reversible Chat/Console handoff in isolation. A failed gate leaves
existing Codex terminals unchanged; new-chat-only delivery is not a substitute.

Installed CLI at investigation: `codex-cli 0.167.0`. Its generated schema exposes
`initialize.codexHome`, distinct `thread.id` and `thread.sessionId`, paginated
turn/item APIs, and `serverRequest/resolved`. Unix sockets require WebSocket
framing; `app-server proxy` proxies raw transport bytes, not JSONL conversion.
