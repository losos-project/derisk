use std::fs;

use derisk::{apps::Apps, desktop::DesktopEntry, icons};

fn entry(id: &str, body: &str) -> DesktopEntry {
    let text = format!("[Desktop Entry]\nType=Application\n{body}");
    DesktopEntry::parse(id, &text).unwrap()
}

#[test]
fn names_and_icons_come_from_the_matching_desktop_file() {
    let firefox = entry(
        "org.mozilla.firefox.desktop",
        "Name=Firefox\nIcon=firefox\nExec=firefox %u\n",
    );
    let code = entry(
        "code.desktop",
        "Name=Visual Studio Code\nIcon=vscode\nStartupWMClass=Code\nExec=/usr/bin/code %F\n",
    );
    let files = entry(
        "org.derisk.files.desktop",
        "Name=Files\nIcon=system-file-manager\nExec=derisk launch org.derisk.files\n",
    );
    let editor = entry(
        "org.derisk.editor.desktop",
        "Name=Text Editor\nExec=derisk launch org.derisk.editor\n",
    );
    let apps = Apps::new([(&files, "📁"), (&editor, "📝"), (&firefox, ""), (&code, "")]);

    // The desktop file ID, case-insensitively.
    let look = apps.look("org.mozilla.Firefox");
    assert_eq!((look.name.as_ref(), look.icon), ("Firefox", "firefox"));
    // StartupWMClass.
    assert_eq!(apps.look("Code").name, "Visual Studio Code");
    // The last part of a reverse-DNS ID, either way round.
    assert_eq!(apps.look("firefox").name, "Firefox");
    // The program, when it is the app's own.
    assert_eq!(apps.look("code").name, "Visual Studio Code");
    // Core apps keep their glyph; without an Icon key the app ID is tried.
    let look = apps.look("org.derisk.editor");
    assert_eq!(
        (look.name.as_ref(), look.icon, look.glyph),
        ("Text Editor", "org.derisk.editor", "📝")
    );
    // The palette's desktop file IDs, with their `.desktop`.
    let look = apps.look("org.derisk.files.desktop");
    assert_eq!(
        (look.name.as_ref(), look.icon),
        ("Files", "system-file-manager")
    );
    assert_eq!(
        apps.look("org.derisk.editor.desktop").icon,
        "org.derisk.editor"
    );
    // `derisk` starts every core app, so it names none of them.
    assert_eq!(apps.look("derisk").name, "Derisk");
}

#[test]
fn unknown_app_ids_still_read_as_names() {
    let apps = Apps::default();
    let look = apps.look("org.example.notes");
    assert_eq!(
        (look.name.as_ref(), look.icon, look.glyph),
        ("Notes", "org.example.notes", "")
    );
    assert_eq!(apps.look("foot").name, "Foot");
    assert_eq!(apps.look("").name, "");
}

