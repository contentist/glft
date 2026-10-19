# glft isn't allowed to use the network

## What happened
This device doesn't let glft use the network at all, so it can't reach other devices.

## Why
On GrapheneOS each app has a **Network** permission, and it is off for glft. On a
computer, a firewall or sandbox is blocking it.

## What to do
- **On the phone:** Settings, Apps, glft, Permissions, **Network**: allow. Then try again;
  there's no need to restart glft.
- **On a computer:** check the firewall and any sandbox glft runs in.
