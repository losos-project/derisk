# Welcome to the derisk demo

This is derisk, the desktop of LosOS, running in your browser.

Every app here is the real one, built from the same Rust code that runs on a LosOS machine and compiled to WebAssembly. What a browser cannot give them is stood in for:

- The files you see live in this page's memory. Make, rename, copy and trash them as you like; a reload puts everything back.
- The System Monitor reads a made-up /proc: the processes of a LosOS session on a four-core machine, with load that comes and goes. Ending one takes it out of the list.
- Settings saves to ~/.config/derisk/settings.conf in that same memory, and the desktop picks up the theme and accent from there, as the session does.

Things that need the real machine (installing apps, sign-in methods, opening files in other programs) say they could not run, with the same dialog you would see on LosOS when they fail.
