# glft

glft moves files and folders between an Android phone and a Linux computer over the user's own
network.  The two devices talk to each other directly, with no server or account in
between.  It checks each file as it arrives, and it keeps a plain-language log of what it's
doing, so if something goes wrong the user can see where.

It's built with GrapheneOS in mind, and it should work on any phone running Android 14 or newer.

## What it does

- glft sends single files or whole folders in either direction, and it keeps the folder
  structure as it was.
- It checks every file piece by piece against what was sent (using BLAKE3), so the user knows
  the file arrived intact.
- If the Wi-Fi drops, the phone goes to sleep or the app gets closed, the transfer carries
  on from where it stopped.
- It tells the user where each file ended up on both devices, and it lets the user open the file or
  its folder from there.
- It only talks to devices the user has paired with, and it turns anything else away before
  reading what it sent.  Transfers are encrypted with keys that belong to the two devices.

On the computer, there's a window the user can drop files onto, a "Send to phone…" entry in
Nemo's right-click menu, an icon in the panel while glft is receiving, and a terminal
program called `glft`.  On the phone, glft appears in the Share menu and can receive files
in the background.

## Installing

The user needs two files from the
[Releases page](https://github.com/contentist/glft/releases/latest): one for the phone and
one for the computer.  They're listed under **Assets** at the bottom of the release.  The
other files aren't needed.

The computer needs 64-bit Linux (x86-64) with GTK 4.12 and glibc 2.39 or newer.  That covers
Linux Mint 22, Ubuntu 24.04, Debian 13 and Fedora 40 or later.  Debian 12 is too old.

### Phone

1. On the phone, open the
   [Releases page](https://github.com/contentist/glft/releases/latest) in the browser.
2. Under **Assets**, tap **`glft-0.2.0.apk`** to download it.
3. Open the downloaded file, from the browser's download message or from the Files app.
4. If Android asks whether the browser may install apps, allow it, then go back.
5. Tap **Install**, then **Open**.

If the user would like updates to arrive on their own, add
`https://github.com/contentist/glft` to [Obtainium](https://obtainium.imranr.dev/) instead
of steps 1 to 5.

Every glft APK is signed with the same certificate.  If the user wants to check it, its SHA-256
fingerprint is:

```
83:89:1A:64:68:02:0F:41:98:9F:D1:92:77:0B:F2:9F:7C:40:D1:BA:1A:AF:08:FC:0D:C1:DC:9F:53:76:07:94
```

### Computer (Linux Mint, Ubuntu, Debian)

1. On the computer, open the
   [Releases page](https://github.com/contentist/glft/releases/latest).
2. Under **Assets**, click **`glft_0.2.0_amd64.deb`**.  It goes into the Downloads folder.
3. Open the Downloads folder and double-click the file.  The package installer opens.
   Click **Install** and enter the computer's password.
4. glft is now in the menu.

In a terminal, this command does the same as steps 3 and 4:

```
sudo apt install ~/Downloads/glft_0.2.0_amd64.deb
```

### Computer (other Linux distributions)

Download **`glft-0.2.0-x86_64-linux.tar.gz`** instead of the `.deb`, then unpack it and run
its setup:

```
cd ~/Downloads
tar xf glft-0.2.0-x86_64-linux.tar.gz
cd glft-0.2.0-x86_64-linux
./glft setup
```

This installs glft into the user's home folder (`~/.local`), so it doesn't need root.  To remove
it later, run `glft uninstall`.

### Checking the downloads (optional)

If the user wants to check the downloads, put `SHA256SUMS` in the same folder as them and run
`sha256sum -c SHA256SUMS`.

## Pairing

The user only needs to do this once for each phone and computer.

1. On the computer, open glft from the menu and click **Pair a phone…**.  A QR code will
   appear.
2. On the phone, open glft, tap **Pair with a computer** under Paired devices, and scan the
   code.
3. Both devices will then ask, "Remember this device?"  Choose **Remember** on each one.

## Sending files

glft doesn't listen for anything until the user turns **Receiving** on.  Before the user sends, turn it
on in glft on the device that will get the files.

To send from the phone to the computer, turn Receiving on in the glft window on the
computer.  Then, on the phone, share files to glft from any app, or open glft and use
**Send files** or **Send a folder**.

To send from the computer to the phone, turn Receiving on in glft on the phone.  Then drop
files onto the glft window on the computer, or right-click them in Nemo and choose
**Send to phone…**.

Received files go into `~/Downloads/glft` on the computer and into `Download/glft` on the
phone.

Both devices need to be on the same Wi-Fi network, or both need to be connected to a mesh
network such as [Yggdrasil](https://yggdrasil-network.github.io/).  glft uses UDP port
47382, in case the user needs to allow it through a firewall.

## If something goes wrong

glft was written to be transparent about the processes it invokes and to provide as much relevant information as a user may need when learning to use the product or when troubleshooting a file transfer.

Each error message explains what happened and what to try next, and each one has a help
page.  The user can open the page with the Help button beside the message, by running
`glft help <id>` in a terminal, or from the [help pages](docs/help/README.md).

Please be aware: If the VPN on the user's phone has "Block connections without VPN" turned on, Android stops every app from reaching the local network, and that includes glft.  glft will tell the user when this is happening.  If both devices run Yggdrasil, glft can send over Yggdrasil instead, and the setting can stay on.

## Building it from source

The Rust toolchain version is pinned in `rust-toolchain.toml`, and `cargo test` runs the
tests.  `packaging/build-deb.sh` and `packaging/build-tarball.sh` build the packages for the
computer, and `./gradlew assembleRelease` in the `android/` folder builds the phone app.
To build the phone app, the user will need the Android SDK, NDK 30.0.16248370 and `cargo-ndk`.

The builds are reproducible, which means the same commit produces the same .deb, tarball
and unsigned APK wherever it's built.  If the user is curious why things are built the way they
are, the reasoning is written up in
[docs/decisions](https://github.com/contentist/glft/tree/main/docs/decisions).

## Licence

glft is free software, released under the [GPL-3.0-or-later](LICENSE).
