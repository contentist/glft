# This device's key is damaged

## What happened
Each device proves who it is with a key made the first time glft runs. This device's key
file can't be read, so glft can't prove it is the device your others paired with.

## Why
The file was cut short (a crash or a full disk while it was written) or edited.

## What to do
- **On a computer:** move `~/.local/share/glft/secret.key` aside (keep it, in case it can
  be recovered) and start glft again. It makes a new key.
- **On the phone:** Settings, Apps, glft, Storage, Clear storage.
- Either way the other devices now see a new device: pair them again, and forget the old
  entry on each.
