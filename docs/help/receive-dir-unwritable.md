# The receive folder can't be written

## What happened
glft couldn't write into the folder it receives into.

## Why
The disk is full, the folder belongs to another user, or, on the phone, the folder you
chose for saving is no longer allowed.

## What to do
- **On a computer:** check that `~/Downloads/glft` is yours (`ls -ld ~/Downloads/glft`).
  In a terminal you can choose another folder: `glft receive on --dir FOLDER`.
- **On the phone:** check that there's space. If you chose a folder to save into, choose
  it again in glft.
