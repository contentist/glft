# 0003: Engine, QR pairing and resume

Date: 2026-10-17. Status: decided (Day 3).

## Questions
- Where do pairings, offers and unfinished transfers live on the laptop, so they outlive
  one terminal command?
- How do two devices pair, and what does "Remember this device?" mean?
- After a dropped connection, how does a transfer continue without asking again, and how
  fast?

## Decisions
- **A background engine (`glftd`), moved here from Day 5.** "Just this time" pairings,
  resuming after the terminal closes, and the single-instance and heartbeat rules all need a
  process that outlives one command. `glft` is its client over a Unix socket in the user's
  runtime folder. It starts on demand (systemd user socket after `glft setup`, otherwise
  spawned directly) and runs until logout or `glft quit`.
- **Receiving is off until switched on.** While it is on, a desktop notification says so
  and has a Stop button: nothing listens on the network unseen. Sending and pairing work
  with receiving off.
- **Pairing by QR code** (`glft pair` shows it; the phone scans it, or the text is pasted).
  The payload is uppercase base32, `GLFT1/<key>/<secret>/<addresses>`, so the QR code uses
  alphanumeric mode and stays small enough to scan off a terminal. The secret is one-time,
  valid for 5 minutes, and a wrong one leaves the code usable. The device name travels over
  the encrypted connection, not in the code.
- **"Remember this device?" is asked on both sides**: Remember (stored, with auto-accept off
  by default), Just this time (in memory until the engine stops, or until receiving is
  switched off on the phone), or No. Each side is told the other's answer.
- **Pairing is the security boundary; accepting is consent for one transfer.** Offers never
  time out; one the sender abandons is shown as missed.
- **Resume needs no new consent:** the same paired device offering the same manifest (same
  BLAKE3 of the manifest) while an unfinished record exists continues into the same folder.
  Records keep per-file progress; every 8 MB a file is synced and then its verified offset
  recorded, and on resume the partial is cut back to that offset, so an unsynced or torn
  tail is fetched again. Records and partials older than 7 days are removed, matching the
  media store's own expiry for pending files.
- **Verified marks:** on the laptop each received file gets the extended attribute
  `user.glft.blake3`, and `glft verify` re-checks files later; the phone keeps a Received
  list.

## Reconnecting after Wi-Fi loss
A phone back on Wi-Fi usually has a new address: GrapheneOS uses a random MAC address per
connection, so the router hands out a new lease. The phone restarts receiving when its
address changes, keeping port 47382, and announces itself by mDNS.

How long the laptop took to resume after the phone's Wi-Fi came back (Pixel 8 Pro, 400 MB
test folder, Wi-Fi switched off for 30 s during the large file):

| Run | Sender behaviour | Wi-Fi back → resumed |
|---|---|---|
| 2026-10-16 evening | retry the stored addresses | about 4 min |
| 1 | same | 30 s |
| 3–6 | add mDNS addresses to the list as they appear | 3 min 37 s, 10 s, 54 s, 3 min 28 s |
| 7 | **dial only the private IPv4 addresses just announced** | **6 s** (1.1 s after the phone was back on the network) |

A packet capture of run 5 showed why: the laptop spread its connection attempts over every
address it had ever known for the phone (old Wi-Fi leases, mobile-data addresses it cannot
reach, several IPv6 addresses), and the phone's current address got one packet in 43
seconds. The phone answered that packet straight away. So:
- **The sender watches mDNS while not connected.** When the receiver announces a new
  address, the attempt in progress is dropped and only the announced private IPv4
  addresses are dialled. Once connected, new announcements are ignored, so a change in the
  phone's mobile-data address cannot cut a working transfer or repeat an offer.
- **The address that worked is saved** for the next transfer, preferring IPv4: the phone's
  IPv6 addresses change with every reconnect.

## Other phone tests
- **Deep sleep (forced Doze, screen off) mid-file:** the transfer carried on at full speed
  and finished while idle.
- **Offer to a phone already asleep for a minute:** it arrived within a second as a
  notification; accepted 5 minutes later, 400 MB in 60 s.
- **App process killed mid-file** (as Android does under memory pressure): after receiving
  was switched back on, the transfer resumed with 355 MB left, no new prompt, all files
  verified, no partial files or pending media entries left. The laptop noticed the loss
  only after QUIC's 15-second idle timeout, since a killed process sends nothing.
- **Laptop engine killed mid-file:** resumed after restart; the result was byte-identical.

## Findings worth keeping
- **A resumed transfer once reported "verified" on the phone with a file incomplete.** The
  media store gives a pending entry its final name, so the resume check took the
  half-written file for a finished one. `ReceiveStorage::published` now asks whether a file
  is really published; a test storage that behaves like the media store covers it.
- **The laptop's Wi-Fi froze during transfers.** The Realtek RTL8852BE driver fails to set
  up TDLS direct links to the phone and stalls its transmit queues. Each test transfer set
  off a failed TDLS attempt and a stall; with TDLS switched off (see
  `docs/help/wifi-freezes.md`) the next test had none. This is a driver problem any large
  local copy can trigger, not a glft one.
- **The router's 5 GHz radio is unreliable** in this test setup, so all tests ran on
  2.4 GHz: about 5–8 MB/s phone transfer speed.
