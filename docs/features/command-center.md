# Command Center

Open **Command Center** above Home in the sidebar, or find it in the command
palette, to see running terminals across projects and worktrees. It includes
background tabs and detached sessions on connected servers. A server that is
disconnected or waiting for its identity to be confirmed is not read.

On macOS, the menu bar Muxy icon lists running sessions across all projects on
connected, confirmed servers, including projects outside the selected workspace.
It turns red when a session needs input or has an unread update. Yellow dots mark
**Needs input** and blue dots mark **Unread update**, using the same activity
detection as the app. Each row includes its project logo or icon and session title,
with a separate attention dot; remote
sessions also name their server. Selecting a row brings that terminal forward,
reusing its project pane or opening the existing session when detached. Opening
the menu does not mark sessions read or attach terminal views. The icon remains
available while Muxy is hidden or minimized, and disappears when the app quits.

Right-click the Command Center sidebar icon and choose **Color** to change its
background. **Default** follows the theme accent. The selected color is remembered
across restarts, while the selection outline keeps the theme accent.

Each tile shows a small project logo, or its configured icon, alongside the
project name and terminal title. Projects without artwork use the same fallback
as the sidebar. Click into a terminal to type,
select text, or paste. Find and the pane-direction shortcuts act on the focused
tile. Right-click for clipboard actions or **Open in Project**.

Use the chevron in a tile's header to collapse its terminal to the header, or
choose **Collapse Terminal** from its right-click menu. The project artwork,
title, attention indicator, and session controls stay available. The chevron is
just before **Open in Project**, and the entire header remains draggable.
Use the chevron again, or **Expand Terminal**, to restore the terminal. Collapsed
sessions keep running, but their hidden views detach; keyboard pane navigation
skips them. Tiles below a collapsed session move up within the same column.
Other columns keep their positions and terminal sizes. Collapse choices follow their
sessions through reordering and switching projects while the app is running;
they reset when the app restarts.

A thin purple/orange/cyan gradient pulses across the bottom of the header while
a session is working, using the same activity and terminal-progress detection as
the sidebar. It remains visible on
collapsed headers. Paused progress has a steady yellow header underline; errors
use red.
On macOS the pulse uses Core Animation and does not redraw terminal contents.

A blue border marks the focused terminal. Amber borders and labels mark
attention: **Needs input** when the agent is blocked, or **Unread update** for
completed work or another unread notification. A focused terminal keeps its
blue border and amber attention label, so both states remain visible. The input
indicator stays until the agent resumes; reading an unread update clears that
attention state. These use the same detection as project status dots. Detection is best
effort for supported agents, rather than a detector for arbitrary shell prompts.

Drag a tile by its header and drop it onto another tile to reorder the grid.
A floating card identifies the terminal being moved; the destination tile is
highlighted to show its new position. The Layout menu opens below its button.
The dashboard remembers this order across refreshes and restarts. Newly started
terminals follow the arranged tiles. Project tabs and splits are unaffected.
The command palette also offers **Move Terminal Earlier** and **Move Terminal
Later** for keyboard reordering.

**Open in Project** returns to an existing tab and pane. A detached session opens
in a new tab in its original project. Clicking a project in the sidebar or the
**Return to Project** button leaves Command Center. Each tile has an **×** button
to close its terminal session, also available in the right-click menu and with
the close-pane shortcut. Closing asks for confirmation, ends that terminal, and
removes its panes from the original project while keeping Command Center open.
Other sessions and project settings are unaffected. Viewing a terminal, changing
the dashboard layout, scrolling, and leaving the dashboard never end a session
or rearrange project tabs and splits.

## Layout

The **Layout** button offers grid previews labeled **columns × rows**, including
**3 × 1**, **2 × 2**, **3 × 2**, **4 × 2**, **5 × 2**, **3 × 3**, **4 × 3**, and **4 × 4**.
Every preset is available regardless of the current terminal count. The selected
preset is remembered independently of project layouts. Choose **Custom…** for a
10-column by 8-row grid picker. Hover to preview the selection and click to apply
it. Arrow keys adjust the selection, Enter applies it, and Escape cancels. Custom
layouts are saved and add rows as more terminals open, just like presets.

**Command Center Layout…** in the command palette opens the same menu for
keyboard navigation. **Open in Project** is also available in the palette.

**Auto** chooses a grid using the number of terminals and the available space.
Manual layouts keep their selected column count and add rows as needed. For
example, **4 × 2** grows to four columns and three rows when a ninth terminal
starts. Rows shrink back to the preset minimum when terminals end. Tiles keep
a minimum readable height, with further rows available by scrolling. Use the
dashboard scrollbar or scroll over a tile header or the space between tiles.
Scrolling inside a terminal scrolls that terminal.

Only tiles on screen keep live terminal views and attachments. Scrolling a tile
away detaches its view while its process continues running. Loading failures
are shown, with **Retry** for failed attachments.

Terminals share one size across clients. A dashboard tile can resize the running
program and reflow its output; returning to its project restores the size of
the project pane. Composer opens in the focused terminal's project so its draft
and broadcast targets remain scoped to that project.
