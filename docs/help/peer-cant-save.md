# The other device can't save the files

## What happened
The device receiving the files couldn't write into its receive folder, and told this one,
so sending stopped.

## Why
On a computer: the folder belongs to another user, or is on a disk that was removed or is
read-only. On a phone: the folder you chose for saving is no longer allowed.

## What to do
On the receiving device, look at glft's log ("What's happening"): it says which folder
and why. Fix that (see **receive-dir-unwritable**), then send the same files again; the
transfer continues where it stopped.
