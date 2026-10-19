# glft couldn't open a network port

## What happened
glft could not open the network port it uses to send or receive.

## Why
Something on this device stops glft from using the network: a firewall, a sandbox, or on
the phone a permission that is off.

## What to do
- **On the phone:** Settings, Apps, glft, Permissions: turn on **Network**.
- **On a computer:** check the firewall (`sudo ufw status`), and that glft isn't running
  in a sandbox without network access.

## If you ask for help
Include the log: "Share as text" on the phone, "Copy all" in the window, or `glft log` in
a terminal. It shows each step and the details of the error, and holds no file contents.
