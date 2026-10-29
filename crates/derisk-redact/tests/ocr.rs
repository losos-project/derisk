//! Redacting a real screenshot end to end: OCR, patterns, black boxes, and
//! OCR again to check nothing secret can still be read.
//!
//! Needs the OCR models, from `$DERISK_OCR_MODELS` as the Nix checks set it;
//! without them the test says so and passes, as `cargo test` on a machine
//! without the models should.

use derisk_redact::{Kind, Ocr, ocr, patterns, redact};

/// A dark settings panel and a terminal, full of secrets and a few lines
/// that are not.
const SCREEN: &[u8] = include_bytes!("data/screen.png");

fn decode(png: &[u8]) -> (u32, u32, Vec<u8>) {
    let mut reader = png::Decoder::new(std::io::Cursor::new(png))
        .read_info()
        .expect("a PNG");
    let mut buf = vec![0; reader.output_buffer_size().expect("a size")];
    let info = reader.next_frame(&mut buf).expect("a frame");
    buf.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buf,
        png::ColorType::Rgb => buf
            .chunks_exact(3)
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        other => panic!("unexpected {other:?}"),
    };
    (info.width, info.height, rgba)
}

#[test]
fn nothing_secret_can_be_read_after_redacting() {
    let Some(dir) = ocr::models_dir() else {
        eprintln!("skipped: no OCR models (set DERISK_OCR_MODELS)");
        return;
    };
    let ocr = Ocr::load(&dir).expect("the models load");
    let (width, height, mut rgba) = decode(SCREEN);

    let report = redact(width, height, &mut rgba, &[], Ok(&ocr));
    assert_eq!(report.unread, None);
    let counts = report.counts();
    for kind in [
        Kind::Email,
        Kind::Card,
        Kind::Iban,
        Kind::Password,
        Kind::Key,
        Kind::Onion,
        Kind::Phone,
        Kind::Address,
    ] {
        assert!(
            counts.iter().any(|(k, _)| *k == kind),
            "no {} blacked out: {counts:?}",
            kind.singular()
        );
    }

    // For looking at: target/tmp/redacted.png.
    let out = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("redacted.png");
    let mut encoder =
        png::Encoder::new(std::fs::File::create(&out).expect("a file"), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("a header");
    writer.write_image_data(&rgba).expect("the pixels");
    writer.finish().expect("the end");

    let image = derisk_redact::Image {
        width,
        height,
        rgba: &rgba,
    };
    let after = ocr.lines(&image).expect("the redacted screen reads");
    let text: Vec<&str> = after.iter().map(|l| l.text.as_str()).collect();
    for line in &text {
        assert!(patterns::find(line).is_empty(), "still readable: {line:?}");
    }
    // What is not secret is left alone.
    // OCR reads the rest a little differently once boxes sit beside it
    // ("sunnv", "ouild"), so only a part of each line is compared.
    for kept in ["Weather today", "finished in", "Recover", "Signed in as"] {
        assert!(
            text.iter().any(|l| l.contains(kept)),
            "lost {kept:?} from {text:#?}"
        );
    }
}
