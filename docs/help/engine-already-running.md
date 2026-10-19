# glft's engine is already running

## What happened
A second engine was started while one was running.

## Why
glft runs one engine per user. The second one stops instead of fighting the first over
the network.

## What to do
`glft status` shows the running one; `glft restart` restarts it.
