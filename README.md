# derisk

An adaptive, agent-first Wayland desktop environment built on
[mcsapi](https://github.com/dasmatus/mcsapi) that adapts to you.

derisk takes the good parts from many desktops:

| Feature | Inspired by |
| --- | --- |
| Automatic tiling (mcsapi `Tall`/`Monocle` layouts) with floating windows | tiling WMs |
| Drag a title bar to an edge or corner to snap; top edge maximizes; Super+arrows; Snap Assist offers the other half | Windows |
| Window buttons (close, minimize, maximize) on the **left** of the title bar | macOS |
| Global menu in the top bar (app menus + a Window menu) | macOS, KDE |
| Overview with workspaces, an exposé grid and widgets (clock, suggestions, failed units, calendar, battery, notes) | GNOME, iPadOS |
| Monochrome systray icons recolored to the theme | macOS, GNOME |
| Adaptive profiles: phone (monocle, no gaps), tablet, desktop | mobile shells |
| App suggestions learned from when you launch apps | Android, iOS |
| Startup animation: logo pops in, ring sweeps, then blooms into the desktop (cross-fade with reduced motion) | — |
| Command palette (Super+Space) for apps, app actions, windows, commands, system actions and files, hosting the agent conversation | Raycast, Spotlight, VS Code |
| Built-in assistant and an agent protocol | agent-first |
| Deep systemd integration | — |

## Command palette

Super+Space (or the search field in the top bar) opens one box for
everything. Type to search, in a single ranked list:

- **Apps** from their `.desktop` files: every installed application
  (XDG data dirs, user files first, `Hidden`/`NoDisplay`/`OnlyShowIn`
  respected) and the core apps, plus suggestions learned from your habits.
- **App actions** from each `.desktop` file's `[Desktop Action]` groups,
  such as Firefox's "New Private Window". The core apps ship their own
  `.desktop` files (`crates/derisk-apps/data`), so "Appearance" opens
  Settings on that page and "Downloads" opens Files there. From a terminal:
  `derisk launch org.derisk.settings --action power`.
- **Windows** on every workspace; choosing one switches there and focuses it.
- **App commands**: every item in the focused app's global menus
  (`register_menu`), so apps get palette commands for free.
- **Commands**: window management (snap, maximize, float, tile, close),
  overview, layouts, focus, workspaces and moving the window between them.
- **System actions**: lock, suspend, hibernate, log out, reboot, power off
  (destructive ones need a second Enter), tray items and failed units
  (restart or dismiss). The palette is their home: there is no separate
  system menu, and the top bar's ⚠ count opens the palette on them.
- **Tabs and extensions** of a browser that registers them: Danube's
  tabs (switch, close, new tab) and its extensions (enable, disable,
  options). An address typed into the palette opens in Danube, or in the
  default browser when Danube is not running.
- **Files** under your home folder.

Anything else goes to the assistant: `open firefox and snap it left`,
Enter, done. When a typed sentence is something the assistant understands
and no entry title contains all its words, the assistant row comes first.

**The palette is where the agent lives.** A request opens its
conversation view: what you asked, each planned step with its progress
(✔ done, ⟳ waiting for a launched app's window, ✖ failed and why), and
the outcome. Keep typing to follow up; Backspace on an empty field returns
to search, `?` opens the conversation directly, and "New" starts over.
Requests from external agents (`ask` and `dispatch` over the agent
protocol) appear in the same conversation, and the panel shows a ✨ count
of agent activity you haven't seen (or ✨ while a request is still
working); click it to open the conversation. Typing on the overview opens
the palette with what you typed.

Prefixes narrow the list: `>` commands, `@` windows, `/` or `~` files,
`?` ask the assistant. ↑/↓ (or Tab, Ctrl+N/P) select, Enter runs, Esc
closes. Picks you use often rise to the top. Agents can open and close it
with `{"action":"palette"}` and open files with `{"action":"open","path":...}`
(absolute paths only; executables and `.desktop` files are refused).

### Plugins

Every palette row and every overview card comes from a plugin: a
WebAssembly component built against
`crates/derisk-plugin/wit/palette/palette.wit` or
`crates/derisk-plugin/wit/widget/widget.wit`. The palette core only ranks,
remembers and draws rows; the overview only lays out and draws cards.

A palette plugin answers `entries` (its catalog, asked again only when the
part of the desktop it reads changes) and `query` (rows for the typed
text, such as the assistant's). A widget plugin answers `render` with
cards made of semantic parts: text in a tone (normal, dim, accent,
danger) and an emphasis (normal, strong, display), progress bars, rows of
buttons and apps, grids and text fields. It never chooses a color, a font
or a size in pixels, so the theme draws every card, and restyling derisk
restyles every widget. The clock, calendar, battery, suggested apps,
failed services and notes are widget plugins.

The bundled plugins are in `plugins/palette/` and `plugins/widgets/`, one
crate each, built by `derisk-plugin`'s build script (`rustup target add
wasm32-unknown-unknown`).

Both kinds run the way [pm](https://github.com/losos-project/pm) runs its
own plugins: in derisk's process, with no WASI, only a one-way log (and,
for the palette, the built-in assistant) to call, a fuel budget per call,
capped memory, and a fresh instance every time. A plugin's manifest names
the parts of the desktop it may see (one that does not ask for files never
sees a file name) and the action kinds its rows or buttons may run, and,
for the palette, the categories of its rows; anything outside it is
dropped. Rows that log out, reboot or power off always need the second
Enter.

Besides the bundled ones, derisk loads plugins from
`derisk/palette-plugins/*.wasm` and `derisk/widget-plugins/*.wasm` under
`$XDG_DATA_DIRS` when root owns them and nobody else can write them, and
from the same folders under `$XDG_DATA_HOME` when each carries a detached
`.sig` (pm's format: `pm sign plugin.wasm`) from a key in
`$XDG_CONFIG_HOME/derisk/trusted/`. A plugin cannot take a name already
loaded for its kind.

Programs feed palette plugins over the agent protocol with
`{"method":"register_palette","source":"danube","data":{...}}`; the
connection owns the data and hears of picks from the rows made of it as
`{"event":"palette","source":"danube","command":{...}}` (see
`plugins/palette/browser`). Cards a program registers with
`register_widget` are drawn by the bundled `programs` widget plugin.

## Agent-first

Everything the shell can do is an `Action` (see `crates/derisk/src/action.rs`). The UI,
keyboard, assistant and external agents all go through the same actions, so
anything a person can do an agent can do too, under the same rules.

- **Assistant**: type in the command palette, e.g.
  `open firefox and snap it left, then go to workspace 2`.
  Try it from a terminal with `derisk ask open firefox and snap it left`.
- **Agent protocol**: JSON lines over stdio or a Unix socket.
- **MCP**: `derisk mcp` serves the same tools to MCP clients over stdio,
  each call going to the running session. Sonne lists it as a context
  server whenever `derisk` is installed, so its agent, with whichever model
  key or agent CLI is set up in Sonne's settings, can drive the desktop.
  Requests the assistant does not understand are offered to that agent in
  the palette ("Ask Sonne's agent"); they open in Sonne's agent panel for
  the person to send.

```console
$ derisk agent
{"method":"tools"}                    # MCP-style tool list with JSON schemas
{"method":"state"}                    # windows, workspaces, menus, tray, clock, failed units, ...
{"method":"ask","text":"open kitty and snap it right"}
{"method":"dispatch","actions":[{"action":"snap","zone":"left"}]}
{"method":"register_menu","window":1,"menus":[{"title":"File","entries":[{"kind":"item","id":"open","label":"Open"}]}]}
```

Responses are `{"ok":true,"result":...}` or `{"ok":false,"error":"..."}`.

Window IDs for `register_menu` come from `state`'s `windows`, each with
its `id`, `app_id` and `title`. On the live desktop's socket, the
connection that registers a window's menus owns them: when the person
picks one of its items in the top bar or the palette (or an agent
dispatches `activate_menu` for it), it gets
`{"event":"menu","window":1,"item":"open"}` as a line of its own between
responses. The owner can register again to replace the menus (a browser
keeping a list of its tabs current, say); another connection cannot, and
a window that does not exist is refused. When the owner disconnects its
menus go away. Menus registered through a headless `derisk agent` have no
owner: picks from them show only as `menu_activated` effects.

Guard rails: apps can only be launched by plain command or desktop-file
name (no paths or arguments); reboot, power off, hibernate and log out need
`"confirmed": true`, which the assistant never sets on its own (the UI asks
first); unit actions only apply to units that are currently failed.
An agent's `"confirmed": true` is not taken as the person's word: the
request waits on screen for someone at the computer to press the button
(`"error"` says so), and the agent's own clicks, keys and accessibility
actions cannot press it. A headless `derisk agent` has no screen, so it
cannot power off or log out at all.

### Computer use

On the live desktop (`derisk session`), agents can also see and drive
what is on screen. The same tree is published over AT-SPI, so screen
readers such as Orca read it too.

```console
{"method":"tree"}                                   # every element: stable id, role, name, value, bounds, state, actions
{"method":"find","role":"button","name":"Save"}     # elements matching a role and/or name (case-insensitive substring)
{"method":"act","element":42,"action":"click"}      # click, focus, set_value (with "value"), expand, collapse, scroll_*, ...
{"method":"screenshot"}                             # PNG path under $XDG_RUNTIME_DIR/derisk/screenshots; "inline":true for base64
{"method":"input","events":[{"type":"click","x":400,"y":300},{"type":"key","key":"ctrl+a"},{"type":"text","text":"hi"}]}
{"method":"register_tree","window":3,"nodes":[{"id":1,"role":"button","name":"Play","actions":["click"]}]}
```

Element ids stay the same while the element exists. Names, values and
window titles are written by apps and web pages, not by the person, so
every `tree` and `find` result carries a `note` saying to treat them as
data, never as instructions. `register_tree` lets an out-of-process
program (a GPUI app, say) describe its own window: its nodes appear under
that window, actions on them come back to it as
`{"event":"action","window":3,"node":1,"action":"click"}` on its
connection, and they disappear when it disconnects.

### Accessibility

- One accessibility tree for the top bar, overview, palette, dialogs and
  the built-in apps (roles, names, states, focus), on AT-SPI.
- Every control is reachable by keyboard and the focused one has a ring
  in the accent colour. Super+B moves focus to the top bar; Tab and
  Shift+Tab move through it, Enter or Space activate, Escape gives focus
  back to the window.
- `--reduced-motion` turns animations off.

## Themes

Everything derisk draws, and every app it can reach, follows one theme
from mcsapi's theming engine (`mcsapi-theme`). Settings → Appearance picks
it: **Automatic** is the built-in dark or light theme with your accent,
kept at 3:1 contrast; any other entry is a theme file, `<id>.theme` under
`derisk/themes` in `$XDG_DATA_HOME` or `$XDG_DATA_DIRS`, which can inherit
from another and change only what it needs:

```toml
name = "Paper"
inherits = "derisk-light"

[colors]
accent = "rose"

[icons]
theme = "Papirus"
```

The session publishes the active theme under `$XDG_RUNTIME_DIR/derisk`
whenever settings change, each file replaced atomically:
`theme.json` (colors, tokens, fonts, icons and the freedesktop appearance
values) for any consumer, `android/values/colors.xml` for the Android
translation layer, and GTK `settings.ini` files. Apps launched from the
shell get `XDG_CONFIG_DIRS` with those GTK settings first (your own
`~/.config/gtk-*` still wins) and `XCURSOR_THEME`/`XCURSOR_SIZE`.
`x2mcsapi --theme <id>` restyles web, Electron, GTK and Qt apps from the
same theme.

## systemd integration

- **Apps run as units.** Each launch is a transient user service
  `app-derisk-<app>@<n>.service` in `app-graphical.slice`
  (`systemd-run --user`, `Type=exec`, `ExitType=cgroup`), following the
  systemd desktop-environment conventions, so every app gets its own cgroup,
  resource accounting and journal stream.
- **Focus boost.** The focused app's unit gets more CPU and IO weight
  (`systemd::FocusBoost`, with the unit found from the client's cgroup via
  `systemd::unit_of_pid`; called by the compositor host on focus changes).
- **Session target.** `derisk-session.target` binds `graphical-session.target`
  and pulls in XDG autostart; the compositor exports `WAYLAND_DISPLAY` and
  friends to the user manager and D-Bus activation environment first
  (`systemd::session_start_argv`).
- **Socket-activated agent.** Without a desktop, `derisk-agent.socket`
  listens on `$XDG_RUNTIME_DIR/derisk/agent.sock` (mode 0600) and starts
  `derisk-agent.service` (`Type=notify`, watchdog, hardening) on demand. A
  `derisk session` serves that socket itself against the live desktop, so it
  stops the headless agent first and the socket unit conflicts with
  `derisk-session.target`.
- **sd_notify, watchdog and journald.** Readiness, status, watchdog
  keep-alives and stopping are reported to the service manager; logs are
  `tracing` events, which go to the journal with structured fields through
  tracing-journald (stderr where no journal is running). `RUST_LOG` filters
  them, `info` and up by default.
- **logind.** Lock, suspend, hibernate, log out, reboot and power off from the
  command palette, assistant or agents, acting on this session
  (`XDG_SESSION_ID`): Log out ends the logind session, not just the target.
- **Lock screen.** Locking hides every window and sends every key to a
  field that answers PAM (the `derisk` service, `crates/derisk/data/pam.d/derisk`)
  through a `derisk auth` child process speaking greetd's protocol, so
  whatever the stack asks for is asked: the password, a security key's PIN
  and touch (systemd-homed's FIDO2 unlock), a verification code. When
  `/etc/pam.d/derisk-fingerprint` is installed, a second conversation
  listens to the fingerprint reader beside the field, and whichever PAM
  accepts first unlocks. An expired password still unlocks a running
  session, with a reminder; the next login asks for a new one. With `--execute` the session also locks when logind asks it to (`loginctl
  lock-session`, `lock-sessions`, `busctl wait` on the session's `Lock`
  signal) and sets logind's `LockedHint`. While locked every agent request,
  `tree`, `screenshot` and `input` included, gets `"the session is locked"`,
  and the AT-SPI tree is empty and ignores actions, since window titles
  would show through the lock.
- **Idle.** With `--execute` the session dims the screen, locks it and
  suspends after the minutes Settings › Power sets, and locks before the
  machine sleeps. It learns when nobody is using it from swayidle, which it
  runs for the purpose and which tells it rather than acting itself
  (`ext_idle_notifier_v1`); swayidle also keeps logind's idle hint. A
  playing video or anything else holding `zwp_idle_inhibit_v1` on a
  visible window keeps all of that from happening, and the top bar says
  an app is keeping the screen on.
- **Other screen lockers.** swaylock or any `ext_session_lock_v1` locker
  can lock the session; derisk sets logind's `LockedHint` for it and does
  not lock again underneath.
- **Windows asking for attention.** A window that rings the bell
  (`xdg_system_bell_v1`) or asks to come forward with an activation token
  while the palette, a polkit prompt or a lock has the keyboard gets its
  app's icon, ringed, in the top bar until it is focused, and marks its
  workspace urgent for pagers. Otherwise an activation focuses it.
- **Dialogs.** A window that names a parent (`xdg_toplevel.set_parent`, or
  through `xdg-foreign` as a portal's file chooser does) floats centred on
  it instead of taking a tile; a modal one (`xdg_dialog_v1`) takes its
  parent's focus and clicks until it closes.
- **Pagers.** The open workspaces are listed through `ext_workspace_v1`,
  numbered as derisk numbers them, and a pager can switch between them.
- **Screen sharing.** xdg-desktop-portal-wlr serves the ScreenCast
  portal over the compositor's capture protocols, and asks which screen or
  window to share through `derisk choose`, a dialog in the session that
  reads xdg-desktop-portal-wlr's dmenu-style list on standard input and
  prints the line picked. Its config names it:
  `[screencast]` `chooser_type=dmenu`, `chooser_cmd=derisk choose`.
- **polkit agent.** With `--execute` the session registers as polkit's
  authentication agent for its logind session, so run0, system-wide app
  installs and fingerprint enrolment can ask someone to authenticate.
  Each request dims the desktop under a dialog that relays what PAM's
  `polkit-1` stack asks through `polkit-agent-helper-1`'s socket
  (`/run/polkit/agent-helper.socket`): the fingerprint reader, a password or
  security key PIN, a code. Every key goes to the dialog while it is open,
  and input from agents can cancel it but not authenticate.
- **Display manager.** `derisk display-manager` replaces gdm: run as root
  from a system service, it starts `derisk greeter` (the lock screen as a
  login screen) on a VT as an unprivileged user, checks the password it is
  given with PAM (`crates/derisk/data/pam.d/derisk-login`), and then opens the user's PAM
  session, so pam_systemd registers it with logind and pam_systemd_home
  unlocks a homed home area, and runs the session command as the user. An
  expired password is changed in the same conversation before the session
  starts, and the account's homed record is brought up to the system's
  password age (`/etc/derisk/sign-in.conf`, `password.max_age_days` and
  `password.warn_days`), which first-boot setup also creates accounts
  under. When
  the session ends the greeter comes back. The root half draws nothing; the
  greeter talks to it over greetd's protocol on a socket only the greeter
  user can open, so `derisk greeter` also runs unchanged under greetd. The
  greeter and the session each drive the display themselves, through
  DRM/KMS, libinput and logind, whenever there is no Wayland or X11 session
  to nest in (`MCSAPI_BACKEND=kms` or `=winit` decides instead):

  ```console
  # derisk display-manager --vt 1 -- \
      derisk greeter -- derisk session --execute
  ```

  It needs a `derisk-greeter` system user and the two PAM services in
  `crates/derisk/data/pam.d`.
- **Failed units widget.** Failed user units show in the top bar and overview
  with restart and reset actions.

Install the units:

```console
$ cargo install --path crates/derisk
$ cp crates/derisk/data/systemd/user/* crates/derisk-portal/data/systemd/user/* ~/.config/systemd/user/
$ systemctl --user daemon-reload
$ systemctl --user start derisk-agent.socket   # headless only; a session serves the socket itself
$ sudo install -m644 crates/derisk/data/pam.d/derisk /etc/pam.d/derisk   # for the lock screen
$ sudo install -m644 crates/derisk/data/pam.d/derisk-fingerprint /etc/pam.d/   # the lock screen's fingerprint reader (fprintd)
$ sudo install -m644 crates/derisk/data/pam.d/derisk-login crates/derisk/data/pam.d/derisk-greeter /etc/pam.d/   # for the display manager
```

## Usage

```console
$ cargo run --release --features host -- session --launch foot
```

`derisk session` runs the desktop as a Smithay compositor nested in a window
of your current X11 or Wayland session. The core apps (Files, Settings, Text
Editor, System Monitor, Calculator, Machines, in `crates/`) run inside
the session and launch by name: `--launch files`, `derisk do open calculator`,
or the command palette. Wayland apps launched from it (or from any
terminal with `WAYLAND_DISPLAY` set to the socket it prints) get derisk's
title bars, tiling, snapping, overview and assistant. The agent protocol is
served on `$XDG_RUNTIME_DIR/derisk/agent.sock` against the live desktop:

```console
$ derisk do open files and snap it right   # natural language, from any terminal
$ derisk send '{"method":"state"}'           # raw agent protocol
```

Session options: `--launch <app>` (repeatable), `--size WxH`,
`--socket PATH`, `--reduced-motion`, and `--execute` to launch apps as
systemd units and run logind session operations.

### Keyboard

| Chord | Action |
| --- | --- |
| Super+Space | Command palette |
| Super (tap), Super+A | Overview (typing opens the palette) |
| Super+←/→/↑/↓ | Snap halves and quarters, maximize, restore, minimize |
| Super+1…9 / Super+Shift+1…9 | Switch workspace / move the window there (one past the last opens a new workspace) |
| Super+J / Super+K, Alt+Tab | Focus next / previous |
| Super+Enter | Promote to the main pane |
| Super+Q | Close |
| Super+F / Super+T | Float / tile |
| Super+M / Super+Shift+M | Monocle / tall |
| Super+B | Focus the top bar (Tab moves on, Escape returns to the window) |
| Escape | Leave the overview |

Settings → Shortcuts rebinds the single-chord ones (palette, overview,
focus, promote, close, float, tile, layouts) or turns them off; the
families (Super+digits, Super+arrows, Alt+Tab) stay fixed.

### Wallpaper

Settings → Wallpaper picks derisk's gradient, one color, a two-color
gradient, a PNG/JPEG/WebP picture (fill, fit, stretch, center or tile), a
slideshow of a folder's pictures (every 1 to 1440 minutes, in order or
shuffled), or a looping video. Videos play silently through an `ffmpeg`
child process (`$DERISK_FFMPEG`, else `ffmpeg` on `PATH`) at 30 fps, at most
1080p, and pause while a window fills the screen and in low power mode.
Anything that fails to load falls back to the gradient and is logged.
The page previews the choice and shows thumbnails of what it finds in
Pictures, Videos and the system backgrounds.

### Privacy

Settings → Privacy turns location, camera and microphone off for every
Flatpak app (it denies them in the portal permission store on Save; turning
one back on lets apps ask again), stops the recently used files list, and
empties trash older than 1 to 365 days. Under it, each installed Flatpak
app's network, display, sound, device, folder and session-bus access can be
switched; derisk writes these as `flatpak override --user` does, to
`$XDG_DATA_HOME/flatpak/overrides/<app>`, so they apply the next time the
app starts. Its portal answers (camera, microphone, location, background,
notifications, screenshots) are set to Ask, Allow or Deny through
`flatpak permission-set` (`$DERISK_FLATPAK`, else `flatpak` on `PATH`).

### Portal

`xdg-desktop-portal-derisk` (`crates/derisk-portal`, on
[ashpd](https://github.com/bilelmoussaoui/ashpd)'s backend traits) is the
xdg-desktop-portal backend for a derisk session. It serves what only the
session knows, and `crates/derisk-portal/data/portal/derisk-portals.conf` leaves everything else
(file chooser, access dialog, printing, ...) to the GTK backend:

| Portal | What it does |
| --- | --- |
| Settings | `org.freedesktop.appearance` from the published `theme.json`, so GTK 4, Qt, Firefox and Electron apps follow dark mode and the accent, live |
| Screenshot | The whole screen from the compositor, over the agent socket, saved to Pictures/Screenshots |
| Wallpaper | Sets a picture as the background through `settings.conf`, which the session reloads |
| Background | Which apps have windows, over the agent socket; apps running without one are allowed for that run and nothing is stored |
| ScreenCast | Not derisk's: `derisk-portals.conf` gives it to xdg-desktop-portal-wlr (above, Screen sharing) |

xdg-desktop-portal asks before a non-interactive screenshot or a wallpaper
without a preview. The interactive and preview cases are the backend's to
confirm, and it asks with derisk's own consent dialog, `derisk-gpui ask`
(an mcsapi `NativeDialog` on the desktop theme; `$DERISK_PORTAL_DIALOG` names
another program). Where `derisk-gpui` is not installed it asks through the GTK
backend's access dialog instead (`$DERISK_PORTAL_ACCESS` names another backend). There is no area picker or color
picker yet, and the lock screen draws its own gradient, so a wallpaper for
the lock screen alone is refused.

Install `crates/derisk-portal/data/portal/derisk.portal` to `share/xdg-desktop-portal/portals`,
`derisk-portals.conf` to `share/xdg-desktop-portal`, the D-Bus service to
`share/dbus-1/services` and the unit to `share/systemd/user`, with `Exec=` and
`ExecStart=` made absolute; `nix build` does.

### Installer and first-boot setup

`derisk installer` and `derisk setup` run on the bare seat like the login
screen, as root, and draw one card of pages over the wallpaper with mcsapi's
components (`mcsapi-components`), so restyling those restyles both. On a
phone the card fills the screen and the on-screen keyboard comes up under a
focused field; elsewhere the keyboard button in the corner brings it up.

```console
$ derisk setup [--force] [--dry-run] [--size WxH]
$ derisk installer [--size WxH] -- <backend command...>
```

`derisk setup` is the first boot: language (`localectl list-locales`),
keyboard layout (xkeyboard-config's `base.lst`, live while picking, with a
field to try it), time zone (`timedatectl`), network, and the first account.
It saves them with `localectl set-locale`, `localectl set-x11-keymap`,
`timedatectl set-timezone` and `homectl create … --member-of=wheel`, the
password passed in `NEWPASSWORD` rather than on the command line, and exits;
when a regular user already exists it exits at once, so a system can start
it before the display manager on every boot. The display manager reads the
saved layout back from localed's file and sets `XKB_DEFAULT_*` for the login
screen and the session. `--dry-run --force` walks the pages without saving.

`derisk installer` draws the pages and joins networks; the disk work is a
backend's, started from the command after `--`, which speaks JSON lines
(`crates/derisk-install/src/install.rs`): it says hello with the system's name and source, lists
disks, installs one with its steps and output, and reboots. LosOS's backend
is `losos-installer serve`. The installer names the disk and asks once more,
with the only red button, before anything is erased.

Both share the Network page: wired ports as networkd sees them and Wi-Fi
over wpa_supplicant's control socket (`crates/derisk-install/src/network.rs`, as NixOS runs it with
`userControlled`). The setup saves a network it joins (`SAVE_CONFIG`).

### Headless commands

```console
$ derisk demo          # headless walkthrough: tiling, drag-to-snap, snap assist, assistant, startup animation, UI render
$ derisk ask <text>    # how the assistant interprets a request
$ derisk agent [--socket PATH | --socket-activated] [--execute]
```

Without `--execute` the agent only simulates: launched apps appear as
windows in the headless shell. With `--execute`, effects run through systemd
(launches, session operations, unit restarts) and are logged to the journal.

## Status

- **Shell** (library `derisk`): window management policy, decorations, the
  top bar, overview, assistant, agent protocol, keyboard shortcuts and
  systemd integration.
- **Compositor host** (`derisk session`, feature `host`): derisk's desktop
  implemented on mcsapi's `mcsapi-compositor` (Smithay, winit backend, GLES
  renderer). The host manages xdg-shell toplevels and popups with server-side
  decorations, input and outputs, and runs the `derisk-apps` core apps in
  process next to Wayland clients; derisk supplies window placement, keys,
  title bars, chrome and the agent socket on the live desktop.

Still to do: a DRM/KMS + libinput backend to run on a bare TTY (today the
session runs nested), layer-shell, XWayland, popup grabs, and bridging D-Bus
menus (`com.canonical.dbusmenu`) and StatusNotifierItem to the global menu
and tray. Apps that insist on client-side decorations (GTK, weston clients)
draw their own title bar inside derisk's.

## Showcase video

`scripts/showcase.py` records the desktop in a virtual X server: it starts
Xvfb, runs `derisk session`, drives it with xdotool and the agent socket, and
encodes with ffmpeg. It shows Wayland clients and the core apps side by
side. It needs Xvfb, xdotool, ffmpeg, foot, neofetch and htop.

```console
$ cargo build --release --features host
$ PATH=$PWD/target/release:$PATH scripts/showcase.py derisk-showcase.mp4
```

## Core apps

The repository is a Cargo workspace with every crate under `crates/` and
nothing but the workspace at the root:

| Crate | What it holds |
| --- | --- |
| `derisk` | The shell (`shell`, `ui`, `palette`, `ipc`, ...) and the `derisk` binary with all its subcommands. |
| `derisk-geom` | Geometry, snapping and server-side decorations. |
| `derisk-desktop` | `.desktop` entries and app names and icons by app ID. |
| `derisk-install` | The installer's backend protocol, locales and time zones, and the network. |
| `derisk-login` | The greetd login protocol, the lock screen's state and the polkit dialog's. |
| `derisk-portal` | `xdg-desktop-portal-derisk`. |
| `derisk-machines` | The libvirt machines apps run in (labels, states, starting and stopping them, running programs in them) and the Machines app. |
| `derisk-apps`, `derisk-gpui`, `derisk-icons` and one crate per app | The core apps, below. |

Each crate keeps everything it needs in its own directory (its README,
LICENSE and any `data/`), and reaches its siblings only through the
workspace's `[workspace.dependencies]`, so any crate can move to a
repository of its own.

The four shell crates have nothing of the shell's above them, so they build
and test without egui or the compositor; `derisk` re-exports each of their
modules at the path it had before (`derisk::geom`, `derisk::install`, ...).
The rest of the shell stays one crate because its modules use one another
in a cycle (the shell's model, its egui drawing and the phone UI). A bare
`cargo build` or `cargo test` at the root covers `derisk` and those four
crates; `--workspace` adds the apps, the portal and GPUI.

The core apps live under `crates/` too. Each app is an `mcsapi_ui::App` with its logic
in a UI-free model and its own tests, and has an ID under `org.derisk.*`.

| App | Crate | What it does |
| --- | --- | --- |
| Files | `derisk-files` | Places, back/forward/up, sort, filter, hidden files, new folder/file, rename (never overwrites), copy/cut/paste with `name (copy).ext` on clashes, the freedesktop.org trash, and `xdg-open`. |
| Settings | `derisk-settings` | Dark/light, accent, text size, reduced motion, window corners and shadows, wallpaper, the top bar's position, auto-hide and contents, layout, gaps, workspaces, adaptive profile, input, shortcuts, notifications, and power, saved to `$XDG_CONFIG_HOME/derisk/settings.conf`. `Settings::theme()` gives the shell its colors. |
| Text Editor | `derisk-editor` | UTF-8 files up to 8 MiB, atomic saves that keep permissions, find and replace, line/column, unsaved-change prompts. |
| System Monitor | `derisk-monitor` | CPU graph, memory, swap, load, uptime, and a sortable, filterable process table with End process, from `/proc`. |
| Calculator | `derisk-calculator` | Expressions with precedence, `^`, `%`, functions, `pi`, `e`, `ans`, and history. |
| Machines | `derisk-machines` | The libvirt machines apps run in, each in its label's colour: make one from a template the OS ships (through `pm machine create`), start, shut down, force off and remove them, and set pm's libvirt plugin up with your own key. |

`derisk-apps` lists them, registers them with `mcsapi-runtime`, and keeps the
running app objects next to their instances (`derisk_apps::Session`), so the
host only gives each instance a surface and calls `mcsapi_ui::run_frame`.
To try them in one window without the compositor:

```console
$ cargo run -p derisk-apps --features preview --bin derisk-preview -- org.derisk.files
```

### GPUI

derisk is moving from egui to [GPUI](https://www.gpui.rs). `derisk-gpui`
draws the ported apps (so far the Calculator) as their own Wayland clients,
in the desktop theme, and follows theme changes while running. When the
`derisk-gpui` binary sits next to `derisk`, the session opens ported apps
with it; otherwise they run in-process with egui as before.

```console
$ cargo build -p derisk-gpui --features gpui
$ cargo run -p derisk-gpui --features gpui -- org.derisk.calculator
```

GPUI cannot draw inside the compositor, so the shell's chrome is still egui.
GPUI panels and overlays run as the compositor's runtime clients: it starts
them on a private connection and places their windows by the role given on
the command line, which a client cannot choose for itself. Panels keep
windows out of their strip; an overlay covers the screen and takes every key.
Both come back if they exit.

```console
$ derisk session --runtime panel:bottom:48 "my-gpui-dock"
$ derisk session --runtime overlay "my-gpui-launcher"
```

## Building

Requires Rust 1.96 (wasmtime's minimum, for the palette plugins) and the system libraries mcsapi links against, for
example on Debian/Ubuntu:

```console
$ sudo apt install libxkbcommon-dev libwayland-dev libegl-dev libgles-dev libinput-dev libudev-dev libgbm-dev libseat-dev libdrm-dev libpam0g-dev
$ cargo build
$ cargo test --workspace
$ cargo clippy --workspace --all-targets -- -D warnings
$ cargo clippy --workspace --all-targets --features derisk/host -- -D warnings
```

### Nix

`flake.nix` has a dev shell with the Rust toolchain and the Wayland, libinput,
GPU and windowing libraries. With [nix-direnv](https://github.com/nix-community/nix-direnv),
`direnv allow` enters it on `cd` (see `.envrc`).

```console
$ nix develop                # the dev shell
$ nix build                  # derisk with the host feature and its portal, plus their units and portal files under share/
$ nix build .#derisk-preview # the apps' preview window
$ nix run . -- session --launch foot
$ nix flake check            # rustfmt, clippy and tests (default and host + preview)
$ nix fmt                    # nixfmt and rustfmt
```

mcsapi comes from Cargo.lock, not a flake input: `cargo update -p mcsapi`
moves it for Cargo and Nix alike.

## License

GPL-3.0-only
