# 0006: Errors and help pages, packages, route choice, where files landed

Date: 2026-10-19. Status: decided (Day 6).

## Decisions
- **One error catalogue, worded for where it shows.** Every failure is an `ErrorKind` with
  an id, a headline and a next step for each `Surface` (terminal, window, phone): the phone
  says "switch receiving on in glft on the computer", the terminal says `glft receive on`.
  The engine sends ids; each client words the next step itself (`next_step_on`). Errors
  made outside the core (the window's dropped non-local files, the phone's "already
  sending", "Network permission off", "receive time limit") joined the catalogue. A test
  checks every kind for a headline, a next step on every surface, a help page, and no raw
  OS error code in the headline. 43 kinds.
- **Help pages are built in.** One Markdown file per id in `docs/help/`, compiled into all
  three programs (`include_str!`): `glft help <id>`, a Help button on the window's error
  banner, and on the phone's error cards and log rows. They work offline and always match
  the installed version. `docs/troubleshooting.md` became the `wifi-freezes` page.
- **The receiver says why it stopped.** It closes the connection with the error's id;
  the sender maps `disk-full` to `peer-disk-full` and `receive-dir-unwritable` to
  `peer-cant-save`, and stops instead of retrying.
- **Packages: a .deb, and a tarball for other distributions.** AppImage and Flatpak are
  deferred.
  - The .deb's `prerm` stops every user's engine before files change (`/proc/*/exe` is
    `/usr/bin/glftd`; SIGTERM, then SIGKILL after 5 s, and says so). `postinst` enables
    the user socket globally. `Depends` comes from `dpkg-shlibdeps`.
  - The tarball's `glft setup` copies itself into `~/.local/lib/glft` with links in
    `~/.local/bin`; `glft uninstall` stops the engine first, then removes everything but
    the key and pairings (`--purge` removes those too). Received files are never touched.
  - Linked against the system GTK (gtk4-rs feature `v4_12`): **GTK 4.12 and glibc 2.39 or
    newer** (Mint 22, Ubuntu 24.04, Debian 13, Fedora 40). Debian 12 (GTK 4.8) is not
    covered; the README says so.
  - After an upgrade receiving is off, as after every start; the README says so.
- **Routes: local first, mesh only when needed (`route::LocalFirst`).** iroh's default
  selector gives IPv6 a 3 ms bonus, so home transfers drifted onto Yggdrasil (IPv6
  `200::/7`) at half speed. glft's own `PathSelector` ranks paths by class: local, then
  other, then tunnel. The class comes from the interface owning the path's local address
  (`IFF_POINTOPOINT`, tunnel names, or ARPHRD 65534 in `/sys`; the `/sys` read is skipped
  on Android, where SELinux denies it), with address ranges as the fallback (mesh is
  `octets[0] & 0xfe == 0x02`, unit-tested with `200::1`, `2001:db8::1` and a real
  Yggdrasil address). Hysteresis applies only within a class.
  - **The pin:** this uses iroh's `unstable-custom-transports` API, so iroh is pinned at
    `=1.3.0`, which also freezes iroh's own fixes. Each iroh release means bumping the pin
    and rerunning the route test (10 GB at home stays on Wi-Fi; lockdown moves it to
    Yggdrasil and back).
- **Every received file says where it landed, on both sides, with one click to open.** The
  receiver sends a `Saved { places }` message after `Done`; older peers ignore it and the
  sender waits at most 2 s for it. The laptop's log, notification ("Show in folder",
  "Open") and the window's Received list name the folder; the phone's "Sent" result and
  its Received list (Open / Show folder) do too. Arrivals survive an engine restart
  (`arrivals.json`, last 20), and so does the log (`log.jsonl`).
- **The phone queues sends.** Each send is its own user-initiated job; later ones show
  "Waiting: …" and start in turn, and each can be cancelled. A network change stops the
  job with `STOP_REASON_CONSTRAINT_CONNECTIVITY`; that pauses and reschedules it rather
  than cancelling ("Stopped on request" before).
- **Devices are named the same everywhere:** `"Pixel 8 Pro" (1040b4f452)`.
- **What arrived verified is kept, however short the attempt** (found by the two-senders
  test). The receiver used to record verified data only every 8 MiB, so a link that
  dropped every few MB never moved forward: every resume said "0 of 1.0 GB here". Now a
  dropped connection first writes and syncs what was verified and records it, and the
  other files in flight finish saving theirs before the attempt ends; progress is reported
  per 4 MiB write. Test: `short_attempts_each_keep_what_arrived` cuts a transfer three
  times before any checkpoint and checks each cut keeps everything written.

## Results (Inspiron 15 3530, Mint 22.3; Pixel 8 Pro, GrapheneOS)
Every failure on the list showed a sentence and a next step on the screen where it
happened.

| # | Test | Result |
|---|---|---|
| 1 | 10 GB, both ways | verified both ways; about 7 MB/s, on Wi-Fi throughout |
| 2 | 5,000 small files, both ways | verified; phone → laptop 51 MB in 998 s (see below) |
| 3, 4 | Wi-Fi off on the phone, then the laptop, mid-transfer | resumed by itself; "waiting to resume", no Ctrl-C hint off the terminal |
| 5 | Phone send stopped by a network change | paused and continued, not cancelled |
| 6, 7 | Lockdown on → over Yggdrasil; off → back to Wi-Fi | as expected, both directions |
| 9 | Laptop not receiving | phone: "The other device did not answer … switch receiving on in glft on the computer" |
| 10 | Declined; disk full (loop-mounted receive folder) | `declined`; sender stops with `peer-disk-full` |
| 11 | Network permission off | `network-denied` with the Settings path |
| 12 | Second engine as a second phone, both sending at once | failed first (stuck resume, fixed above); after the fix, Pixel 1.0 GB at 6.8 MB/s alongside 11 GB at 54 MB/s, both verified |
| 13 | Port taken | the holder named, a spare port used |
| 14 | .deb install, upgrade while receiving, remove | "stopping the running engine (62131) before its files change" |
| 15 | Tarball setup, send, uninstall | engine stopped first; only data left |

Measurements:
- **Wi-Fi ceiling 7–8 MB/s**: 2.4 GHz channel 6 at 144 Mbit/s, two hops through the router
  (no direct link between the devices). **Yggdrasil at home: 2.5–3 MB/s.**
- **The phone reads many small files slowly**: each file is opened afresh through
  Android's document system, about 0.2 s each, which dominates 5,000 files.

## Open
- Mobile data over Yggdrasil is untested; it waits for a dedicated Yggdrasil peer.
- The phone reads many files one at a time; show progress while reading, and read in
  parallel.
- Real two-phone testing waits for a second phone.
