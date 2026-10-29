# derisk-index

derisk's file index: the names and the text of the files under home, and
of any other folders named in Settings, kept current by a user service the
command palette searches.

`derisk-index.service` walks home once, then follows what changes through
inotify, and answers searches on `$XDG_RUNTIME_DIR/derisk/index.sock`
(`derisk-index.socket`). Hidden entries, symlinks and whatever a
`.gitignore`, `.ignore` or git's excludes leave out are not indexed. A file
is also indexed by the words in its first megabyte of text: plain text as
it is, a PDF's through `pdftotext` (poppler, when it is installed), and an
OpenDocument or Office Open XML file's (`.odt`, `.docx`, `.xlsx`, `.pptx`
and their kin) from the XML inside it. The index is under
`~/.cache/derisk-index` and is rebuilt if deleted.

Settings' Privacy page has the two settings it reads from
`settings.conf` when it starts, and restarts it when they change:
`search.index = false` deletes the index and finds nothing, and
`search.folders` names folders to index beside home, such as a second
disk's. A folder that is not there when the service starts, such as a
disk not mounted yet, keeps what was indexed from it until it is back. Enable it with the
graphical session (it brings its socket along):

```console
$ systemctl --user enable --now derisk-index.service
$ derisk-index search quarterly budget
~/notes/q3.md:12: Quarterly budget: rent, power and the new laptop
```

Part of [derisk](https://github.com/losos-project/derisk), where it is built as
a member of the derisk Cargo workspace; see the README there.

Licensed under GPL-3.0-only (see `LICENSE`).
