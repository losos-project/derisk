# derisk-machines

The libvirt machines on this computer, as derisk shows and drives them: their
labels, templates and states from libvirt's domain XML, starting and stopping
them, and running programs in them through the QEMU guest agent.

It builds against libvirt's C library (`libvirt-dev` on Debian and Ubuntu,
`libvirt` in nixpkgs). On wasm32 it has the labels and the XML parsing only.

Part of [derisk](https://github.com/losos-project/derisk), where it is built as
a member of the derisk Cargo workspace; see the README there.

Licensed under GPL-3.0-only (see `LICENSE`).
