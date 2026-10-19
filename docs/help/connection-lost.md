# The connection was lost

## What happened
The connection to the other device dropped during a transfer.

## Why
Wi-Fi dropped or changed, a device slept or moved away, or the app on the other side was
closed. Once the other side has accepted, glft keeps trying by itself and continues where
it stopped: files already verified aren't sent again.

## What to do
- Wait: when both devices are back on the same network, the transfer continues by itself.
- If you stopped it, send the same files again. It continues without asking again.
- If the computer's Wi-Fi freezes during transfers, see **wifi-freezes**.

## If you ask for help
Include the log: "Share as text" on the phone, "Copy all" in the window, or `glft log` in
a terminal. It shows each step and the details of the error, and holds no file contents.
