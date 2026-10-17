//! The GPU renderer: the frame as one instanced draw call through OpenGL ES
//! 3 (or desktop OpenGL 3.3 where only that is offered), over EGL.
//!
//! Every quad of the frame, rectangles first and glyphs after, goes into one
//! instance buffer; a vertex shader turns each into a rectangle and a
//! fragment shader either fills it or tints the glyph's coverage from the
//! atlas texture. The atlas lives in memory ([`Atlas`]) and only the rows
//! that changed are uploaded, so a frame of text already seen uploads
//! nothing but the quads.

use std::{num::NonZeroU32, rc::Rc};

use glow::HasContext;
use glutin::{
    config::{ConfigTemplateBuilder, GlConfig},
    context::{ContextApi, ContextAttributesBuilder, PossiblyCurrentContext, Version},
    display::{GetGlDisplay, GlDisplay},
    prelude::{GlSurface, NotCurrentGlContext},
    surface::{Surface, SwapInterval, WindowSurface},
};
use glutin_winit::{DisplayBuilder, GlWindow};
use raw_window_handle::HasWindowHandle;
use winit::{
    event_loop::ActiveEventLoop,
    window::{Window, WindowAttributes},
};

use crate::{atlas::Atlas, frame::Frame};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no OpenGL display: {0}")]
    Display(String),
    #[error("no OpenGL ES 3 or OpenGL 3.3 context: {0}")]
    Context(#[source] glutin::error::Error),
    #[error("no OpenGL surface for the window: {0}")]
    Surface(String),
    #[error("OpenGL is drawn in software here ({0}), which the software renderer does faster")]
    Software(String),
    #[error("the shaders did not build: {0}")]
    Shader(String),
}

pub struct Renderer {
    gl: glow::Context,
    program: glow::Program,
    vao: glow::VertexArray,
    instances: glow::Buffer,
    texture: glow::Texture,
    viewport: Option<glow::UniformLocation>,
    atlas_size: Option<glow::UniformLocation>,
    /// The atlas size the texture was last allocated at.
    uploaded: Option<(u32, u32)>,
    /// Instance data, kept between frames so its allocation is.
    scratch: Vec<f32>,
    // Drop the surface before the context it was made current with.
    surface: Surface<WindowSurface>,
    context: PossiblyCurrentContext,
    /// GL_RENDERER, for the log.
    pub name: String,
}

/// A window that could not be given a GPU renderer, if one was made, and
/// why not.
pub type Failure = (Option<Rc<Window>>, Error);

/// Renderers whose GL_RENDERER says they rasterise on the CPU.
const SOFTWARE: [&str; 4] = ["llvmpipe", "softpipe", "swrast", "SwiftShader"];

/// Floats per instance: the rectangle, the atlas rectangle, the colour.
const STRIDE: usize = 12;

impl Renderer {
    /// A window and a renderer for it. When the window was made but no GPU
    /// renderer could be, the window comes back with the error, so the
    /// software renderer can draw into it instead. A software GL counts as no
    /// GPU unless `software_ok`.
    #[allow(clippy::result_large_err)]
    pub fn new(
        event_loop: &ActiveEventLoop,
        attributes: WindowAttributes,
        software_ok: bool,
    ) -> Result<(Rc<Window>, Self), Failure> {
        let template = ConfigTemplateBuilder::new();
        let (window, config) = DisplayBuilder::new()
            .with_window_attributes(Some(attributes))
            .build(event_loop, template, |configs| {
                // Any opaque config: the terminal paints every pixel.
                configs
                    .reduce(|best, c| {
                        if c.num_samples() < best.num_samples() {
                            c
                        } else {
                            best
                        }
                    })
                    .expect("EGL reported a display with no configs")
            })
            .map_err(|e| (None, Error::Display(e.to_string())))?;
        let Some(window) = window.map(Rc::new) else {
            return Err((None, Error::Display("no window".into())));
        };
        match Self::with_window(&window, &config, software_ok) {
            Ok(renderer) => Ok((window, renderer)),
            Err(error) => Err((Some(window), error)),
        }
    }

