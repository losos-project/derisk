//! Live previews of apps an agent builds, drawn inside the Agent's window.
//!
//! A preview is a separate program (a native binary, or a `wasm32-wasip2`
//! component) that draws with egui and speaks this crate's protocol on its
//! stdin and stdout. The Agent sends it input and the desktop theme; it answers each
//! request with one tessellated frame, which the Agent paints into the right-hand
//! pane. Tessellating in the guest matters: text is laid out against the
//! guest's own font atlas, so shipping shapes instead of meshes would draw the
//! host's glyphs at the guest's texture coordinates.
//!
//! The guest side ([`serve`]) depends on egui alone, so the same app source
//! builds for Linux and for `wasm32-wasip2`. mcsapi's own crates link Smithay,
//! which does not build for wasm, so a guest gets the theme as a
//! [`PreviewTheme`] instead of an `mcsapi_ui::Theme`.

use std::io::{self, Read, Write};

use egui::{Color32, Rect, TexturesDelta, epaint::Mesh};
use serde::{Deserialize, Serialize};

#[cfg(feature = "host")]
mod host;
#[cfg(feature = "host")]
pub use host::{Preview, PreviewSource, PreviewStatus};

#[cfg(feature = "wasm")]
pub mod wasm;

/// The largest message either side accepts. A first frame carries the font
/// atlas, which is a few megabytes; anything far beyond that is a broken peer.
const MAX_MESSAGE_LEN: usize = 256 * 1024 * 1024;

/// The five desktop colors a preview draws with, mirroring `mcsapi_ui::Theme`.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PreviewTheme {
    pub background: Color32,
    pub surface: Color32,
    pub foreground: Color32,
    pub border: Color32,
    pub accent: Color32,
}

impl Default for PreviewTheme {
    fn default() -> Self {
        Self {
            background: Color32::from_rgb(24, 24, 27),
            surface: Color32::from_rgb(39, 39, 42),
            foreground: Color32::from_rgb(244, 244, 245),
            border: Color32::from_rgb(82, 82, 91),
            accent: Color32::from_rgb(96, 165, 250),
        }
    }
}

/// What the host sends.
// One is built per frame and serialized at once; boxing the frame would
// only add an allocation to every one.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Serialize, Deserialize)]
pub enum HostMessage {
    /// Run one frame with this input and answer with [`GuestMessage::Frame`].
    Frame {
        input: egui::RawInput,
        theme: PreviewTheme,
    },
    /// Exit cleanly.
    Shutdown,
}

/// What the guest sends.
#[derive(Debug, Serialize, Deserialize)]
pub enum GuestMessage {
    /// Sent once, before the first frame.
    Hello {
        title: String,
    },
    Frame(GuestFrame),
}

/// One tessellated frame, in the guest's points with its origin at the top left
/// of the preview pane.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct GuestFrame {
    pub textures_delta: TexturesDelta,
    pub meshes: Vec<ClippedMesh>,
    /// When the guest wants another frame without new input; `None` means only
    /// on input.
    pub repaint_after_ms: Option<u64>,
    pub cursor_icon: egui::CursorIcon,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ClippedMesh {
    pub clip_rect: Rect,
    pub mesh: Mesh,
}

/// Writes one length-prefixed message.
pub fn write_message(writer: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let bytes = postcard::to_stdvec(message).map_err(io::Error::other)?;
    let len = u32::try_from(bytes.len()).map_err(io::Error::other)?;
    writer.write_all(&len.to_le_bytes())?;
    writer.write_all(&bytes)?;
    writer.flush()
}

/// Reads one length-prefixed message; `Ok(None)` at a clean end of stream.
pub fn read_message<T: for<'de> Deserialize<'de>>(reader: &mut impl Read) -> io::Result<Option<T>> {
    let mut len = [0u8; 4];
    match reader.read_exact(&mut len) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MESSAGE_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("preview message of {len} bytes is over the {MAX_MESSAGE_LEN} byte limit"),
        ));
    }
    let mut bytes = vec![0u8; len];
    reader.read_exact(&mut bytes)?;
    postcard::from_bytes(&bytes)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// Runs a preview guest on stdin and stdout until the host shuts it down.
