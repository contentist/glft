# 0007: The release: reproducible builds, signing, distribution, install steps

Date: 2026-10-19. Status: decided (Day 7). glft 0.2.0 is the first public release.

## Decisions
- **Releases are built by a script, from a clean copy of the tag.** `packaging/release.sh
  VERSION` clones the tag into a temporary folder, builds the .deb, the tarball and the
  APK there, signs the APK, writes `SHA256SUMS` and creates a *draft* GitHub release with
  the notes from `CHANGELOG.md`. A person publishes the draft. `--local` builds and signs
  only, for a test run.
  - Everything that could make the release wrong is checked before anything is built: no
    uncommitted changes, the tag at HEAD and on GitHub, the version in `Cargo.toml`, a
    `CHANGELOG.md` section, every commit by the project's GitHub noreply address, the
    signing key present, and the README naming exactly this version's files (below).
  - It asks for the key's password first and checks it at once, then needs nothing more.
    The password is never written anywhere.
- **The builds are reproducible.** The same commit gives byte-for-byte the same .deb,
  tarball and unsigned APK, wherever it is built (`packaging/common.sh`).
  - No path of the build machine in the programs: `--remap-path-prefix` renames the
    checkout to `/glft` and Cargo's home to `/cargo`, for the computer and for the phone
    library. Before, each program held about 900 such paths, the username among them.
  - File times in the packages come from the commit (`SOURCE_DATE_EPOCH`).
  - All Android build dependencies are pinned by checksum
    (`android/gradle/verification-metadata.xml`, 381 of them).
  - The APK carries only `libglft_ffi.so`. cargo-ndk copied every library the build made,
    including iroh's own and ones left from earlier builds; the app never loaded them. The
    APK is 1.2 MB smaller.
  - Release builds use their own target folder (`target/dist`), so the flags above don't
    force development builds to start over.
- **One signing key for every glft APK.** Its certificate carries only `CN=glft`, nothing
  personal. The SHA-256 fingerprint is in the README, so anyone can check an APK, and
  Obtainium and Android refuse an update signed with any other key. The key is kept
  outside the repository; losing it means no update can ever be installed over glft.
  APKs are signed with build-tools 34's `apksigner`: from build-tools 35 on, it makes
  APKs that F-Droid's tools cannot verify.
- **Distribution: GitHub Releases, followed on the phone with Obtainium.** Stores are a
  bonus. IzzyOnDroid doesn't accept apps written with AI, so it is out. F-Droid has no
  such rule (one is only proposed); the listing waits until the app looks good enough to
  be listed. AppImage and Flatpak are deferred; the tarball covers other distributions.
- **Nothing personal is published.** Commits use the GitHub noreply address; before every
  push and release the author address is checked and the changes are scanned for email
  addresses, real network addresses, host names and keys. Tests use made-up addresses
  (`2001:db8::`, `200:aaaa:…`, `201:1111:…`).
- **On Yggdrasil, the computer is reachable by the whole mesh.** Every service listening
  on the computer's Yggdrasil address can be reached from anywhere on it, not only from
  the user's own devices. Allow only glft (UDP 47382) through the firewall on the
  Yggdrasil interface.
- **The install steps are step by step, with exact file names.** A first version of the
  README only said which file was for which device; it failed the newcomer check (below):
  the reader didn't know which of the four files to download or what to do with them.
  Now the README says the user needs two files of the four, gives the requirements first,
  numbers the steps for the phone and for Mint, Ubuntu and Debian (double-click the .deb;
  the terminal command as the alternative), and names every file exactly. `release.sh`
  refuses to release if the README doesn't name all three of the version's files, or
  still names another version, so the names can't go stale. The release notes start with
  a "Which file?" line.
- **glft 0.2.0 is the finished first version.** It keeps its name. The next product,
  Rata, is built on `main` after it; nothing is renamed until then.

## Results (Inspiron 15 3530, Mint 22.3; Pixel 8 Pro, GrapheneOS)
- **Reproducible:** two complete builds from copies at different paths gave identical
  .deb, tarball and unsigned APK. No build paths in any of them.
- **Release test run** (0.1.5, `--local`): checksums correct, APK signed and verified, the
  .deb's maintainer is the noreply address.
- **As an anonymous visitor**, after publishing: the downloads match `SHA256SUMS` and the
  files built here byte for byte, the APK carries the release certificate, all 44 help
  pages open on GitHub and on the site, and every commit shows only the noreply address.
- **Newcomer check** (the planned "stranger" test, with a separate account and a screen
  recording, was dropped as too complicated): glft was uninstalled from the phone, then
  both sides were installed from the release by following only the README, paired, and
  one file sent each way.

  | Step | Result |
  |---|---|
  | Finding what to download | **failed**: no steps, no file named per device; fixed above |
  | Phone: the APK from the browser | installed; Android didn't ask about installing apps |
  | Phone APK | byte-for-byte the release file |
  | Laptop: the .deb over 0.1.5 | installed; the old engine stopped by the package |
  | Pairing to first file | about 1 minute |
  | Phone install to first file | about 8 minutes, including a stop for the issue below |
  | Files | phone → laptop and laptop → phone, verified |

  Found on the way:
  - **The offer on the phone was easy to miss**: with Do Not Disturb on, it made no sound,
    didn't pop up, and sat low in the notification list. The "Incoming files" channel is
    set right (high importance, default sound); Do Not Disturb silences it.
  - **"Remember this device?"** was answered "Just this time" on the laptop, so the
    pairing would have ended at logout. Paired again with Remember.
  - **An upgrade keeps the old Received list and log** (`arrivals.json`, `log.jsonl` in
    `~/.local/share/glft`), along with the key. A newcomer starts empty; this only
    affects upgrades.

## Open
- The signing key's backup: a copy apart from the computer (a USB stick and a second
  place), and the password checked with `keytool -list`.
- Offers on the phone under Do Not Disturb: show waiting offers prominently in the app,
  say when Do Not Disturb is on, and vibrate (Rata).
- Mobile data over Yggdrasil is untested; it waits for a dedicated Yggdrasil peer.
- Not yet reported to iroh: batched UDP sends under Android's VPN (0004), and the IPv6
  route bias (0006).
- The README's double-click step is checked on Mint only; Ubuntu 24.04 opens .deb files in
  App Center, which has had trouble with downloaded packages. The terminal command covers
  it.
