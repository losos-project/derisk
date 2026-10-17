# derisk-index

derisk's file index: the names and the text of the files under home, kept
current by a user service the command palette searches.

`derisk-index.service` walks home once, then follows what changes through
inotify, and answers searches on `$XDG_RUNTIME_DIR/derisk/index.sock`
(`derisk-index.socket`). Hidden entries, symlinks and whatever a
`.gitignore`, `.ignore` or git's excludes leave out are not indexed; a text
file is also indexed by the words in its first megabyte. The index is under
`~/.cache/derisk-index` and is rebuilt if deleted. Enable it with the
graphical session (it brings its socket along):

```console
$ systemctl --user enable --now derisk-index.service
$ derisk-index search quarterly budget
~/notes/q3.md:12: Quarterly budget: rent, power and the new laptop
```

Part of [derisk](https://github.com/losos-project/derisk), where it is built as
a member of the derisk Cargo workspace; see the README there.

Licensed under GPL-3.0-only (see `LICENSE`).
