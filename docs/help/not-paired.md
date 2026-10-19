# The devices aren't paired

## What happened
The other device refused the connection because it doesn't know this one.

## Why
Pairing is how glft knows which devices to trust: a device that isn't paired is refused
before anything it sends is read. The pairing may have been forgotten on the other side,
or it was for "just this time" and has ended.

## What to do
Pair again:
1. On the computer, in glft's window, click **Pair a phone…** (or run `glft pair`).
2. On the phone, in glft, tap **Pair with a computer** and scan the code.
3. Answer **Remember** on both.
