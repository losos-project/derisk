use std::path::Path;

use derisk::{
    geom::rect,
    wallpaper::{
        VIDEO_FPS, covered, decode_size, ffmpeg_args, lock_screen, placement, shuffled,
        slideshow_files, video_size,
    },
};
use derisk_settings::{Fit, Rgb, Wallpaper, WallpaperKind};
use mcsapi::toolkit::egui::{Rect, pos2, vec2};

fn screen() -> Rect {
    Rect::from_min_size(pos2(0.0, 0.0), vec2(1920.0, 1080.0))
}

#[test]
fn fill_crops_the_overflow_and_keeps_the_shape() {
    // A square picture on a 16:9 screen loses its top and bottom.
    let (rect, uv) = placement(Fit::Fill, [1000, 1000], screen());
    assert_eq!(rect, screen());
    assert!((uv.width() - 1.0).abs() < 1e-6);
    assert!((uv.height() - 1080.0 / 1920.0).abs() < 1e-6);
    assert!((uv.center().y - 0.5).abs() < 1e-6);
}

#[test]
fn fit_letterboxes_center_crops_and_tile_repeats() {
    let (rect, uv) = placement(Fit::Fit, [1000, 1000], screen());
    assert_eq!(rect.size(), vec2(1080.0, 1080.0));
    assert_eq!(rect.center(), screen().center());
    assert_eq!(uv, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)));

    let (rect, uv) = placement(Fit::Center, [3840, 500], screen());
    assert_eq!(rect.size(), vec2(1920.0, 500.0));
    assert!((uv.width() - 0.5).abs() < 1e-6);

    let (rect, uv) = placement(Fit::Tile, [480, 270], screen());
    assert_eq!(rect, screen());
    assert_eq!(uv.max, pos2(4.0, 4.0));
}

#[test]
fn pictures_decode_no_larger_than_shown() {
    // A 6000×4000 photo filling a 1920×1080 screen needs 1920 wide.
    assert_eq!(
        decode_size(Fit::Fill, [6000, 4000], [1920, 1080], 8192),
        [1920, 1280]
    );
    // Never upscaled; capped by the texture limit.
    assert_eq!(
        decode_size(Fit::Fill, [800, 600], [1920, 1080], 8192),
        [800, 600]
    );
    assert_eq!(
        decode_size(Fit::Center, [6000, 3000], [1920, 1080], 2048),
        [2048, 1024]
    );
}

#[test]
fn a_window_covering_the_work_area_hides_the_wallpaper() {
    let area = rect(0, 28, 1920, 1052);
    assert!(covered(area, [rect(0, 28, 1920, 1052)]));
    assert!(covered(area, [rect(-10, 0, 1940, 1100)]));
    // Tiles with gaps between them leave the wallpaper showing.
    assert!(!covered(
        area,
        [rect(8, 36, 948, 1036), rect(964, 36, 948, 1036)]
    ));
    assert!(!covered(area, []));
}

#[test]
fn shuffles_are_permutations() {
    let order = shuffled(50, 42);
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(sorted, (0..50).collect::<Vec<_>>());
    assert_ne!(order, sorted);
    assert_eq!(shuffled(50, 42), order);
    assert!(shuffled(0, 1).is_empty());
}

#[test]
fn videos_decode_at_most_1080p_with_even_sides() {
    assert_eq!(video_size([1920, 1080]), [1920, 1080]);
    let [w, h] = video_size([3840, 2160]);
    assert_eq!([w, h], [1920, 1080]);
    let [w, h] = video_size([2561, 1441]);
    assert!(w % 2 == 0 && h % 2 == 0 && w * h <= 1920 * 1080);
}

#[test]
fn ffmpeg_writes_looping_raw_frames_at_the_screen_size() {
    let args: Vec<_> =
        ffmpeg_args(Path::new("/v/a b.mp4"), [1280, 720], Fit::Fit, Rgb(1, 2, 3)).collect();
    let at = |flag: &str| args[args.iter().position(|a| a == flag).unwrap() + 1].clone();
    assert_eq!(at("-i"), "/v/a b.mp4");
    assert_eq!(at("-stream_loop"), "-1");
    assert_eq!(at("-pix_fmt"), "rgba");
    assert_eq!(at("-f"), "rawvideo");
    let vf = at("-vf");
    assert!(vf.starts_with(&format!("fps={VIDEO_FPS},")), "{vf}");
    assert!(
        vf.contains("pad=1280:720") && vf.contains("color=0x010203"),
        "{vf}"
    );
    assert!(args.contains(&"-an".to_owned()));
    assert_eq!(args.last().unwrap(), "pipe:1");
}

#[test]
fn slideshows_take_pictures_only() {
    let dir = std::env::temp_dir().join(format!("derisk-slides-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for name in ["b.webp", "a.JPG", "c.txt", "d.mp4"] {
        std::fs::write(dir.join(name), b"").unwrap();
    }
    assert_eq!(
        slideshow_files(&dir),
        [dir.join("a.JPG"), dir.join("b.webp")]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_lock_screen_shows_its_own_picture_or_the_gradient() {
    let mut desktop = Wallpaper {
        kind: WallpaperKind::Video,
        path: "/home/me/Videos/loop.webm".into(),
        fit: Fit::Center,
        ..Wallpaper::default()
    };
    assert_eq!(lock_screen(&desktop), Wallpaper::default());
    desktop.lock_screen = "/home/me/Pictures/lake.jpg".into();
    let lock = lock_screen(&desktop);
    assert_eq!(lock.kind, WallpaperKind::Image);
    assert_eq!(lock.path, Path::new("/home/me/Pictures/lake.jpg"));
    assert_eq!(lock.fit, Fit::Fill);
}
