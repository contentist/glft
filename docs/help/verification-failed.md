# A file didn't match its fingerprint

## What happened
A file arrived different from what was sent, so glft didn't save it.

## Why
Every piece is checked against a BLAKE3 fingerprint as it arrives. A mismatch means the
file changed on the sending side while it was sent or, very rarely, damage on the way.

## What to do
Send again. If it keeps failing, close any program that is still writing the file (a
download, an editor, a camera app), then send.

## If you ask for help
Include the log: "Share as text" on the phone, "Copy all" in the window, or `glft log` in
a terminal. It shows each step and the details of the error, and holds no file contents.
