# Step 1: partial compatibility evidence

Status: **native compatibility gate passes for the tested local engine and
launch context**, including the Rust session owner and real terminal handoff.
Provider, protocol, draft and recovery implementation followed the isolated probe;
remaining visual acceptance is recorded separately.

Run on macOS with `codex-cli 0.167.0`:

```sh
python3 scripts/check-codex-chat.py --rust-native-test
python3 -m unittest discover -s scripts/tests -p test_codex_chat.py -v
```

The probe owns a temporary home/project, Unix app-server and native TUI. It uses
the built-in OpenAI provider pointed at a loopback streaming fixture with a
synthetic API key. Plugins, plugin prefetch, apps, hooks and shell snapshots are
disabled only in the temporary configuration. No real credentials or paid model
requests are used. All owned children and temporary directories are cleaned up.

Verified:

- Native WebSocket handshake, canonical server home and rejection of a wrong home.
- Socket peer PID, canonical native executable, engine/TUI start times and socket
  inode identity; process identity is revalidated before handoff.
- Two cooperating clients, approval replay after unsubscribe/reconnect, external
  answer resolution, and a stale competing answer. The declined command would
  create an isolated marker file; the marker remains absent.
- A native TUI starts a turn; API clients join while approval is pending. Stopping
  that TUI leaves the shared turn active and its request replayable.
- Legacy and paginated histories with multiple seeded turns. Pagination uses a
  one-entry page size, checks turn IDs, unique item IDs, prompt/reply content and
  exact history preservation across native Console recovery.
- A separate thread's history stays unchanged.
- Ten self-checks cover framing, fragmentation/ping, message/queue byte bounds,
  absolute read deadlines, early events, typed request IDs, cursor cycles/page
  budgets and reused PIDs.

The extended native gate additionally verifies:

- The Rust adapter's real WebSocket transport and kernel socket-pair binding.
- Direct resume through Muxy's session owner from an idle shell, normalized
  history, input gating, exact-thread Console recovery, and takeover from the
  recovered real Codex TUI. The shell's captured spawn environment is used for
  discovery and launch, rather than substituting the server environment.
- Native persisted client-message correlation IDs for operation reconciliation.
- Owned stdio engine completion/shutdown and native --no-daemon Console recovery.

Limits:

- Targets are explicit native thread IDs chosen from the verified engine. The
  implementation does not infer the current thread from TUI argv or sessionId.
- The tested fork starts a new session tree, so distinct thread.id/sessionId values
  remain unexercised. Runtime target paths always use thread.id.
- Signed-in visual desktop acceptance remains unverified. The Computer Use native
  pipe and browser host were unavailable. GPUI tests cover the actual interaction
  paths without provider billing or access to live sessions.

The basic probe alone reports a partial gate. With --rust-native-test the JSON
report identifies the tested native transport and owner handoff gate as passed.
Unsupported target contexts remain in Console; no live server was restarted.
