//! `derisk-faces`: how many faces the camera sees, for the privacy display.
//!
//! ```text
//! derisk-faces watch [--device /dev/videoN] [--fps N] [--confidence N]
//! derisk-faces count <picture.ppm> [--confidence N]
//! ```
//!
//! `watch` opens the first camera (or `--device`) at 320×240, the smallest
//! size most webcams offer, and prints `faces N` for every frame it
//! searches, a few a second. The session starts it only while one of the
//! privacy display's conditions holds and stops it as soon as none does, so
//! the camera (and its light) is on only then. Frames stay in this process's
//! memory, one at a time, and are never written anywhere; only the count
//! leaves it. It prints `camera none` and exits when there is no camera to
//! open, and exits when the session stops reading.
//!
//! `count` prints the faces in a binary PPM picture, for trying the detector
//! without a camera.

use std::{io::Write, process::ExitCode};

use derisk_faces::{Bgr, Detector};

/// Below this the detector's guesses are mostly shadows and posters.
const CONFIDENCE: u8 = 70;
/// Searches a second: enough to notice someone within a moment of their
/// looking, few enough that the search stays a small share of one core.
const FPS: u32 = 4;
const WIDTH: u32 = 320;
const HEIGHT: u32 = 240;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match args.first().map(String::as_str) {
        Some("watch") => watch(&args[1..]),
        Some("count") => count(&args[1..]),
        _ => Err("usage: derisk-faces watch [--device PATH] [--fps N] [--confidence N]\n       derisk-faces count PICTURE.ppm [--confidence N]".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("derisk-faces: {error}");
            ExitCode::FAILURE
        }
    }
}

/// The value after `flag` in `args`, parsed.
fn option<T: std::str::FromStr>(args: &[String], flag: &str) -> Result<Option<T>, String> {
    match args.iter().position(|a| a == flag) {
        None => Ok(None),
        Some(i) => args
            .get(i + 1)
            .and_then(|v| v.parse().ok())
            .map(Some)
            .ok_or_else(|| format!("{flag} needs a value")),
    }
}

fn count(args: &[String]) -> Result<(), String> {
    let path = args.first().ok_or("count needs a picture")?;
    let confidence = option(args, "--confidence")?.unwrap_or(CONFIDENCE);
    let bytes = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    let (width, height, rgb) =
        ppm(&bytes).ok_or_else(|| format!("{path}: not a binary 8-bit PPM"))?;
    let bgr: Vec<u8> = rgb
        .as_chunks::<3>()
        .0
        .iter()
        .flat_map(|p| [p[2], p[1], p[0]])
        .collect();
    let image = Bgr::new(&bgr, width, height, width as usize * 3).map_err(|e| e.to_string())?;
    let mut detector = Detector::new();
    let faces: Vec<_> = detector.detect(&image, confidence).collect();
    println!("faces {}", faces.len());
    for face in faces {
        let r = face.rect;
        println!(
            "face {} at {},{} {}x{}",
            face.confidence, r.x, r.y, r.width, r.height
        );
    }
    Ok(())
}

/// A binary PPM's size and pixels: `P6`, width, height, 255, then RGB.
fn ppm(bytes: &[u8]) -> Option<(u32, u32, &[u8])> {
    let mut fields = Vec::new();
    let mut at = 0;
    while fields.len() < 4 {
        while bytes.get(at)?.is_ascii_whitespace() || *bytes.get(at)? == b'#' {
            if bytes[at] == b'#' {
                while *bytes.get(at)? != b'\n' {
                    at += 1;
                }
            }
            at += 1;
        }
        let start = at;
        while !bytes.get(at)?.is_ascii_whitespace() {
            at += 1;
        }
        fields.push(std::str::from_utf8(&bytes[start..at]).ok()?);
    }
    let (width, height): (u32, u32) = (fields[1].parse().ok()?, fields[2].parse().ok()?);
    let pixels = bytes.get(at + 1..)?;
    (fields[0] == "P6" && fields[3] == "255" && pixels.len() >= (width * height * 3) as usize)
        .then_some((width, height, pixels))
}

