# AI Agents

Muxy recognizes AI coding agents running in its terminals and shows whether
each one is working, waiting for you, or done. It needs no setup, hooks, or
plugins, and works while no app is open.

## Supported agents

Antigravity, Claude Code, Codex, Copilot, Cursor, Droid, Grok, Hermes, Kiro,
Oh My Pi, OpenCode, Pi, and Xal.

The server spots them from the running program and what's on screen.
Recognition is best effort.

An agent that reports its own status with the
[program status protocol](https://www.superlogical.com/rex/docs/build/program-status)
(OSC 7501), such as recent Claude Code, is followed by its reports instead of
its screen. Its `app` name is enough to recognize it, so this also works over
SSH.

Oh My Pi detection reads its OSC terminal-title run state: a spinner while
working, `!` when waiting, and `>` when idle. It also recognizes optional OSC
progress reports, `⎋` working messages, and the older `Working… [esc]` spinner.
In `omp --print` sessions, a final non-empty
`Working...` line signals work; once output follows it, that progress line
no longer keeps the session busy. Detection uses terminal escape sequences
and screen text, so disabling or customizing these signals can affect recognition.

For foreground `omp` and `oh-my-pi` processes, automatic terminal titles omit
the leading `π ` prefix. The provider icon remains visible, and custom tab
titles are unchanged.

Hermes is recognized whether it was installed as a `hermes` command or by its
installer, which starts Python with a launcher program. In the TUI
(`hermes --tui`), its state comes from the window title it sets: `⏳` while
working, `⚠` while waiting, and `✓` when idle, in every language. In the
default CLI, it comes from the screen: the spinner on the line above the input
while working, and the approval, question, password, and setup prompts with
their English labels. The classic layout (`display.cli_layout: classic`) and
other languages show the CLI as idle.

## States

| State | Meaning |
| --- | --- |
| Working | The agent is busy. |
| Waiting | The agent needs your input. |
| Idle | The agent is done or not doing anything. |

Tabs, worktrees, and projects show the agent's icon and state. When several
agents run, "waiting" wins.

## Notifications

The desktop app sends a macOS notification when an agent needs your attention
or finishes working.

- No notification is shown for the pane you are looking at, or for detached
  terminals.
- Looking at the terminal clears the alert in every app.
- Clicking a notification jumps to its pane.
- Only one app notifies, so alerts are never doubled.
- Turn notifications off in **System Settings → Notifications → Muxy**.

Paired phones get [push notifications](mobile.md#push-notifications) for the
same events while the Muxy app is away, about 20 seconds after they happen,
unless someone has looked at the terminal by then. Tapping one opens the
terminal on the phone.

Alerts live in memory on the server, at most one per terminal. Restarting the
server clears them.

## Agents panel

`Cmd+Opt+A` shows the agents panel, focuses it, and hides it once it has
focus. Use the arrow keys and Return to open a row.

- It sits at the bottom of the expanded sidebar; drag its top edge to make it
  taller or shorter. With the sidebar collapsed it
  opens as a popup that closes when you click elsewhere. Once you have shown
  the panel, the collapsed sidebar keeps a button for it until you hide it.
- **Move to Right** docks it in the right panel, even when the left sidebar is
  collapsed. Drag its left edge to resize it; Muxy remembers the side and width.
  It shares this area with extension panels and the panel-mode composer: opening
  one replaces the other. Use `Cmd+Opt+A` to bring Agents back. **Move to Left
  Sidebar** returns it to the sidebar.
- **Detach** floats it over the workspace. Drag its header to move it and its
  edges or corner to resize it; Muxy remembers where. **Reattach to Sidebar** or
  **Reattach to Right Panel** docks it on its last side. The other side's move
  button is also available while floating.
- **Settings → Appearance → Agents panel rows** lists each agent grouped by
  project, most urgent first, with its status on the right like its tab, where
  Return opens its pane; or each project and worktree running agents, with
  counts, where Return opens the worktree. Completed and idle agents keep their
  place, so opening a finished agent doesn't move it.
- **Settings → Appearance → Group by project** puts a heading naming each
  project above its agents. It's on by default.
- **Settings → Appearance → Agents panel density** shows each agent on two
  lines, its tab's title and then its agent and status, with its project too
  when agents aren't grouped (**Normal**), or on one short line with just its
  icon and title (**Compact**).
- **Settings → Appearance → Agents panel icon** shows each row's agent icon,
  or the icon of its project. Worktrees show their project's icon.
- **Settings → Appearance → Show project colors** marks each row with a bar of
  its project's color down its left edge. It's offered with agent icons in
  Normal rows only, since a project icon already shows its project's color
  and a compact row's icon is its only mark.
- **Settings → Appearance → Show idle time** shows how long ago each completed
  or idle agent finished, at the right of its row. After the app restarts, an
  agent whose finish you already saw shows no time until it finishes again.

## Saved agent sessions

Click the **Agent Sessions** history icon on the right of the titlebar, or use
**Toggle Agent Sessions** in the command palette or View menu, to show or hide
the right panel. It has no default shortcut; assign one in Settings → Keyboard.
This is separate from the running-agents panel.

- Sessions belong to the selected local project directory. Worktrees have their
  own history; remote projects never display this computer's sessions.
- Search matches session titles, IDs, and provider names. The filter button on
  the right of search opens a multi-select menu with matching counts,
  **Select all**, and **Clear**. Provider choices, panel visibility, and width
  are remembered.
- Sessions are grouped by recency, with the provider and age under each title.
  Use Up/Down while searching to select a session, then Return to resume it.
  Tab opens provider filtering. Escape clears the search; Escape again returns
  focus to the terminal.
- Click a session to resume it in a new project terminal using the harness's
  native resume command. The harness must be installed and available on the
  terminal's PATH. Muxy does not install, configure, or authenticate it.
- Drag the left edge to resize. The panel shares the right-panel area with
  Agents, extension panels, and the panel-mode composer; opening one replaces
  the other.
- **Pop Out Panel** in its header floats it over the workspace instead, beside
  any docked panel and other floating panels. Drag its header to move it and
  any edge or corner to resize it; **Dock Panel** puts it back. The choice and
  where it floats are remembered. Clicking a floating panel brings it to the
  front.
- History refreshes on opening the panel or switching projects. Large histories
  load automatically in bounded batches across providers; keep browsing while
  loading completes. Use **Refresh sessions** to pick up later changes.

Discovery only reads local JSONL files; it runs in the background, caches
metadata rather than conversations, and never installs hooks or launches a
harness. Files are not modified by discovery. Resuming hands control to the
harness, which may update its session normally.

| Provider | Session files | Resume command |
| --- | --- | --- |
| Claude Code | `~/.claude/projects/*/*.jsonl` | `claude --resume ID` |
| Codex | `~/.codex/sessions/YYYY/MM/DD/*.jsonl` | `codex resume ID` |
| Copilot | `~/.copilot/session-state/*/events.jsonl` | `copilot --resume ID` |
| Droid | `~/.factory/sessions/*/*.jsonl` | `droid --resume ID` |
| Pi | `~/.pi/agent/sessions/*/*.jsonl` | `pi --session ID` |
| Oh My Pi | `~/.omp/agent/sessions/*/*.jsonl` | `omp --resume ID` |
| Xal | `~/.xal/sessions/*/*.jsonl` | `xal resume ID` |

`CLAUDE_CONFIG_DIR` and `CODEX_HOME` are respected when set to absolute paths.
OpenCode, Cursor, Grok, Kiro, Antigravity, and Hermes appear as **Not
supported** in the filter menu: their database-only or unverified storage
formats are not read.
Running-agent detection for these providers is unaffected.

Session discovery is best effort: malformed or unfinished records, records
without a project directory, and nested subagent artifacts are skipped. Reads
are bounded to the start and end of transcripts; unusually large metadata,
older formats, custom storage locations, and titles in separate databases may
not be found. Sessions are sorted by transcript modification time.

## Agents Focused sidebar

Choose **Agents Focused** from the sidebar layout menu to list only the tabs
that run an agent.

## From a shell and extensions

- `muxy activity list --json` prints agent status and unread alerts as JSON.
  `muxy activity ack <event-id>` clears alerts.
- Extensions get the `agent.status` event and `muxy.agents.list()`. See
  [Events](../extensions/events.md).
- Agents can build extensions for you. See
  [Build with an agent](../extensions/agents.md).


## Chat on macOS

When a project has no tabs, its command-card screen also offers a new-session
composer. Enter a prompt, choose OpenAI or Anthropic, a model and effort, then
press Enter or **Start Chat** to start and send in Chat mode. The **CLI / Chat**
switch defaults to Chat. Choose CLI to launch the selected provider's terminal
interface with the same prompt, model and effort. Multiline prompts are preserved.
Shift-Enter adds a newline. The + button continues to offer its existing tab choices.

The Models menu offers discovered models and saved choices on that server; you
can also type a model alias. The provider validates the selection when the new
session starts. Startup and delivery failures preserve the prompt. Branch and
New Worktree controls use the existing Git flows, and the draft follows newly
created worktrees within the same repository. Both modes start in the selected
project or worktree folder, on its selected branch. CLI drafts clear after the
startup command is delivered to the terminal; startup failures retain the draft.

**New Chat** opens a local draft. The composer picker offers **OpenAI / model**
(Codex CLI) and **Anthropic / model** (Claude Code), using the installed CLIs and
existing account configuration. Changing provider keeps text and attachments.
Provider is remembered per project; model and effort are remembered separately
for each provider. Existing saved choices belong to Claude.

Model discovery sends no prompt. A conversation starts on first Send. The first
message freezes its provider, model, effort and attachments. The composer clears
only after provider acknowledgment; edits made while delivery is pending remain.
An uncertain send is checked against operation status and native history and is
never automatically resent. Keep its draft until acceptance is established.
After startup, provider stays fixed for that conversation; offered model and
effort settings can change while idle.

Codex history comes from its verified app-server, including paginated history and
threads without JSONL paths. Select the exact thread in the engine-scoped picker
or use **Resume in Chat** from the agent sessions panel. Console recovery keeps
the exact native thread ID, executable, configuration home and engine endpoint.
Leaving a shared daemon detaches without interrupting its work. Leaving an owned
standalone engine requires confirmed completion before shutdown and recovery.

The server verifies native process start time and socket ownership before taking
over a running Codex terminal. Unsupported profiles, CLI overrides, remote
transports, custom backends and standalone TUIs without a verifiable API endpoint
stay in Console with an explanation. No shared daemon is terminated. Native
approval choices keep their once, session and rule scopes; requests Muxy cannot
represent remain pending for Console or another capable client.

Codex Chat requires compatible desktop and server builds. Older clients get an
upgrade explanation and read-only Console access while Chat owns the session.
API-capable servers do not fall back to filesystem-only Codex discovery. Existing
Claude rendering, selection, tool errors, questions and approvals share the same
transcript and composer components.
