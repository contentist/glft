# 0002: Android storage — media store into Download/glft by default

Date: 2026-10-16. Status: decided (Day 2).

## Question
Where does the phone save received files by default, and is writing a folder tree fast
enough? The plan named the folder picker (SAF); review proposed the media store into
`Download/glft` with no setup, with the picker as "save somewhere else".

## How it was measured
A debug-only test in the app (`Node::benchmark_storage`) writes 1,000 files of 64 KB into
10 folders. It goes through the same `ReceiveStorage` calls a transfer makes (create the
partial, write, sync, publish), with 8 files in flight as a transfer has. Device: Pixel 8
Pro, GrapheneOS, Android 17.

| Route | One at a time | **8 in flight** | Breakdown (8 in flight, per file) |
|---|---|---|---|
| **Media store → Download/glft** | 92.8 ms/file | **15.9 ms/file** (1,000 files in 16 s) | create 36 ms, write 0.3 ms, publish 90 ms |
| Direct file paths in Download/glft | 56.1 ms/file | 42.9 ms/file | create 95 ms, write 0.4 ms, publish 247 ms |
| Folder picker (SAF) | — | 66.7 ms/file | create 117 ms, write 0.3 ms, publish 406 ms |

Writing the data is never the cost; creating and publishing entries in Android's storage
service is. The media store handles parallel requests well; direct paths (FUSE) and the
document provider largely serialize them.

## Decision
- **Default: media store, `Download/glft`.** It needs no permission or setup, has the best
  numbers, and files are indexed as they are published. In the end-to-end test, all 17
  FLAC tracks of an album were in the audio index on arrival, so music players see them
  with no rescan.
- **"Save somewhere else": the folder picker.** It is fine for albums (about 1 s for 15
  tracks) and slow for very large folders (about 67 s per 1,000 files). Optimizing it is
  later work.
- **Direct paths are not used.** They work, but they are slower than the media store and add
  nothing.

## Findings worth keeping
- **Name clashes are still detected with the media store.** A folder created by another
  app (adb shell) next to an incoming album made the album arrive as `… (2)`.
  `MediaStoreStorage.exists` checks the path on disk as well as the media store, and
  folders are visible on disk even when their files belong to someone else. The feared
  reinstall case (files owned by an earlier install) still has to be tried, since
  reinstalling wipes the device key.
- **Hard links don't exist on Android's shared storage** (EACCES). `FsStorage::finish` falls
  back to check-then-rename.
- **The folder picker refuses `Download` itself** ("Can't use this folder") but accepts a
  subfolder, as the PDF said.
- **GrapheneOS's Network permission** makes socket creation fail with EPERM, while
  `checkSelfPermission(INTERNET)` still reports it as granted (deliberately, for app
  compatibility). The app detects it by opening a socket and explains it before
  receiving starts; the core reports it as `network-denied`.
- **Android 17's local network permission** appears in the system dialog as "Nearby devices"
  with wording about finding and locating devices. glft shows its own explanation first.

## End-to-end result (Day 2 "done")
Laptop → Pixel 8 Pro, app in the background, phone found by mDNS (no address typed),
accepted from the notification: 1 folder, 23 files, 232 MB in 13.3 s (about 17 MB/s over
Wi-Fi). All 23 files match the laptop by SHA-256, with no partial files left.