#[cfg(target_os = "linux")]
fn watch(args: &[String]) -> Result<(), String> {
    use std::time::{Duration, Instant};

    use derisk_faces::{grey_to_bgr, yuyv_to_bgr};
    use v4l::{
        FourCC,
        buffer::Type,
        io::{mmap::Stream, traits::CaptureStream},
        video::Capture,
    };

    let confidence = option(args, "--confidence")?.unwrap_or(CONFIDENCE);
    let fps: u32 = option(args, "--fps")?.unwrap_or(FPS).clamp(1, 30);
    let device: Option<String> = option(args, "--device")?;
    let Some(device) = open_camera(device.as_deref()) else {
        println!("camera none");
        return Ok(());
    };

    let mut format = device.format().map_err(|e| e.to_string())?;
    format.width = WIDTH;
    format.height = HEIGHT;
    format.fourcc = FourCC::new(b"YUYV");
    let format = device.set_format(&format).map_err(|e| e.to_string())?;
    let grey = match &format.fourcc.repr {
        b"YUYV" => false,
        b"GREY" => true,
        other => {
            return Err(format!(
                "the camera sends {}, not YUYV or GREY",
                String::from_utf8_lossy(other)
            ));
        }
    };
    let (width, height) = (format.width, format.height);
    // Ask the camera itself for few frames, where it lets us: fewer to
    // copy, and a slower sensor uses less power. The pacing below holds
    // either way.
    if let Ok(mut params) = device.params() {
        params.interval = v4l::Fraction::new(1, fps);
        let _ = device.set_params(&params);
    }
    let mut stream =
        Stream::with_buffers(&device, Type::VideoCapture, 2).map_err(|e| e.to_string())?;

    // The camera on one thread and the search on another, with room for
    // one frame between them: a frame that arrives while the last is still
    // being searched replaces nothing and is dropped, so the count is
    // always of a recent frame and memory holds two at most.
    let (frames, searched) = crossbeam_channel::bounded::<Vec<u8>>(1);
    let period = Duration::from_secs(1) / fps;
    std::thread::scope(|scope| {
        let searcher = scope.spawn(move || -> Result<(), String> {
            let mut detector = Detector::new();
            let mut bgr = Vec::new();
            let mut out = std::io::stdout().lock();
            for frame in searched {
                if grey {
                    grey_to_bgr(&frame, width, height, &mut bgr);
                } else {
                    yuyv_to_bgr(&frame, width, height, &mut bgr);
                }
                let image =
                    Bgr::new(&bgr, width, height, width as usize * 3).map_err(|e| e.to_string())?;
                let faces = detector.detect(&image, confidence).count();
                // The session stopped reading: nothing more to do.
                if writeln!(out, "faces {faces}")
                    .and_then(|()| out.flush())
                    .is_err()
                {
                    return Ok(());
                }
            }
            Ok(())
        });
        let mut next = Instant::now();
        let captured = loop {
            let (frame, _) = match stream.next() {
                Ok(frame) => frame,
                Err(e) => break Err(e.to_string()),
            };
            if Instant::now() < next {
                continue;
            }
            next = Instant::now() + period;
            match frames.try_send(frame.to_vec()) {
                Ok(()) | Err(crossbeam_channel::TrySendError::Full(_)) => {}
                // The searcher ended: the session went away, or it failed.
                Err(crossbeam_channel::TrySendError::Disconnected(_)) => break Ok(()),
            }
        };
        drop(frames);
        let searched = searcher
            .join()
            .unwrap_or_else(|_| Err("the search panicked".into()));
        captured.and(searched)
    })
}

#[cfg(not(target_os = "linux"))]
fn watch(_args: &[String]) -> Result<(), String> {
    println!("camera none");
    Ok(())
}

/// The camera at `path`, or the first device that captures video.
#[cfg(target_os = "linux")]
fn open_camera(path: Option<&str>) -> Option<v4l::Device> {
    use v4l::capability::Flags;
    let captures = |device: &v4l::Device| {
        device
            .query_caps()
            .is_ok_and(|caps| caps.capabilities.contains(Flags::VIDEO_CAPTURE))
    };
    if let Some(path) = path {
        return v4l::Device::with_path(path).ok().filter(captures);
    }
    // A camera registers several nodes (metadata, a second stream); the
    // first that captures video is the picture.
    (0..64).find_map(|index| v4l::Device::new(index).ok().filter(captures))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ppm_headers_are_read_with_comments() {
        let mut bytes = b"P6\n# a comment\n2 1\n255\n".to_vec();
        bytes.extend([1, 2, 3, 4, 5, 6]);
        let (w, h, pixels) = ppm(&bytes).unwrap();
        assert_eq!((w, h), (2, 1));
        assert_eq!(pixels, [1, 2, 3, 4, 5, 6]);
        assert!(ppm(b"P3\n1 1\n255\n1 2 3").is_none());
    }
}
