# The other device didn't answer

## What happened
glft tried every address it knows for the other device, and nothing answered.

## Why
The usual reasons, most likely first:
1. Receiving isn't switched on there. A device answers only while it receives.
2. The two devices aren't on the same network: the phone is on mobile data, a guest
   Wi-Fi, or a network that keeps devices apart.
3. A firewall on the computer blocks UDP.
4. A VPN on the phone blocks the local network (glft then says so instead; see
   **vpn-lockdown**).

## What to do
- **Sending to a computer:** switch **Receiving** on in glft's window there (or run
  `glft receive on`).
- **Sending to the phone:** switch **Receiving** on in glft on the phone.
- Check that both are on the same Wi-Fi.
- On a computer with a firewall, allow UDP port 47382 from the local network, for example
  `sudo ufw allow from 192.168.0.0/16 to any port 47382 proto udp` (adjust to your
  network).
- If both devices are on a mesh such as Yggdrasil, glft tries it as well; that needs
  Yggdrasil running on both.

## If you ask for help
Include the log: "Share as text" on the phone, "Copy all" in the window, or `glft log` in
a terminal. It shows each step and the details of the error, and holds no file contents.