///
/// stdout belongs to the protocol, so the app must log to stderr.
pub fn serve(title: &str, draw: impl FnMut(&mut egui::Ui, &PreviewTheme)) -> io::Result<()> {
    serve_on(
        title,
        &mut io::stdin().lock(),
        &mut io::stdout().lock(),
        draw,
    )
}

/// [`serve`] over any pair of streams.
pub fn serve_on(
    title: &str,
    reader: &mut impl Read,
    writer: &mut impl Write,
    mut draw: impl FnMut(&mut egui::Ui, &PreviewTheme),
) -> io::Result<()> {
    let context = egui::Context::default();
    write_message(
        writer,
        &GuestMessage::Hello {
            title: title.to_owned(),
        },
    )?;
    while let Some(message) = read_message::<HostMessage>(reader)? {
        let (input, theme) = match message {
            HostMessage::Frame { input, theme } => (input, theme),
            HostMessage::Shutdown => break,
        };
        let mut message = GuestMessage::Frame(run_frame(&context, input, &theme, &mut draw));
        write_message(writer, &message)?;
        if let GuestMessage::Frame(frame) = &mut message {
            // Sent, so the host owns these textures now.
            frame.textures_delta.clear();
        }
    }
    Ok(())
}

fn run_frame(
    context: &egui::Context,
    input: egui::RawInput,
    theme: &PreviewTheme,
    draw: &mut impl FnMut(&mut egui::Ui, &PreviewTheme),
) -> GuestFrame {
    let output = context.run_ui(input, |ui| {
        egui::Frame::new()
            .fill(theme.background)
            .inner_margin(8)
            .show(ui, |ui| {
                ui.set_min_size(ui.available_size());
                draw(ui, theme)
            });
    });
    let repaint_after_ms = output
        .viewport_output
        .get(&egui::ViewportId::ROOT)
        .map(|viewport| viewport.repaint_delay)
        .filter(|delay| *delay < std::time::Duration::from_secs(3600))
        .map(|delay| delay.as_millis().min(u128::from(u64::MAX)) as u64);
    let meshes = context
        .tessellate(output.shapes, output.pixels_per_point)
        .into_iter()
        .filter_map(|primitive| match primitive.primitive {
            egui::epaint::Primitive::Mesh(mesh) => Some(ClippedMesh {
                clip_rect: primitive.clip_rect,
                mesh,
            }),
            // Paint callbacks run GPU code in the guest's process, which the
            // host cannot reach.
            egui::epaint::Primitive::Callback(_) => None,
        })
        .collect();
    GuestFrame {
        textures_delta: output.textures_delta,
        meshes,
        repaint_after_ms,
        cursor_icon: output.platform_output.cursor_icon,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_answers_each_frame_request_and_stops_on_shutdown() {
        let mut requests = Vec::new();
        let input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(320.0, 200.0),
            )),
            ..Default::default()
        };
        write_message(
            &mut requests,
            &HostMessage::Frame {
                input,
                theme: PreviewTheme::default(),
            },
        )
        .expect("encode frame request");
        write_message(&mut requests, &HostMessage::Shutdown).expect("encode shutdown");

        let mut replies = Vec::new();
        serve_on("Test", &mut requests.as_slice(), &mut replies, |ui, _| {
            ui.label("hello from the guest");
        })
        .expect("serve");

        let mut replies = replies.as_slice();
        match read_message::<GuestMessage>(&mut replies).expect("read hello") {
            Some(GuestMessage::Hello { title }) => assert_eq!(title, "Test"),
            other => panic!("expected hello, got {other:?}"),
        }
        match read_message::<GuestMessage>(&mut replies).expect("read frame") {
            Some(GuestMessage::Frame(mut frame)) => {
                assert!(!frame.meshes.is_empty(), "the label tessellates to a mesh");
                assert!(
                    !frame.textures_delta.set.is_empty(),
                    "the first frame carries the font atlas"
                );
                frame.textures_delta.clear();
            }
            other => panic!("expected a frame, got {other:?}"),
        }
        assert!(
            read_message::<GuestMessage>(&mut replies)
                .expect("read end")
                .is_none()
        );
    }

    #[test]
    fn oversized_messages_are_refused() {
        let len = (MAX_MESSAGE_LEN as u32 + 1).to_le_bytes();
        let error = read_message::<GuestMessage>(&mut len.as_slice()).expect_err("too large");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
