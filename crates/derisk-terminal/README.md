# derisk-terminal

derisk Terminal: a small terminal emulator with Nerd Font glyphs, drawn on
the GPU through OpenGL ES 3, or in software where there is no GPU. Its
window is `org.derisk.terminal`, and `data/org.derisk.terminal.desktop` is
its desktop entry.

```console
$ derisk-terminal [--renderer auto|gpu|software] [-D DIR] [-a APP_ID] [-T TITLE] [[-e] COMMAND [ARG...]]
```

The README of the derisk workspace says how it draws ("Terminal").

Part of [derisk](https://github.com/losos-project/derisk), where it is built as
a member of the derisk Cargo workspace; see the README there.

Licensed under GPL-3.0-only (see `LICENSE`).
