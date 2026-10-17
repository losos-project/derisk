//! derisk Terminal: a small terminal emulator for the derisk desktop.
//!
//! - **Emulation** is alacritty_terminal's: the VT parser, the grid and its
//!   scrollback, selection, and the pty ([`pty`]).
//! - **Glyphs** come from the theme's monospace family through its Nerd
//!   Font build, so the icons prompts and TUIs print draw like letters
//!   ([`font`]), rasterised once into an atlas ([`atlas`]). Box-drawing and
//!   block characters are drawn to the cell instead, so borders join
//!   ([`boxdraw`]).
//! - **Drawing is deferred**: output only changes the grid, and a frame is
//!   built from it when the compositor is ready for one ([`frame`]), however
//!   much was printed in between.
//! - **The GPU draws the frame** in one instanced call ([`gpu`]); without a
//!   GPU, or with only a software OpenGL, the CPU draws the same frame into
//!   shared memory ([`cpu`]).
//!
//! Colours and the font follow the derisk theme ([`theme`]).

pub mod app;
pub mod atlas;
pub mod boxdraw;
pub mod cpu;
pub mod font;
pub mod frame;
pub mod gpu;
pub mod input;
pub mod pty;
pub mod theme;

use std::{path::PathBuf, thread};

use alacritty_terminal::event::WindowSize;
use winit::event_loop::EventLoop;

/// The window's app ID, which the desktop entry is named after.
pub const APP_ID: &str = "org.derisk.terminal";
/// The window title until the shell sets one.
pub const TITLE: &str = "Terminal";

/// What the desktop calls the window.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Name {
    /// The app ID, so a launcher can give its window its own entry's name
    /// and icon, as `foot --app-id` does.
    pub app_id: String,
    /// The title until the shell sets one, and again when it resets it.
    pub title: String,
}

impl Default for Name {
    fn default() -> Self {
        Self {
            app_id: APP_ID.to_owned(),
            title: TITLE.to_owned(),
        }
    }
}

/// Which renderer to use.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Renderer {
    /// The GPU when there is one, else software.
    #[default]
    Auto,
    /// The GPU, even a software OpenGL such as llvmpipe.
    Gpu,
    /// The CPU, whatever GPU there is.
    Software,
}

impl Renderer {
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "auto" => Some(Self::Auto),
            "gpu" => Some(Self::Gpu),
            "software" => Some(Self::Software),
            _ => None,
        }
    }
}

/// What to run, and where.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// The program and its arguments; the user's shell when `None`.
    pub command: Option<Vec<String>>,
    pub directory: Option<PathBuf>,
    pub renderer: Renderer,
    pub name: Name,
}

#[derive(Debug, thiserror::Error, miette::Diagnostic)]
pub enum Error {
    #[error(transparent)]
    #[diagnostic(transparent)]
    Font(#[from] font::Error),
    #[error("no display: {0}")]
    #[diagnostic(help("derisk-terminal is a Wayland (or X11) app; run it inside the session"))]
    Display(#[from] winit::error::EventLoopError),
    #[error("starting the shell: {0}")]
    Shell(#[source] std::io::Error),
}

/// Opens the window and runs the shell in it until either ends.
pub fn run(options: Options) -> Result<(), Error> {
    let mut watch = theme::Watch::new(theme::published_path());
    let look = watch.poll().expect("the first poll always answers");
    let fonts = font::Fonts::new(&look.family, look.size)?;
    let cell = fonts.cell();
    let event_loop = EventLoop::<app::Wake>::with_user_event().build()?;

    let pty = pty::spawn(
        options.command,
        options.directory,
        WindowSize {
            num_lines: 24,
            num_cols: 80,
            cell_width: cell.width as u16,
            cell_height: cell.height as u16,
        },
    )
    .map_err(Error::Shell)?;
    let stop = pty::Stop::new().map_err(Error::Shell)?;
    // A handful of 64 KiB reads in flight is plenty: past that the reader
    // waits, and so does the shell.
    let (output_tx, output_rx) = crossbeam_channel::bounded(16);
    let (input_tx, input_rx) = crossbeam_channel::bounded(256);
    let proxy = event_loop.create_proxy();
    let file = pty.file();

    let result = thread::scope(|s| {
        let stop = &stop;
        s.spawn(move || {
            pty::read(file, stop, output_tx, || {
                let _ = proxy.send_event(app::Wake::Output);
            });
            let _ = proxy.send_event(app::Wake::Exited);
        });
        s.spawn(move || pty::write(file, stop, input_rx));
        let mut app = app::App::new(
            options.renderer,
            options.name,
            file,
            output_rx,
            input_tx,
            atlas::Glyphs::new(fonts),
            (look, watch),
        );
        let result = event_loop.run_app(&mut app);
        // Hanging up both channels and signalling the eventfd ends the two
        // threads however the event loop ended, so the scope can close.
        drop(app);
        stop.signal();
        result
    });
    // Dropping the pty hangs up on the shell (SIGHUP) and reaps it.
    drop(pty);
    Ok(result?)
}
