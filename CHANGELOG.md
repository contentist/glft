# Changes

## 0.2.0

The first public release.

glft sends files and whole folders directly between an Android phone (GrapheneOS or any
Android 14 or newer) and a Linux computer, over your local network or a mesh such as
Yggdrasil, with no server in between.

- **Verified:** every file is checked with BLAKE3 as it arrives.
- **Resumes by itself** after Wi-Fi drops, the phone sleeps or the app is closed, without
  asking again.
- **Says where every file landed,** on both devices, with one click to open it or its
  folder.
- **Pairing by QR code:** devices that aren't paired are refused before anything they
  send is read.
- **On the computer:**
  - a window (drop files on it);
  - "Send to phone…" in Nemo's right-click menu;
  - a panel icon while receiving;
  - the `glft` terminal program.
- **On the phone:**
  - glft in the Share menu;
  - receiving in the background;
  - several sends queue up.
- **Errors in plain words,** each with a next step and a help page, which work offline.
- **VPN lockdown is explained,** and glft sends over Yggdrasil instead.

Downloads:
- `glft-0.2.0.apk`: the phone app, or add this repository to Obtainium.
- `glft_0.2.0_amd64.deb`: for Mint, Ubuntu and Debian (needs GTK 4.12 and glibc 2.39).
- `glft-0.2.0-x86_64-linux.tar.gz`: other distributions; unpack it and run `./glft setup`.
- `SHA256SUMS`: check the downloads with `sha256sum -c SHA256SUMS`.

The builds are reproducible: building this tag gives the same .deb, tarball and unsigned
APK.
