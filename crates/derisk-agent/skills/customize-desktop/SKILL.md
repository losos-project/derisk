---
name: customize-desktop
description: Changes how the derisk desktop (LosOS) looks and behaves from a plain-language request, such as "make it dark with a blue accent and a bigger font" or "put the bar at the bottom and use my beach photo as wallpaper". Shows the exact settings it will change, applies them only after the user agrees, and says how to undo. Use when the user wants to restyle, theme, recolor or rearrange their desktop, change the wallpaper, fonts, icons, top bar, window corners or keyboard shortcuts.
---

# Customizing the derisk desktop

derisk is the LosOS desktop. Everything it lets a user restyle lives in two
kinds of plain-text file, and the running session re-reads them by itself, so
this skill only ever edits those files. It never patches, rebuilds or
restarts derisk, and never touches files outside the ones named here.

| File | What it holds | Who reads it |
|------|---------------|--------------|
| `~/.config/derisk/settings.conf` (`$XDG_CONFIG_HOME/derisk/settings.conf`) | `key = value` lines: theme choice, accent, blur, panel opacity, top bar, window corners, wallpaper, layout, shortcuts | The session, the Settings app, GPUI apps |
| `~/.local/share/derisk/themes/<id>.theme` | A theme file: colors, fonts, icon and cursor theme, control radius | Loaded when `appearance.theme = <id>` |

The session checks `settings.conf`'s modification time about once a second
and, when it changed, re-applies everything from it, including loading the
named theme file again. It then republishes the theme to
`$XDG_RUNTIME_DIR/derisk/theme.json` and GTK's `settings.ini`, and writes the
icon theme to GSettings, so GTK, Qt, Flatpak and Android apps follow.
Editing only a theme file does nothing until `settings.conf` is written
again; the helper below always rewrites it.

## Rules that hold throughout

- **Show before you change.** Before anything is written, show the user a
  short table of every key you will set, its current value and its new
  value, plus the full theme file if you are writing one. Then end your
  reply with the question "Apply these changes?" and the options "Apply",
  "Change something" and "Cancel"; the user's next message is the answer.
  Write nothing unless the answer is "Apply". A yes covers only the
  table you showed; if you change the plan afterwards, show it and ask again.
- **Only through the helper.** Every write goes through `desk.py apply`,
  which checks each value against what derisk accepts, backs up both files
  first and writes atomically. Do not edit `settings.conf` with `sed`, an
  editor tool or `echo >>`. A value derisk rejects is silently replaced by
  its default, so an unchecked edit can look applied and not be.
- **Always say how to undo**, with the exact command `desk.py apply` printed.
- **Only installed things.** Use only fonts `fc-list` reports, icon and
  cursor themes `desk.py show` lists, and wallpaper files that exist. Do not
  download or install fonts, icon themes or pictures unless the user asks
  for that separately; if they do, put them under `~/.local/share/fonts`,
  `~/.local/share/icons` or `~/Pictures` and say where.
- **Look and feel only.** This skill changes the keys in the table below and
  nothing else. Privacy, default apps and search engine, power and sleep,
  keyboard and touchpad, and notifications belong to the Settings app: say
  so, and that it opens from the command palette (Super+Space, "Settings").
- **Be honest about what shows.** Some keys are saved but not yet drawn by
  every part of the desktop (see "What takes effect where"). Say so in the
  table rather than promising a change the user will not see.
- **The Settings app must be closed.** It reads `settings.conf` once when it
  opens and writes every key back when the user presses Save, which would
  undo your change. It runs inside the session, not as its own process, so
  ask the user to close its window first (or, with derisk's desktop tools,
  look for it with `find_elements`).

## Step 1: Read the current state

Write the helper at the end of this skill to
`~/.local/share/derisk-agent/desktop/desk.py`, exactly as given, then run:

```sh
cd ~/.local/share/derisk-agent/desktop
python3 -I desk.py show
fc-list : family | sort -u
```

`show` prints the settings file's path, every key this skill may change with
its current value (`(set)` marks those in the file; the rest are derisk's
defaults), the theme files present, the installed icon themes, and the theme
the session last published. If `python3` is missing, say so and stop.

