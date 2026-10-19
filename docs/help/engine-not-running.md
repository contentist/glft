# glft's engine isn't running

## What happened
The window or the terminal needed glft's background engine (`glftd`), and it couldn't be
started.

## Why
It isn't installed next to `glft`, or it stops straight away because of another problem.

## What to do
In a terminal:
1. `glft setup` sets the engine up again.
2. If that doesn't help, run `glftd` by hand: it prints why it stops.

## If you ask for help
Include the log: "Share as text" on the phone, "Copy all" in the window, or `glft log` in
a terminal. It shows each step and the details of the error, and holds no file contents.
