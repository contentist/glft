# No home folder

## What happened
glft could not find your home folder, so it has nowhere to keep its key, its paired
devices or unfinished transfers.

## Why
The program was started without the usual `HOME` setting, for example from a system
service or a stripped-down environment.

## What to do
- **On a computer:** start glft from your normal desktop session or a terminal. To run it
  elsewhere, set `GLFT_DATA_DIR` to a folder it may use.
- **On the phone:** restart the phone. If it keeps happening, ask for help.

## If you ask for help
Include the log: "Share as text" on the phone, "Copy all" in the window, or `glft log` in
a terminal. It shows each step and the details of the error, and holds no file contents.
