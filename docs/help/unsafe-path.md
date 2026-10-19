# An unsafe file path was refused

## What happened
The sender offered a file whose path points outside the receive folder, so glft refused
the whole offer. Nothing was saved.

## Why
glft never writes outside the folder it receives into. A path such as `../something` is
how a damaged or hostile sender could try to overwrite other files.

## What to do
If you trust the sender, ask them to update glft. If you don't know why this happened,
forget that device, and pair it again only if you're sure it's yours.

## If you ask for help
Include the log: "Share as text" on the phone, "Copy all" in the window, or `glft log` in
a terminal. It shows each step and the details of the error, and holds no file contents.