    fn with_window(
        window: &Window,
        config: &glutin::config::Config,
        software_ok: bool,
    ) -> Result<Self, Error> {
        let display = config.display();
        let handle = window
            .window_handle()
            .map_err(|e| Error::Surface(e.to_string()))?
            .as_raw();
        let gles = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::Gles(Some(Version::new(3, 0))))
            .build(Some(handle));
        let desktop = ContextAttributesBuilder::new()
            .with_context_api(ContextApi::OpenGl(Some(Version::new(3, 3))))
            .build(Some(handle));
        // SAFETY: the handle is the live window's, which outlives the
        // context: the renderer is dropped before the window it was given.
        let (context, es) = unsafe {
            match display.create_context(config, &gles) {
                Ok(context) => (context, true),
                Err(_) => (
                    display
                        .create_context(config, &desktop)
                        .map_err(Error::Context)?,
                    false,
                ),
            }
        };
        let attributes = window
            .build_surface_attributes(Default::default())
            .map_err(|e| Error::Surface(e.to_string()))?;
        // SAFETY: as above, the window outlives the surface.
        let surface = unsafe { display.create_window_surface(config, &attributes) }
            .map_err(|e| Error::Surface(e.to_string()))?;
        let context = context.make_current(&surface).map_err(Error::Context)?;
        // Wait for the compositor's frame: one swap per displayed frame is
        // what keeps a flood of output from drawing frames nobody sees.
        let _ = surface.set_swap_interval(&context, SwapInterval::Wait(NonZeroU32::MIN));

        // SAFETY: the context is current on this thread, and glow only calls
        // the functions it loads with it current.
        let gl = unsafe {
            glow::Context::from_loader_function_cstr(|name| display.get_proc_address(name))
        };
        // SAFETY: from here on every call is on the current context.
        unsafe {
            let name = gl.get_parameter_string(glow::RENDERER);
            if !software_ok && SOFTWARE.iter().any(|s| name.contains(s)) {
                return Err(Error::Software(name));
            }
            let program = program(&gl, es)?;
            let vao = gl.create_vertex_array().map_err(Error::Shader)?;
            let instances = gl.create_buffer().map_err(Error::Shader)?;
            gl.bind_vertex_array(Some(vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(instances));
            for location in 0..3u32 {
                gl.enable_vertex_attrib_array(location);
                gl.vertex_attrib_pointer_f32(
                    location,
                    4,
                    glow::FLOAT,
                    false,
                    (STRIDE * 4) as i32,
                    (location * 16) as i32,
                );
                gl.vertex_attrib_divisor(location, 1);
            }
            let texture = gl.create_texture().map_err(Error::Shader)?;
            gl.bind_texture(glow::TEXTURE_2D, Some(texture));
            // Glyph quads are placed on whole pixels at the size they were
            // rasterised at, so nearest sampling is exact, and never blurs.
            for (parameter, value) in [
                (glow::TEXTURE_MIN_FILTER, glow::NEAREST),
                (glow::TEXTURE_MAG_FILTER, glow::NEAREST),
                (glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE),
                (glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE),
            ] {
                gl.tex_parameter_i32(glow::TEXTURE_2D, parameter, value as i32);
            }
            gl.pixel_store_i32(glow::UNPACK_ALIGNMENT, 1);
            gl.use_program(Some(program));
            let viewport = gl.get_uniform_location(program, "viewport");
            let atlas_size = gl.get_uniform_location(program, "atlas_size");
            let sampler = gl.get_uniform_location(program, "atlas");
            gl.uniform_1_i32(sampler.as_ref(), 0);
            gl.enable(glow::BLEND);
            // The fragment shader outputs premultiplied colour.
            gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);
            Ok(Self {
                gl,
                program,
                vao,
                instances,
                texture,
                viewport,
                atlas_size,
                uploaded: None,
                scratch: Vec::new(),
                surface,
                context,
                name,
            })
        }
    }

    /// Follows a window resize.
    pub fn resize(&self, window: &Window) {
        window.resize_surface(&self.surface, &self.context);
    }

    pub fn draw(&mut self, window: &Window, frame: &Frame, atlas: &mut Atlas) {
        self.scratch.clear();
        for quad in frame.rects.iter().chain(&frame.glyphs) {
            let (u, v, solid) = match quad.atlas {
                Some((u, v)) => (u as f32, v as f32, 0.0),
                None => (0.0, 0.0, 1.0),
            };
            let c = |v: u8| f32::from(v) / 255.0;
            self.scratch.extend_from_slice(&[
                quad.x as f32,
                quad.y as f32,
                quad.width as f32,
                quad.height as f32,
                u,
                v,
                quad.width as f32,
                quad.height as f32,
                c(quad.color.r),
                c(quad.color.g),
                c(quad.color.b),
                solid,
            ]);
        }
        let gl = &self.gl;
        // SAFETY: the context is current: nothing else makes one current on
        // this thread.
        unsafe {
            gl.bind_texture(glow::TEXTURE_2D, Some(self.texture));
            let size = (atlas.width(), atlas.height());
            let dirty = atlas.take_dirty();
            if self.uploaded != Some(size) {
                gl.tex_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    glow::R8 as i32,
                    size.0 as i32,
                    size.1 as i32,
                    0,
                    glow::RED,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(atlas.pixels())),
                );
                self.uploaded = Some(size);
            } else if let Some((start, end)) = dirty {
                let rows = &atlas.pixels()[(start * size.0) as usize..(end * size.0) as usize];
                gl.tex_sub_image_2d(
                    glow::TEXTURE_2D,
                    0,
                    0,
                    start as i32,
                    size.0 as i32,
                    (end - start) as i32,
                    glow::RED,
                    glow::UNSIGNED_BYTE,
                    glow::PixelUnpackData::Slice(Some(rows)),
                );
            }

            gl.viewport(0, 0, frame.width as i32, frame.height as i32);
            let c = |v: u8| f32::from(v) / 255.0;
            gl.clear_color(c(frame.clear.r), c(frame.clear.g), c(frame.clear.b), 1.0);
            gl.clear(glow::COLOR_BUFFER_BIT);
            gl.use_program(Some(self.program));
            gl.uniform_2_f32(
                self.viewport.as_ref(),
                frame.width as f32,
                frame.height as f32,
            );
            gl.uniform_2_f32(self.atlas_size.as_ref(), size.0 as f32, size.1 as f32);
            gl.bind_vertex_array(Some(self.vao));
            gl.bind_buffer(glow::ARRAY_BUFFER, Some(self.instances));
            let bytes = std::slice::from_raw_parts(
                self.scratch.as_ptr().cast::<u8>(),
                self.scratch.len() * 4,
            );
            gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytes, glow::STREAM_DRAW);
            gl.draw_arrays_instanced(
                glow::TRIANGLE_STRIP,
                0,
                4,
                (self.scratch.len() / STRIDE) as i32,
            );
        }
        window.pre_present_notify();
        if let Err(error) = self.surface.swap_buffers(&self.context) {
            tracing::warn!("swapping buffers: {error}");
        }
    }
}

