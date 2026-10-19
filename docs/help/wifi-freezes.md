# The computer's Wi-Fi freezes during transfers

Seen on computers with a Realtek RTL8852BE Wi-Fi chip.

**What you see:** while glft sends to your phone, web pages on the laptop stop loading for
a while, or other network programs drop their connections. The transfer itself may slow
down or pause and then carry on. Nothing says the Wi-Fi disconnected.

**How to check:** run these after it happens.

```
journalctl -k | grep rtw89
journalctl -u wpa_supplicant | grep TDLS
```

If the first shows `timed out to flush queues` and the second shows
`TDLS: Failed to set TPK to the driver` at the same times, this is the problem.

**What causes it:** when two devices on the same Wi-Fi exchange a lot of data, the laptop
tries to set up a direct link to the other device (TDLS) instead of going through the
router. The Linux driver for this Realtek chip (`rtw89_8852be`) fails to set that link up
and stalls everything the laptop is sending. glft triggers it only because it moves a lot
of data to the phone; a large copy to any device on the network can do the same.

**Fix:** switch TDLS off. Your devices then talk through the router, as most traffic does
anyway. From the glft source folder:

```
sudo install -m 755 -o root -g root contrib/networkmanager/90-no-tdls /etc/NetworkManager/dispatcher.d/
sudo wpa_cli -i wlp1s0 set tdls_disabled 1
```

The first line switches it off each time Wi-Fi connects; the second switches it off now.
Replace `wlp1s0` with your Wi-Fi interface (`nmcli device` lists it). To undo:
`sudo rm /etc/NetworkManager/dispatcher.d/90-no-tdls`, then reconnect.

**If the freezes continue,** the driver's power saving is the next suspect: create
`/etc/modprobe.d/rtw89.conf` with

```
options rtw89_core disable_ps_mode=y
options rtw89_pci disable_aspm_l1=y disable_aspm_l1ss=y
```

and restart. Check that NetworkManager's Wi-Fi power saving is off as well
(`wifi.powersave = 2` in a file under `/etc/NetworkManager/conf.d/`).

On the computer this was found on, every transfer to the phone set off a failed TDLS
attempt and a stall; with TDLS off there were none (see
`docs/decisions/0003-engine-pairing-resume.md`).
