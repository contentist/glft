# 0005: The computer's window, panel icon and Nemo entry

Date: 2026-10-17. Status: decided (Day 5).

## Decisions
- **The engine owns the panel icon** (StatusNotifierItem, through the `ksni` crate). The
  process that listens shows that it listens, so the icon cannot go away while listening
  carries on, or stay after it stops; terminal-only use gets it too. It shows while
  receiving is on, with "Receiving into …", "Open glft", any waiting offers (Accept /
  Decline) and "Stop receiving". Where no panel shows such icons, or the panel's applet
  goes away, the "Receiving" notification shows instead. Offer and "Received from"
  notifications stay as they were.
- **The engine stops when nothing needs it** (the plan's heartbeat rule). After 60 s with
  no client connected, receiving off, no send running, no offer waiting and no "just this
  time" pairing, it exits; systemd's socket (or the next client) starts it again. A window
  counts only while it pings: every 2 s from the GTK main loop, so a frozen window stops
  counting after 6 s, and a lost watch is reopened only by the next ping, never by a
  background thread. `GLFT_IDLE_SECS` changes the limit (0: never).
- **A "just this time" pairing keeps the engine running** until logout or `glft quit`.
  Found during the test: with the idle exit, it would have been forgotten a minute after the
  window closed. The wording says "until you log out" everywhere.
- **One protocol for windows:** `Watch` sends status, devices, the log, progress and offers
  as they change, and `Ping` is its heartbeat. Progress names its send (`send` id) or its
  sender (`peer`), so several transfers can each have a bar; `CancelSend` (also `glft
  cancel N`) stops one. The terminal and the window share one client
  (`glft_engine::client`), so both start and reach the engine the same way.
- **Plain GTK4 (4.14), no libadwaita, no custom title bar.** It follows the Mint theme,
  and the window manager draws the title bar, which is what LocalSend's window got wrong.
- **One window, through GApplication** (`io.github.contentist.glft`): a second launch
  hands its paths to the running window. The menu entry and Nemo's "Send to phone…" both
  run `glft-window %F`.
- **Nothing is sent without one more click.** Dropped, chosen or right-clicked files show
  "Send glft-drop-test (folder), 3 files, 4.8 MB to:" with one button per paired device,
  as on the phone, even when only one device is paired. Sends are detached: closing the
  window doesn't stop them.
- **Pairing in the window:** the QR code (drawn from the same code as `glft pair`), the
  code text with Copy, a 5-minute countdown, then "Remember …?" with the same three
  answers.
- **`glft setup` installs the menu entry, icon and Nemo action** for the user, pointing at
  the `glft-window` next to `glft`. Packages will install them system-wide (Day 6).
- **The log names a paired device from the start** ("Connecting to "Pixel 8 Pro"" rather
  than its key's short id), and says "Connecting" once per send, not once per redial.

## Results (Inspiron 15 3530, Mint 22.3, Cinnamon 6.6.9, GTK 4.14.5; Pixel 8 Pro receiving)
| Check | X11 | Wayland (Cinnamon on Wayland) |
|---|---|---|
| Title bar: move, resize, minimise, maximise, close | works | works |
| Keyboard: Tab order, Enter, typing | works | works |
| Folder dropped from Nemo → confirm → phone | 3 files, 5.0 MB verified in 1.0 s | confirm row appears |
| Nemo "Send to phone…" with the window already open | same window used; verified in 0.8 s | window comes up with the confirm row |
| Pairing from the window (QR scanned by the phone) | works, Remember and Just this time | not repeated |
| Panel icon: appears with receiving, menu, Stop receiving | works | not repeated |

On Wayland the window is a native Wayland client (connected to `wayland-0` only).

Other checks:
- **Frozen window** (`kill -STOP`): after 6 s the engine logged "A glft window stopped
  answering" and stopped counting it; after `kill -CONT` the window carried on.
- **Pairing again from the window** recorded the phone's Yggdrasil address
  (`200:33a9:…:b3e9`), which the Day 3 pairing could not have (the phone had no Yggdrasil
  then). The laptop can now reach the phone over the mesh too.
- Engine tests (`crates/glft-engine/tests/engine.rs`) run real `glftd` processes: idle
  exit, a watch kept alive by pings and dropped when they stop, cancelling a send, a
  "just this time" pairing keeping the engine, and two engines pairing and sending with
  tagged progress.

## Open
- The full log page ("Show all") and "save somewhere else" for receiving are not in the
  window yet; it shows the last 8 lines and "Copy all".
- The phone's "(Ctrl-C stops)" wording, from Day 4, is still to fix.
