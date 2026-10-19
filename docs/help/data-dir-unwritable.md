# glft can't write its data folder

## What happened
glft could not save something in its own data folder: its key, the paired devices, or a
note of how far a transfer got.

## Why
The disk is full, or the folder (`~/.local/share/glft` on a computer) belongs to another
user or was made read-only.

## What to do
- Free some space.
- **On a computer:** check the folder is yours: `ls -ld ~/.local/share/glft`. If it says
  `root`, a command was run with `sudo` by mistake, and
  `sudo chown -R $USER: ~/.local/share/glft` gives it back.
- **On the phone:** free storage in Settings, Storage, then open glft again.

## If you ask for help
Include the log: "Share as text" on the phone, "Copy all" in the window, or `glft log` in
a terminal. It shows each step and the details of the error, and holds no file contents.