## Step 2: Turn the request into settings

Map each part of the request onto the keys and theme fields below. When a
word could mean several things ("cleaner", "more modern"), pick the most
likely reading, say which, and let the confirmation in step 3 catch it. Ask
only when no reasonable reading exists, such as "use my photo" with several
photos in `~/Pictures`.

### settings.conf keys

| Key | Values (derisk default) | Live? |
|-----|------------------------|-------|
| `appearance.theme` | `auto`, or a theme ID: a built-in (`derisk-dark`, `derisk-light`, `derisk-high-contrast`) or a theme file's name without `.theme` (`auto`) | yes |
| `appearance.scheme` | `dark`, `light` (`dark`). Used only while `appearance.theme = auto` | yes |
| `appearance.accent` | `lime`, `sky`, `violet`, `rose`, `amber` (`lime`). Used only while `appearance.theme = auto`; derisk adjusts it to stay legible | yes |
| `appearance.text_scale` | 0.75 to 2.0 (`1`) | saved only |
| `appearance.reduce_motion` | `true`, `false` (`false`) | yes |
| `appearance.blur` | 0 (off) to 10 (`6`) | yes |
| `appearance.top_bar_opacity`, `appearance.overview_opacity`, `appearance.snap_assist_opacity` | 20 to 100 percent (`70`, `80`, `85`) | yes |
| `top_bar.position` | `top`, `bottom` (`top`) | yes |
| `top_bar.autohide` | `true`, `false` (`false`) | yes |
| `top_bar.search`, `top_bar.app_name`, `top_bar.date`, `top_bar.battery` | show each item: `true`, `false` (all `true`) | yes |
| `top_bar.clock_24h` | `true`, `false` (`true`) | yes |
| `windows.corner_radius` | 0 to 20 pixels (`10`) | yes |
| `windows.shadows` | `true`, `false` (`true`) | yes |
| `wallpaper.kind` | `default` (derisk's gradient, tinted by the accent), `color`, `gradient`, `image`, `slideshow`, `video` (`default`) | yes |
| `wallpaper.path` | absolute path: a PNG, JPEG or WebP for `image`, a folder for `slideshow`, a video for `video` | yes |
| `wallpaper.fit` | `fill`, `fit`, `stretch`, `center`, `tile` (`fill`) | yes |
| `wallpaper.color`, `wallpaper.color2` | `#rrggbb`: the color, or the gradient's top and bottom (`#111827`, `#1e1b4b`) | yes |
| `wallpaper.interval_min` | slideshow minutes, 1 to 1440 (`30`) | yes |
| `wallpaper.shuffle` | `true`, `false` (`false`) | yes |
| `wallpaper.pause_when_covered`, `wallpaper.pause_in_low_power` | `true`, `false` (both `true`) | yes |
| `desktop.layout` | `tall`, `monocle` (`tall`) | saved only |
| `desktop.gaps` | 0 to 64 pixels (`8`) | saved only |
| `desktop.workspaces` | 1 to 9 (`9`) | saved only |
| `desktop.profile` | `automatic`, `phone`, `tablet`, `desktop` (`automatic`) | saved only |
| `shortcut.<id>` | a chord such as `Super+Shift+M`, or `none` to turn it off | yes |

Shortcut IDs and their defaults: `palette` (Super+Space), `overview`
(Super+A), `focus_next` (Super+J), `focus_previous` (Super+K), `promote`
(Super+Enter), `close` (Super+Q), `float` (Super+F), `tile` (Super+T),
`monocle` (Super+M), `tall` (Super+Shift+M). A chord is modifiers (`Super`,
`Ctrl`, `Alt`, `Shift`; at least one of the first three) and one key: a
letter, a digit, `Enter`, `Tab`, `Escape`, `Space` or an arrow. Two
shortcuts on one chord means only the first in that list fires: check for a
clash with `desk.py show` and tell the user.

### What takes effect where

- **Live** keys change the screen within about a second.
- **Saved only** keys are kept, and the Settings app shows them, but the
  derisk session in this release does not act on them yet: `text_scale` is
  applied only by the `derisk-preview` developer window, and the tiling layout, gaps, workspace
  count and profile come from the screen size. Set them if the user wants,
  and say they will not see a difference yet.
- **Fonts** (theme file `[fonts]`) reach GTK apps opened afterwards, GPUI
  apps such as the Calculator, and everything that reads `theme.json`.
  derisk's own top bar, overview and built-in egui apps draw with their
  embedded font and keep its size, so "a bigger font" does not enlarge the
  shell itself in this release. Say so.
- **Icon and cursor themes** apply to the shell and its apps at once, and to
  GTK, Qt and Flatpak apps when they next start. A new cursor theme shows in
  apps started afterwards.
- **A video wallpaper** needs `ffmpeg`; check `command -v ffmpeg` (or
  `$DERISK_FFMPEG`) first and offer a picture instead if it is missing.

### When to write a theme file

The settings keys only pick a built-in light or dark look with one of five
accents. Write a theme file when the request needs anything else:

- an accent that is not one of the five (any `#rrggbb`),
- other background, surface, text or border colors,
- a font family, weight or size,
- an icon or cursor theme other than Papirus-Dark (dark) or Papirus (light),
  or another cursor size,
- a different control radius.

A named theme brings its own scheme and accent, so `appearance.scheme` and
`appearance.accent` stop mattering while it is selected: put those into the
theme file too. To go back to the plain built-in look later, set
`appearance.theme = auto`.

Use the theme ID `my-theme` and the name `My Theme` unless the user
names it. If `~/.local/share/derisk/themes/my-theme.theme` already exists,
start from it: read it, change what the request asks, and keep the rest.
Otherwise start from the built-in closest to the request (`derisk-dark`,
`derisk-light` or `derisk-high-contrast`) and list only what changes.

The format is a small subset of TOML: `[section]` headers, `key = value`,
`#` comments; strings in double quotes, numbers bare. Every key is optional.
A wrong type or an out-of-range number makes the whole file fail, and the
session then falls back to the automatic theme.

```toml
name = "My Theme"
inherits = "derisk-dark"     # the theme it starts from
scheme = "dark"              # "dark" or "light"; GTK, Qt and the portal follow it

[colors]                     # "#rrggbb"
background = "#0f172a"
surface = "#1e293b"          # raised controls, tracks
foreground = "#f8fafc"       # text
border = "#64748b"
accent = "#2563eb"           # or a built-in accent name such as "sky"
destructive = "#dc2626"

[fonts]                      # families must be installed (fc-list)
sans = "Ubuntu"
sans_weight = 300            # 1 to 1000; 400 regular, 700 bold
monospace = "Hack"
size = 14                    # logical pixels, 6 to 72
small_size = 12
monospace_size = 12

[icons]                      # directory names from `desk.py show`
theme = "Papirus-Dark"
cursor = "Adwaita"
cursor_size = 24             # 8 to 256

[shape]
radius = 6                   # control corners, 0 to 32; cards use twice this
```

Defaults: dark is background `#0f172a`, surface `#1e293b`, foreground
`#f8fafc`, border `#64748b`; light is `#f8fafc`, `#e2e8f0`, `#0f172a`,
`#94a3b8`. Both use Ubuntu 300 at 14 px, Hack at 12 px and a radius of 6;
dark uses Papirus-Dark and light uses Papirus, with the Adwaita cursor at 24.
High contrast is black and white with a `#ffd600` accent.

Guidance for common requests:

- **Accent color.** An accent written in a theme file is used exactly as
  given. Check it with `desk.py contrast <accent> <background>`: below 3:1
  it is hard to see, so darken it on light backgrounds and lighten it on
  dark ones until it passes, and say you did. The five named accents (via
  `appearance.accent` or `accent = "sky"`) are adjusted by derisk itself.
- **Text and background.** Keep `foreground` at 4.5:1 or more against both
  `background` and `surface` (`desk.py contrast`).
- **Bigger or smaller text.** Scale `size`, `small_size` and
  `monospace_size` together (for "bigger", about 1.2 times: 17, 14, 14), and
  also set `appearance.text_scale` to the same factor so the Settings app
  shows the choice.
- **Light/dark icons.** When the scheme changes, switch the icon theme to the
  matching variant if one is installed (Papirus for light, Papirus-Dark for
  dark), or icons will be drawn for the wrong background.
- **Wallpaper colors to match.** For a request like "everything blue", a
  `gradient` wallpaper between a dark and a mid tone of the accent ties the
  look together; propose it in the table, do not slip it in.

## Step 3: Confirm

Show the plan as a table: key or theme field, current value, new value, and
"live" or "saved only". If you write a theme file, show it in full. Mention
anything you could not do and why (a font that is not installed, a key that
is not drawn yet). Then ask as the rules say.

On "Change something", adjust and show the table again. On "Cancel", stop
and write nothing.

## Step 4: Apply

On "Apply", write the theme file (if any) to a scratch path first, for
example `~/.local/share/derisk-agent/desktop/next.theme`, then run one command:

```sh
cd ~/.local/share/derisk-agent/desktop
python3 -I desk.py apply [--theme my-theme next.theme] key=value ...
```

`--theme ID FILE` installs the file as `~/.local/share/derisk/themes/ID.theme`
and sets `appearance.theme = ID`. Quote each `key=value` for the shell when
it holds spaces or `#` (`'wallpaper.color=#0b1220'`). The helper refuses the
whole command, writing nothing, if any key is not one of the table's or any
value is one derisk would ignore; fix the value and run it again.

It backs up `settings.conf` and the theme file it replaces to
`~/.local/share/derisk-agent/desktop/backups/<time>/` before writing, keeps every
other line and comment in `settings.conf`, and prints the undo command.

## Step 5: Check it landed

When a theme file was written, run `python3 -I desk.py check "My Theme"`
(the theme's `name`). It waits up to five seconds for the session to publish
that theme to `theme.json` and prints its colors, fonts and icons. If it
reports a fall back, the theme file has an error: run
`journalctl --user -b -g theme` for the line and reason, then either fix the
file and apply again (with the user's yes for the fix) or undo.

When derisk's desktop tools are available (`screenshot`), take one and look
at the result yourself: is the accent visible, is the text readable, did the
wallpaper change. If something looks wrong, say what and offer a fix or the
undo. Without them, ask the user to look.

Outside a derisk session (no `XDG_RUNTIME_DIR`, or `show` reports no
published theme), the files are still written and take effect at the next
login. Say so instead of running `check`.

## Step 6: Tell the user

In a few lines: what changed, what they will see now and what only later,
and how to undo:

```sh
python3 -I ~/.local/share/derisk-agent/desktop/desk.py undo <time>
```

`undo` without a time reverts the latest change. Undo several changes newest
first: each one restores the files as they were just before that change.
After an undo, the session picks the old settings up within a second.

## desk.py

```python
import json, os, re, shutil, sys, time

home = os.path.expanduser("~")

def xdg(variable, fallback):
    value = os.environ.get(variable, "")
    return value if os.path.isabs(value) else os.path.join(home, fallback)

settings_path = os.path.join(xdg("XDG_CONFIG_HOME", ".config"), "derisk", "settings.conf")
themes_dir = os.path.join(xdg("XDG_DATA_HOME", ".local/share"), "derisk", "themes")
backups_dir = os.path.join(xdg("XDG_DATA_HOME", ".local/share"), "derisk-agent", "desktop", "backups")
runtime = os.environ.get("XDG_RUNTIME_DIR", "")
theme_json = os.path.join(runtime, "derisk", "theme.json") if os.path.isabs(runtime) else None
data_dirs = [xdg("XDG_DATA_HOME", ".local/share"), os.path.join(home, ".icons")] + [
    d for d in (os.environ.get("XDG_DATA_DIRS") or "/usr/local/share:/usr/share").split(":") if os.path.isabs(d)]

# The keys this skill may change, with derisk-settings' defaults and the
# values its parser (crates/derisk-settings/src/model.rs, Settings::set)
# accepts. A value derisk rejects is skipped with a warning and the default
# stays, so checking here is what makes a change actually land.
def enum(*names):
    return lambda v: v in names
def number(low, high, integer=True):
    pattern = r"\d+" if integer else r"\d+(\.\d+)?"
    return lambda v: re.fullmatch(pattern, v) is not None and low <= float(v) <= high
def boolean(v):
    return v in ("true", "false")
def rgb(v):
    return re.fullmatch(r"#[0-9a-fA-F]{6}", v) is not None
def theme_id(v):
    return v == "auto" or (re.fullmatch(r"[A-Za-z0-9_.-]{1,48}", v) is not None and not v.startswith("."))
def absolute_or_empty(v):
    return v == "" or os.path.isabs(v)
def chord(v):
    if v.lower() == "none":
        return True
    *modifiers, key = [part.strip().lower() for part in v.split("+")]
    names = {"super": "logo", "logo": "logo", "meta": "logo", "win": "logo",
             "shift": "shift", "ctrl": "ctrl", "control": "ctrl", "alt": "alt"}
    flags = [names.get(m) for m in modifiers]
    if None in flags or len(set(flags)) != len(flags) or not set(flags) & {"logo", "ctrl", "alt"}:
        return False
    return key in ("enter", "return", "tab", "escape", "esc", "space", "left", "right", "up", "down") \
        or re.fullmatch(r"[a-z0-9]", key) is not None

keys = {
    "appearance.theme": ("auto", theme_id),
    "appearance.scheme": ("dark", enum("dark", "light")),
    "appearance.accent": ("lime", enum("lime", "sky", "violet", "rose", "amber")),
    "appearance.text_scale": ("1", number(0.75, 2.0, integer=False)),
    "appearance.reduce_motion": ("false", boolean),
    "appearance.blur": ("6", number(0, 10)),
    "appearance.top_bar_opacity": ("70", number(20, 100)),
    "appearance.overview_opacity": ("80", number(20, 100)),
    "appearance.snap_assist_opacity": ("85", number(20, 100)),
    "desktop.layout": ("tall", enum("tall", "monocle")),
    "desktop.gaps": ("8", number(0, 64)),
    "desktop.workspaces": ("9", number(1, 9)),
    "desktop.profile": ("automatic", enum("automatic", "phone", "tablet", "desktop")),
    "top_bar.position": ("top", enum("top", "bottom")),
    "top_bar.autohide": ("false", boolean),
    "top_bar.search": ("true", boolean),
    "top_bar.app_name": ("true", boolean),
    "top_bar.date": ("true", boolean),
    "top_bar.battery": ("true", boolean),
    "top_bar.clock_24h": ("true", boolean),
    "windows.corner_radius": ("10", number(0, 20)),
    "windows.shadows": ("true", boolean),
    "wallpaper.kind": ("default", enum("default", "color", "gradient", "image", "slideshow", "video")),
    "wallpaper.path": ("", absolute_or_empty),
    "wallpaper.fit": ("fill", enum("fill", "fit", "stretch", "center", "tile")),
    "wallpaper.color": ("#111827", rgb),
    "wallpaper.color2": ("#1e1b4b", rgb),
    "wallpaper.interval_min": ("30", number(1, 1440)),
    "wallpaper.shuffle": ("false", boolean),
    "wallpaper.pause_when_covered": ("true", boolean),
    "wallpaper.pause_in_low_power": ("true", boolean),
}
shortcut_defaults = {
    "palette": "Super+Space", "overview": "Super+A", "focus_next": "Super+J",
    "focus_previous": "Super+K", "promote": "Super+Enter", "close": "Super+Q",
    "float": "Super+F", "tile": "Super+T", "monocle": "Super+M", "tall": "Super+Shift+M",
}
for shortcut, default in shortcut_defaults.items():
    keys[f"shortcut.{shortcut}"] = (default, chord)

def strip_comment(line):
    # derisk's rule: `#` starts a comment at the start of a line or as a word
    # of its own, so `#rrggbb` colors survive.
    if line.lstrip().startswith("#"):
        return ""
    match = re.search(r"\s#(\s|$)", line)
    return line[:match.start()] if match else line

def read_settings():
    try:
        with open(settings_path) as file:
            lines = file.read().splitlines()
    except FileNotFoundError:
        lines = []
    values = {}
    for line in lines:
        key, sep, value = strip_comment(line).partition("=")
        if sep:
            values[key.strip()] = value.strip()
    return lines, values

def icon_themes():
    found = set()
    for data in data_dirs:
        base = data if data.endswith(".icons") else os.path.join(data, "icons")
        for name in os.listdir(base) if os.path.isdir(base) else []:
            if os.path.exists(os.path.join(base, name, "index.theme")):
                found.add(name)
    return sorted(found)

def theme_files():
    dirs = [themes_dir] + [os.path.join(d, "derisk", "themes") for d in data_dirs[2:]]
    found = {}
    for directory in dirs:
        for name in sorted(os.listdir(directory)) if os.path.isdir(directory) else []:
            if name.endswith(".theme"):
                found.setdefault(name[:-len(".theme")], os.path.join(directory, name))
    return found

def show():
    lines, values = read_settings()
    print(f"settings file: {settings_path} ({'present' if lines else 'absent, all defaults'})")
    for key, (default, _) in keys.items():
        value = values.get(key)
        mark = "" if value is None else "   (set)"
        print(f"  {key} = {default if value is None else value}{mark}")
    print("theme files:", json.dumps(theme_files()))
    print("built-in themes: derisk-dark derisk-light derisk-high-contrast")
    print("icon themes:", " ".join(icon_themes()))
    if theme_json and os.path.exists(theme_json):
        with open(theme_json) as file:
            print("published theme.json:", file.read().strip())
    else:
        print("published theme.json: none (no derisk session in this environment)")

def write_atomically(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    temporary = path + ".tmp"
    with open(temporary, "w") as file:
        file.write(text)
    os.replace(temporary, path)

def apply(arguments):
    theme = None
    if arguments[:1] == ["--theme"]:
        if len(arguments) < 3:
            sys.exit("--theme needs an ID and a file")
        theme, theme_source, arguments = arguments[1], arguments[2], arguments[3:]
        if not theme_id(theme) or theme == "auto":
            sys.exit(f"bad theme ID {theme!r}: letters, digits, '-', '_' and '.' only")
    changes = {}
    for argument in arguments:
        key, sep, value = argument.partition("=")
        key, value = key.strip(), value.strip()
        if not sep or key not in keys:
            sys.exit(f"refused {argument!r}: not a key this skill changes")
        if not keys[key][1](value):
            sys.exit(f"refused {key} = {value!r}: derisk would ignore that value")
        if "\n" in value or re.search(r"\s#(\s|$)", " " + value):
            sys.exit(f"refused {key}: a ' # ' in the value would be read as a comment")
        changes[key] = value
    if theme is None and not changes:
        sys.exit("nothing to change")

    stamp = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime())
    while os.path.exists(os.path.join(backups_dir, stamp)):
        stamp += "+"
    backup = os.path.join(backups_dir, stamp)
    os.makedirs(backup)
    manifest = {"settings": settings_path, "settings_existed": os.path.exists(settings_path),
                "theme": None, "theme_existed": False}
    if manifest["settings_existed"]:
        shutil.copy2(settings_path, os.path.join(backup, "settings.conf"))
    if theme is not None:
        theme_path = os.path.join(themes_dir, theme + ".theme")
        manifest["theme"] = theme_path
        manifest["theme_existed"] = os.path.exists(theme_path)
        if manifest["theme_existed"]:
            shutil.copy2(theme_path, os.path.join(backup, "theme"))
        with open(theme_source) as file:
            write_atomically(theme_path, file.read())
        changes.setdefault("appearance.theme", theme)
    with open(os.path.join(backup, "manifest.json"), "w") as file:
        json.dump(manifest, file, indent=1)

    # Edit in place: keep the user's other lines and comments, replace the
    # first line of each changed key, drop later duplicates (the last one
    # would win otherwise), and append keys the file did not have.
    lines, _ = read_settings()
    out, done = [], set()
    for line in lines:
        key = strip_comment(line).partition("=")[0].strip()
        if key in changes:
            if key not in done:
                out.append(f"{key} = {changes[key]}")
                done.add(key)
            continue
        out.append(line)
    out += [f"{key} = {value}" for key, value in changes.items() if key not in done]
    # A rename always moves the modification time, which is what derisk's
    # SettingsWatch polls, so the session re-reads settings and theme within
    # a second even when only the theme file changed.
    write_atomically(settings_path, "\n".join(out) + "\n")
    print(f"applied; undo with: python3 -I desk.py undo {stamp}")

def undo(arguments):
    stamps = sorted(os.listdir(backups_dir)) if os.path.isdir(backups_dir) else []
    stamp = arguments[0] if arguments else (stamps[-1] if stamps else None)
    if stamp is None or stamp not in stamps:
        sys.exit(f"no backup {stamp!r}; have: {' '.join(stamps) or 'none'}")
    backup = os.path.join(backups_dir, stamp)
    with open(os.path.join(backup, "manifest.json")) as file:
        manifest = json.load(file)
    if manifest["theme"]:
        if manifest["theme_existed"]:
            shutil.copy2(os.path.join(backup, "theme"), manifest["theme"])
        elif os.path.exists(manifest["theme"]):
            os.remove(manifest["theme"])
    if manifest["settings_existed"]:
        with open(os.path.join(backup, "settings.conf")) as file:
            write_atomically(manifest["settings"], file.read())
    elif os.path.exists(manifest["settings"]):
        os.remove(manifest["settings"])
    undone = os.path.join(os.path.dirname(backups_dir), "undone")
    os.makedirs(undone, exist_ok=True)
    os.replace(backup, os.path.join(undone, stamp))
    print(f"restored the desktop as it was before {stamp}")

def check(arguments):
    # The session polls once a second; give it a few.
    expected = arguments[0] if arguments else None
    if not theme_json:
        sys.exit("XDG_RUNTIME_DIR is not set: not inside a derisk session")
    for _ in range(10):
        time.sleep(0.5)
        try:
            with open(theme_json) as file:
                published = json.load(file)
        except (FileNotFoundError, json.JSONDecodeError):
            continue
        if expected is None or published.get("name") == expected:
            break
    else:
        sys.exit(f"the session did not publish {expected!r}; it fell back to the automatic theme, "
                 "so the theme file has an error (see `journalctl --user -b -g theme`)")
    print(json.dumps({k: published.get(k) for k in ("id", "name", "scheme", "palette", "fonts", "icons")}))

def contrast(arguments):
    # WCAG 2 contrast ratio. derisk keeps a settings accent at 3:1 against the
    # background itself, but an accent written in a theme file is used as is.
    def luminance(color):
        channels = [int(color.lstrip("#")[i:i + 2], 16) / 255 for i in (0, 2, 4)]
        r, g, b = [c / 12.92 if c <= 0.04045 else ((c + 0.055) / 1.055) ** 2.4 for c in channels]
        return 0.2126 * r + 0.7152 * g + 0.0722 * b
    if len(arguments) != 2 or not all(rgb(a) for a in arguments):
        sys.exit("usage: desk.py contrast #rrggbb #rrggbb")
    lighter, darker = sorted(map(luminance, arguments), reverse=True)
    print(f"{(lighter + 0.05) / (darker + 0.05):.2f}:1")

commands = {"show": lambda _: show(), "apply": apply, "undo": undo, "check": check, "contrast": contrast}
if len(sys.argv) < 2 or sys.argv[1] not in commands:
    sys.exit("usage: desk.py show | apply [--theme ID FILE] KEY=VALUE... | undo [STAMP] | check [THEME NAME] | contrast FG BG")
commands[sys.argv[1]](sys.argv[2:])
```
