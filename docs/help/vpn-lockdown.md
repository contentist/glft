# A VPN is blocking the local network

## What happened
The phone's VPN is set to **Block connections without VPN**. While it is, apps can't reach
your local network in either direction: the phone can't send to the computer over Wi-Fi,
and the computer can't send to the phone.

## Why
Lockdown sends everything through the VPN, and the local network is outside it. Android
doesn't tell apps about this for Wi-Fi, so glft checks for itself and explains it.

## What to do
Any one of these:
- **Switch lockdown off while you transfer:** Settings, Network & internet, VPN, the gear
  next to your VPN, **Block connections without VPN** off. Keep **Always-on VPN** on if
  you use it: switching that off can stop the VPN app (Yggdrasil's does).
- **Use the VPN app's local network option** if it has one (often "Allow LAN" or "Bypass
  LAN"), as well as switching lockdown off.
- **Use a mesh:** if both devices are on Yggdrasil, glft sends over it by itself, even
  under lockdown, and says so. The devices need each other's Yggdrasil addresses, which
  they exchange when they pair while both are on it.

## If you ask for help
Include the log: "Share as text" on the phone, "Copy all" in the window, or `glft log` in
a terminal. It shows each step and the details of the error, and holds no file contents.
