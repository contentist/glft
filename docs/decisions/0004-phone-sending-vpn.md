# 0004: The phone sends; VPN lockdown and Yggdrasil

Date: 2026-10-17. Status: decided (Day 4), two questions open (end of this record).

## Decisions
- **Share menu with a confirm panel.** glft takes one file or many from any app's Share
  menu. A panel shows what is being sent and the paired computers, and nothing is sent
  until a computer is tapped. In the app, "Send files" and "Send a folder" (the folder
  picker) lead to the same panel. A folder arrives whole, empty folders included.
- **Sends run as user-initiated data transfer jobs** with a progress notification and
  Cancel. The job holds the read grants for shared files itself (`JobInfo.setClipData`), so
  it can reopen them after the share panel has closed. A folder's grant is kept
  permanently. Files are opened again for every request rather than held open.
- **Sending the same files again resumes:** files shared by Android give the same manifest
  as files sent by path, so the receiver recognises an interrupted transfer.
- **One button per computer; the route is automatic.** Wi-Fi is tried first, and a mesh
  address (Yggdrasil) is added after 4 seconds, or at once if the local network refuses. A
  per-send "Wi-Fi or Yggdrasil" choice was built, measured and removed (below).
- **glft reports the route that carried the data**, not the one the connection started on:
  "Reaching … over Yggdrasil", "The connection … moved to the local network", and "Sent to …
  over Yggdrasil" at the end.
- **Mesh addresses are kept apart** in the paired-device store and always tried. Before
  this, the address that last worked replaced every saved address, which dropped the
  laptop's Yggdrasil address after one Wi-Fi transfer. Local network announcements (mDNS)
  no longer carry mesh addresses.
- **VPN lockdown is detected and explained:** a card on the main screen says that the VPN
  is set to "Block connections without VPN", what that stops in both directions, and how to
  undo it, with a button to Android's VPN settings. The core turns an unreachable computer
  into the `vpn-lockdown` error, and the log says when glft sends over Yggdrasil instead.
- **The laptop shows "Received from …"** when a transfer arrives, so files accepted without
  asking are noticed.
- **"What's happening" has its own screen,** with error details on tap and "Share as text"
  for help requests.

## Findings worth keeping
- **Android does not report lockdown for Wi-Fi.** Under lockdown, glft's callback for the
  Wi-Fi network said "not blocked". What works: a UDP socket "connected" to the router (it
  sends nothing) fails with "Permission denied" while the local network is blocked. Android
  also blocks every network for an app in the background (blocked reason `0x40`); the VPN
  is then blocked too, which lockdown alone doesn't do, and glft keeps its last answer
  until it can look again.
- **A connection started over Yggdrasil moves to Wi-Fi when both devices share one.**
  Measured on the laptop during a 150 MB "Over Yggdrasil" send on the same Wi-Fi: 25 KB
  arrived on `tun0` (Yggdrasil) and 161 MB on `wlp1s0` (Wi-Fi). iroh's multipath moves to
  the better route by itself, and its settings don't allow turning that off. A per-send
  choice therefore only does something when the devices are on different networks, which
  the automatic route already handles, so the choice was removed.
- **Mixing routes in one connection stalls it under lockdown.** With lockdown on, the first
  connection over Yggdrasil carried the offer and then no data for about 35 seconds, until
  glft reconnected; the reconnect then finished at once. Without lockdown, a connection
  made over Yggdrasil only (an endpoint with no local network lookup) finished 400 KB in
  0.1 s. The likely cause is iroh trying the laptop's Wi-Fi addresses, which lockdown
  refuses; it is not proven.
- **Turning off Android's "Always-on VPN" also stopped the Yggdrasil app's VPN.** glft
  greys out mesh sending when no VPN is running and says so.
- **Before Day 4 the phone had never connected to the laptop;** the laptop always dialled.
  An accidental lockdown made every early test go over Yggdrasil, which is how the
  lockdown handling was found to be needed and then tested.

## Results (Pixel 8 Pro → Inspiron, laptop receiving and accepting without asking)
| Test | Result |
|---|---|
| Share 1 file, Wi-Fi | 300 KB verified in 0.1 s, notification on the laptop within a second |
| Share 10 files | all verified; a name clash arrived as `track01 (2).flac` |
| Send a folder (21 files, 157 MB, an empty folder) | identical tree on the laptop (`diff -r`), over Yggdrasil under lockdown, 57 s |
| Lockdown on | card shown, Wi-Fi greyed out, sent over Yggdrasil and verified (after the 35 s stall) |
| 150 MB, Wi-Fi | 7.8 MB/s |

## Open
- **The stall under lockdown.** Needs iroh's own logging on the phone to prove the cause.
- **Yggdrasil away from home.** With the phone's Wi-Fi off (mobile data only), the
  Yggdrasil app did not reconnect to its public peer within several minutes, although the
  phone could reach the peer's port. So phone-to-laptop over Yggdrasil from another network
  is not shown to work yet. More than one public peer, and checking the app's reconnect
  behaviour, come first.
