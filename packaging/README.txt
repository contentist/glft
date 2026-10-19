glft: send files directly between your phone and this computer, verified
=========================================================================

glft moves files and whole folders between an Android phone (GrapheneOS or any
Android 14 or newer, with the glft app) and this computer, over your local network,
or a mesh such as Yggdrasil, with no server in between. Every file is checked as it
arrives, and every step is shown in plain words.

Requirements
------------
- Linux on x86-64 with glibc 2.39 or newer (Linux Mint 22, Ubuntu 24.04 or newer,
  Debian 13, Fedora 40 or newer, and others of that age).
- GTK 4.12 or newer for the window. The terminal program and the engine need no GTK.
  Not covered: Debian 12, which has GTK 4.8.
- systemd is optional: with it, the engine starts when glft needs it; without it,
  glft starts the engine itself.

Installing from this download (any distribution)
------------------------------------------------
    ./glft setup

This copies glft into ~/.local/lib/glft, links it from ~/.local/bin, and adds glft to
your menu and "Send to phone..." to Nemo's right-click menu. Nothing listens on the
network until you switch receiving on.

To remove it:

    glft uninstall            stops the engine, then removes everything but your
                              key and pairings, so a reinstall needs no new pairing
    glft uninstall --purge    also removes the key and pairings

Received files are yours and are never removed.

With the .deb (Mint, Ubuntu, Debian)
------------------------------------
    sudo apt install ./glft_VERSION_amd64.deb
    sudo apt remove glft

The package stops any running engine before it replaces or removes its files. So after
an upgrade receiving is off, as it is after every start: switch it on again in the
window, or with `glft receive on`. Unfinished transfers continue once it's on.

Getting started
---------------
1. Open glft from the menu (or run glft-window).
2. Click "Pair a phone..." and scan the code with glft on the phone.
3. Drop files on the window, or right-click them in Nemo, "Send to phone...".

Help
----
Every error says what happened and what to do next, and has a help page:

    glft help                 lists the topics
    glft help <topic>         e.g. glft help peer-unreachable

The window and the phone show the same pages behind each error's Help button.

Checking the download
---------------------
    sha256sum -c SHA256SUMS

Licence: GPL-3.0-or-later. Source: https://github.com/contentist/glft
