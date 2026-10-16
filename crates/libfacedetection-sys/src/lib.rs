//! libfacedetection's C++ entry point, as bindgen sees it.
//!
//! `facedetect_cnn` runs the CNN over a BGR image and writes what it found
//! into a caller-owned buffer of [`FACEDETECTION_RESULT_BUFFER_SIZE`] bytes:
//! an `int` count, then [`FACEDETECTION_RESULT_STRIDE_SHORTS`] `short`s a
//! face (confidence 0–100, x, y, width, height, then five landmarks). It
//! keeps its weights in globals it fills on the first call, so no two calls
//! may run at once. The safe wrapper is `derisk-faces`.

#![allow(non_upper_case_globals, non_camel_case_types, non_snake_case)]

include!(concat!(env!("OUT_DIR"), "/bindings.rs"));