#[test]
fn icons_come_from_the_set_theme_then_hicolor_then_other_themes_then_pixmaps() {
    let root = std::env::temp_dir().join(format!("derisk-icons-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let icons_dir = root.join("icons");
    let pixmaps = root.join("pixmaps");
    let png = |path: std::path::PathBuf, size: u32| {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        image::RgbaImage::from_pixel(size, size, image::Rgba([255, 0, 0, 255]))
            .save(&path)
            .unwrap();
        path
    };
    let small = png(icons_dir.join("hicolor/32x32/apps/foot.png"), 32);
    let big = png(icons_dir.join("hicolor/128x128/apps/foot.png"), 128);
    let svg = icons_dir.join("hicolor/scalable/apps/foot.svg");
    fs::create_dir_all(svg.parent().unwrap()).unwrap();
    fs::write(
        &svg,
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16" fill="#00f"/></svg>"##,
    )
    .unwrap();
    let generic = png(
        icons_dir.join("Adwaita/48x48/places/system-file-manager.png"),
        48,
    );
    let pixmap = png(pixmaps.join("legacy.png"), 48);
    let themed = icons_dir.join("Themed/scalable/apps/foot.svg");
    fs::create_dir_all(themed.parent().unwrap()).unwrap();
    fs::copy(&svg, &themed).unwrap();
    let roots = (vec![icons_dir.clone()], vec![pixmaps.clone()]);
    let unset = icons::theme_chain("", &roots.0);
    let find = |name, want| icons::find_in(name, want, &roots.0, &roots.1, &unset);

    // The smallest PNG at least as big, else the SVG, else a smaller PNG.
    assert_eq!(find("foot", 64), Some(big.clone()));
    assert_eq!(find("foot", 32), Some(small));
    assert_eq!(find("foot", 256), Some(svg.clone()));
    assert_eq!(find("system-file-manager", 64), Some(generic));
    assert_eq!(find("legacy", 64), Some(pixmap));
    assert_eq!(find(big.to_str().unwrap(), 64), Some(big.clone()));
    assert_eq!(find("../foot", 64), None);
    assert_eq!(find("missing", 64), None);
    // A set theme comes before hicolor, as the spec has it.
    let set = icons::theme_chain("Themed", &roots.0);
    assert_eq!(
        icons::find_in("foot", 64, &roots.0, &roots.1, &set),
        Some(themed)
    );

    // Decoded no bigger than asked, in both formats.
    assert_eq!(icons::load(&big, 64).unwrap().size, [64, 64]);
    let blue = icons::load(&svg, 64).unwrap();
    assert_eq!(blue.size, [64, 64]);
    assert_eq!(
        blue.pixels[0],
        mcsapi::toolkit::egui::Color32::from_rgb(0, 0, 255)
    );
    fs::remove_dir_all(&root).unwrap();
}

#[test]
fn action_icons_come_from_papirus_as_greyscale_masks() {
    let root = std::env::temp_dir().join(format!("derisk-papirus-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let write = |rel: &str| {
        let path = root.join(rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(
            &path,
            r##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16"><rect width="16" height="16" fill="#f44336"/></svg>"##,
        )
        .unwrap();
        path
    };
    // Papirus's own layout: symbolic icons beside the sized directories.
    let symbolic = write("Papirus-Dark/symbolic/actions/window-close-symbolic.svg");
    write("Papirus-Dark/16x16/actions/window-close.svg");
    let plain = write("Papirus/16x16/places/folder.svg");
    // And Papirus's layout since 2025, a level deeper under each size,
    // where the colored category icon beside it would mask to a blob.
    let science = write("Papirus/16x16/symbolic/categories/applications-science-symbolic.svg");
    write("Papirus/16x16/categories/applications-science.svg");
    // A big plain icon is a picture, never used as a mask.
    write("Papirus/48x48/actions/list-add.svg");
    let roots = [root.clone()];
    let papirus = icons::theme_chain("", &roots);
    let find = |name| icons::find_action_in(name, &roots, &papirus);

    // Without a set theme: the symbolic variant first, Papirus-Dark before
    // Papirus, any context.
    assert_eq!(find("window-close"), Some(symbolic.clone()));
    assert_eq!(find("folder"), Some(plain));
    assert_eq!(find("applications-science"), Some(science));
    assert_eq!(find("../folder"), None);
    assert_eq!(find("list-add"), None);

    // A set theme and the themes it inherits come first, Adwaita-style
    // `scalable/actions` included; Papirus fills in what they lack.
    let breeze = write("Breeze/actions/16/window-close.svg");
    let adwaita = write("Adwaita/scalable/actions/go-up-symbolic.svg");
    fs::write(
        root.join("Breeze/index.theme"),
        "[Icon Theme]\nName=Breeze\nInherits=Adwaita,hicolor\n",
    )
    .unwrap();
    let chain = icons::theme_chain("Breeze", &roots);
    assert_eq!(
        chain,
        [
            "Breeze",
            "Adwaita",
            "hicolor",
            "Papirus-Dark",
            "Papirus",
            "Papirus-Light"
        ]
    );
    let find = |name| icons::find_action_in(name, &roots, &chain);
    assert_eq!(find("window-close"), Some(breeze));
    assert_eq!(find("go-up"), Some(adwaita));
    assert_eq!(
        find("folder"),
        Some(root.join("Papirus/16x16/places/folder.svg"))
    );
    // Themes that inherit each other, or name a path, end the chain.
    fs::write(
        root.join("Adwaita/index.theme"),
        "[Icon Theme]\nInherits=Breeze,../etc\n",
    )
    .unwrap();
    assert_eq!(icons::theme_chain("Breeze", &roots).len(), 6);

    // A red icon becomes a white mask, tinted when painted.
    let mask = icons::load_mask(&symbolic, 16).unwrap();
    assert_eq!(mask.pixels[0], mcsapi::toolkit::egui::Color32::WHITE);

    // Without the theme each icon still has a glyph.
    assert_eq!(icons::glyph("system-lock-screen"), "🔒");
    assert_eq!(icons::glyph("battery-level-80-charging"), "⚡");
    assert_eq!(icons::glyph("no-such-icon"), "•");
    assert_eq!(icons::battery(84, false), "battery-level-80");
    assert_eq!(icons::battery(100, true), "battery-level-100-charging");
    assert_eq!(icons::battery(3, false), "battery-level-0");
    fs::remove_dir_all(&root).unwrap();
}
