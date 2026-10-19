# The paired-device list is damaged

## What happened
The list of paired devices can't be read, so glft can't tell which devices to trust, and
trusts none.

## Why
The file was cut short (a crash or a full disk while it was written) or edited.

## What to do
- **On a computer:** move `~/.local/share/glft/devices.json` aside and pair your devices
  again.
- **On the phone:** Settings, Apps, glft, Storage, Clear storage, then pair again.
