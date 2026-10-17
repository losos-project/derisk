# derisk-machines

The person's quickemu machines, as pm's quickemu plugin keeps them under
`$XDG_DATA_HOME/pm/machines` and derisk shows and drives them: their labels,
templates and states from pm's `machine.json` and QEMU's pid file, starting
them as the person's own systemd services, stopping them through QEMU's
monitor, and running programs in them through the QEMU guest agent.

It needs no C library; at run time it uses `systemd-run` and `systemctl`.
On wasm32 it has the labels and the record parsing only.

Part of [derisk](https://github.com/losos-project/derisk), where it is built as
a member of the derisk Cargo workspace; see the README there.

Licensed under GPL-3.0-only (see `LICENSE`).