impl Drop for Renderer {
    fn drop(&mut self) {
        // SAFETY: still current; these are this renderer's own objects.
        unsafe {
            self.gl.delete_program(self.program);
            self.gl.delete_vertex_array(self.vao);
            self.gl.delete_buffer(self.instances);
            self.gl.delete_texture(self.texture);
        }
    }
}

const VERTEX: &str = r#"
layout(location = 0) in vec4 rect;
layout(location = 1) in vec4 uv;
layout(location = 2) in vec4 color;
uniform vec2 viewport;
uniform vec2 atlas_size;
out vec2 v_uv;
out vec3 v_color;
flat out float v_solid;
void main() {
    // The four corners of a triangle strip, from the vertex number alone.
    vec2 corner = vec2(float(gl_VertexID & 1), float(gl_VertexID >> 1));
    vec2 position = rect.xy + corner * rect.zw;
    gl_Position = vec4(position / viewport * vec2(2.0, -2.0) + vec2(-1.0, 1.0), 0.0, 1.0);
    v_uv = (uv.xy + corner * uv.zw) / atlas_size;
    v_color = color.rgb;
    v_solid = color.a;
}
"#;

const FRAGMENT: &str = r#"
precision mediump float;
uniform sampler2D atlas;
in vec2 v_uv;
in vec3 v_color;
flat in float v_solid;
out vec4 out_color;
void main() {
    float a = v_solid > 0.5 ? 1.0 : texture(atlas, v_uv).r;
    out_color = vec4(v_color * a, a);
}
"#;

/// Compiles the program, in GLSL ES 3.00 or, on a desktop context, 3.30:
/// the same source, which uses nothing the two disagree on.
unsafe fn program(gl: &glow::Context, es: bool) -> Result<glow::Program, Error> {
    let header = if es {
        "#version 300 es\n"
    } else {
        "#version 330 core\n"
    };
    // SAFETY: the caller has the context current.
    unsafe {
        let program = gl.create_program().map_err(Error::Shader)?;
        let mut shaders = Vec::new();
        for (kind, source) in [
            (glow::VERTEX_SHADER, VERTEX),
            (glow::FRAGMENT_SHADER, FRAGMENT),
        ] {
            let shader = gl.create_shader(kind).map_err(Error::Shader)?;
            gl.shader_source(shader, &format!("{header}{source}"));
            gl.compile_shader(shader);
            if !gl.get_shader_compile_status(shader) {
                return Err(Error::Shader(gl.get_shader_info_log(shader)));
            }
            gl.attach_shader(program, shader);
            shaders.push(shader);
        }
        gl.link_program(program);
        if !gl.get_program_link_status(program) {
            return Err(Error::Shader(gl.get_program_info_log(program)));
        }
        for shader in shaders {
            gl.detach_shader(program, shader);
            gl.delete_shader(shader);
        }
        Ok(program)
    }
}
