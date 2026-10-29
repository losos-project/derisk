//! Reading the screen's text with ocrs.
//!
//! ocrs finds the words with one model and reads each line with another;
//! both are ONNX files the OS installs, about 12 MB together. They are
//! loaded once, the first time a screenshot needs them, and kept: loading
//! takes a few milliseconds, but a screenshot can come at any time.

use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use ocrs::{ImageSource, OcrEngine, OcrEngineParams, TextItem as _};

use crate::{Image, Rect};

/// The model files, in the directory [`models_dir`] finds.
pub const DETECTION_MODEL: &str = "text-detection.onnx";
/// See [`DETECTION_MODEL`].
pub const RECOGNITION_MODEL: &str = "text-recognition.onnx";

/// One line of text and where each of its characters is.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Line {
    /// The line as read.
    pub text: String,
    /// One box for each `char` of `text`, in order; a space has the box
    /// of the gap it stands for.
    pub boxes: Vec<Rect>,
}

/// The OCR engine with its models loaded.
pub struct Ocr {
    engine: OcrEngine,
}

impl std::fmt::Debug for Ocr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Ocr").finish_non_exhaustive()
    }
}

impl Ocr {
    /// Loads the models from `dir`.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let model = |name: &str| {
            let path = dir.join(name);
            rten::Model::load_file(&path)
                .map_err(|e| format!("cannot load {}: {e}", path.display()))
        };
        let engine = OcrEngine::new(OcrEngineParams {
            detection_model: Some(model(DETECTION_MODEL)?),
            recognition_model: Some(model(RECOGNITION_MODEL)?),
            ..OcrEngineParams::default()
        })
        .map_err(|e| e.to_string())?;
        Ok(Self { engine })
    }

    /// The engine every screenshot shares, loaded from [`models_dir`] the
    /// first time; the error says why there is none, every time.
    pub fn shared() -> Result<&'static Self, &'static str> {
        static SHARED: OnceLock<Result<Ocr, String>> = OnceLock::new();
        SHARED
            .get_or_init(|| {
                let dir = models_dir().ok_or_else(|| {
                    format!(
                        "the OCR models ({DETECTION_MODEL}, {RECOGNITION_MODEL}) are not installed"
                    )
                })?;
                Self::load(&dir)
            })
            .as_ref()
            .map_err(String::as_str)
    }

    /// Every line of text in `image`.
    pub fn lines(&self, image: &Image<'_>) -> Result<Vec<Line>, String> {
        let rgb: Vec<u8> = image
            .rgba
            .chunks_exact(4)
            .flat_map(|p| [p[0], p[1], p[2]])
            .collect();
        let source = ImageSource::from_bytes(&rgb, (image.width, image.height))
            .map_err(|e| e.to_string())?;
        let input = self
            .engine
            .prepare_input(source)
            .map_err(|e| e.to_string())?;
        let words = self
            .engine
            .detect_words(&input)
            .map_err(|e| e.to_string())?;
        let lines = self.engine.find_text_lines(&input, &words);
        let read = self
            .engine
            .recognize_text(&input, &lines)
            .map_err(|e| e.to_string())?;
        Ok(read
            .into_iter()
            .flatten()
            .map(|line| {
                let mut out = Line::default();
                for c in line.chars() {
                    let r = c.rect;
                    out.text.push(c.char);
                    out.boxes.push(Rect {
                        x: r.left().max(0) as u32,
                        y: r.top().max(0) as u32,
                        width: r.width().max(0) as u32,
                        height: r.height().max(0) as u32,
                    });
                }
                out
            })
            .collect())
    }
}

/// Where the models are: `$DERISK_OCR_MODELS`, then `share/derisk/ocr`
/// beside the running program's `bin` (how Nix installs them), then
/// `derisk/ocr` under each of `$XDG_DATA_DIRS`.
pub fn models_dir() -> Option<PathBuf> {
    let has_models =
        |dir: &Path| dir.join(DETECTION_MODEL).is_file() && dir.join(RECOGNITION_MODEL).is_file();
    if let Some(dir) = std::env::var_os("DERISK_OCR_MODELS") {
        return Some(PathBuf::from(dir)).filter(|d| has_models(d));
    }
    let beside = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.parent()?.join("share/derisk/ocr")));
    let data_dirs = std::env::var_os("XDG_DATA_DIRS")
        .map(|dirs| {
            std::env::split_paths(&dirs)
                .map(|d| d.join("derisk/ocr"))
                .collect()
        })
        .unwrap_or_else(|| {
            vec![
                PathBuf::from("/usr/local/share/derisk/ocr"),
                PathBuf::from("/usr/share/derisk/ocr"),
            ]
        });
    beside.into_iter().chain(data_dirs).find(|d| has_models(d))
}
