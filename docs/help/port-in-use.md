# The port is taken

## What happened
glft was asked to listen on a particular port, and another program already uses it.

## Why
glft prefers UDP port 47382. When it may choose, it names the program holding that port
in the log and moves to a spare one, which paired devices find by themselves. Only an
explicit choice of port ends in this error.

## What to do
- **In a terminal:** pick another port with `--port`, or leave `--port` out and glft
  picks one. `sudo ss -ulpn | grep 47382` shows what holds it.
- **In the window or on the phone:** switch receiving off and on again.
