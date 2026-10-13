# Constraints

Supported platforms, and lessons that shape the code.

## Platforms

| Part | Supported |
| --- | --- |
| Desktop app | macOS 14 and later |
| `muxy` and `muxy-server` | macOS 14 and later, and Linux with glibc 2.35 or later, on x86_64 and ARM64 |
| Mobile SDK | iOS and Android |

Not yet supported: Windows, and musl or Alpine Linux. Linux builds must be
built and run natively on both architectures to count as supported.

### Experimental Linux desktop

The Linux desktop port is under development. It is tried on Ubuntu 24.04 ARM64
in SwiftVM on Apple Silicon, with GNOME and KDE Plasma, each on Wayland and X11.
This does not establish support for other distributions, x86_64 desktop builds,
or hardware GPU drivers. SwiftVM uses Mesa's software Vulkan renderer, so its
performance is not representative of a native GPU.

The system draws the window: frame, buttons, shadow, resizing and tiling. GNOME
on Wayland draws no frame, so there the app shows its own buttons and the
window cannot be resized from its edges.

The port covers the terminal desktop, in-app dialogs and menus, desktop-portal
file selection, Linux shortcuts, and `xdg-open` for external resources. Commands
that macOS offers in the menu bar are in the command palette. Copy and paste
use Ctrl+Shift+C and Ctrl+Shift+V in terminals; text fields use Ctrl+C and Ctrl+V.
Existing configured shortcuts remain literal. New configurations use the system
monospace font and inline paths for composer images.

Voice input, extension scripts and webviews, global Quick Terminal shortcuts,
native desktop notifications, and automatic updates still need Linux backends.
Clipboard image submission is unavailable because the current clipboard API
cannot safely preserve all MIME types and detect ownership changes. Use inline
image paths instead. The command-line tool's in-app installer still expects a
macOS bundle; run the built Linux CLI directly for now.

To build on Ubuntu, install Rust from `rust-toolchain.toml`, Zig 0.15.2, and the
development packages `build-essential pkg-config clang cmake libssl-dev
libfontconfig1-dev libfreetype-dev libxkbcommon-dev libxkbcommon-x11-dev
libwayland-dev libxcb1-dev libvulkan-dev`. Runtime packages include
`mesa-vulkan-drivers xdg-desktop-portal xdg-desktop-portal-gnome xdg-utils`.
Then run `bash scripts/build-desktop-linux.sh`. `MUXY_ZIG` can point at Zig, and
`CARGO_TARGET_DIR` must point to a guest-local writable directory when the source
checkout is shared read-only. Keep `muxy-app`, `muxy`, and `muxy-server` together.

Visual checks are performed by a person: start the app from a desktop terminal,
open a project, create and split terminals, exercise shell input and Ctrl+C,
copy/paste Unicode text, scroll history, open Settings and a folder picker, then
cancel a close confirmation before confirming another. Check both Wayland and
X11 sessions. Next milestones are native x86_64 and hardware GPU validation,
Linux packaging and launch integration, and the deferred platform backends.

On Linux hosts that end a user's processes at logout, run
`loginctl enable-linger` so the server keeps running after an SSH session ends.

Turning on mobile access may show a macOS firewall prompt for `muxy-server`, and
the iOS app needs local network permission.

macOS charges a process's privacy permissions, such as Local Network, to the app
process that started it. The desktop app starts the server through a small copy
of itself that lives as long as the server, so terminals keep Muxy's permissions
after the app quits.

## Terminal engine

- Ghostty's terminal isn't thread-safe, so one thread owns each terminal.
- The history limit is in bytes.
- The server decides when to compress history, once a session goes quiet.
- Measure memory as physical footprint, not RSS. Compressed pages are released
  to the system but not freed, so RSS doesn't show the savings.
- Ghostty's Rust API is unstable. Pin its version and keep it inside
  `muxy-terminal`.

## PTYs

- How fast a terminal fills up depends on how the program writes, not on parsing.
  In our benchmarks, a program that writes line by line reached about 12 MB/s
  on macOS. One that writes large blocks reached 280 MB/s through the same PTY.
- Reads therefore arrive small and often. The reader must be its own blocking
  thread. Sleeping to batch reads would stall the program, because the kernel's
  PTY buffer holds only a few kilobytes.

## Memory and drawing

- A queue that grows with output explodes when an app stalls. Merge instead of
  queueing ([D8](./decisions.md#d8-one-merged-frame-in-flight)).
- Redraw only when something changes. Heavy per-cell color changes are the worst
  case: merge rectangles by color and skip blank text before reaching for caches.
- Measure again after changing the terminal engine, the screen format, or flow
  control.

## Shell integration

The server loads its shell hooks for zsh and fish automatically, without
editing your startup files. Bash is opt-in. Add this to the file your Bash
profile loads for interactive shells:

```bash
if [[ ${MUXY_SHELL_INTEGRATION:-0} == 1 ]]; then
    source "$MUXY_SHELL_INTEGRATION_DIR/muxy.bash"
fi
```

If you already have a Bash `DEBUG` trap, it is kept. Jumping between prompts
still works, but command start and exit status aren't marked. Turning shell
integration off in Settings applies to new terminals right away.
