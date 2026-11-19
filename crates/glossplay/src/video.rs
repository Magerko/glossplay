use eframe::egui;
use eframe::egui_glow;
use eframe::glow::{self, HasContext};
use gp_media::mpv::RenderContext;
use std::sync::{Arc, Mutex};

#[derive(Default)]
struct Target {
    fbo: Option<glow::Framebuffer>,
    texture: Option<glow::Texture>,
    width: i32,
    height: i32,
}

/// Draws mpv's video into a rectangle of the egui layout.
///
/// mpv renders into my own framebuffer, which is then blitted on the GPU into egui's.
/// Rendering straight into the window only works when the video covers all of it.
pub struct VideoSurface {
    pub render: Arc<RenderContext>,
    target: Arc<Mutex<Target>>,
}

impl VideoSurface {
    pub fn new(render: RenderContext) -> Self {
        VideoSurface {
            render: Arc::new(render),
            target: Arc::default(),
        }
    }

    pub fn paint(&self, ui: &egui::Ui, rect: egui::Rect) {
        let render = self.render.clone();
        let target = self.target.clone();
        let callback = egui_glow::CallbackFn::new(move |info, painter| {
            let gl = painter.gl();
            let vp = info.viewport_in_pixels();
            let (w, h) = (vp.width_px, vp.height_px);
            if w <= 0 || h <= 0 {
                return;
            }
            let mut t = target.lock().unwrap();
            unsafe {
                if t.fbo.is_none() || t.width != w || t.height != h {
                    release(gl, &mut t);
                    let texture = gl.create_texture().ok();
                    gl.bind_texture(glow::TEXTURE_2D, texture);
                    gl.tex_image_2d(
                        glow::TEXTURE_2D,
                        0,
                        glow::RGBA8 as i32,
                        w,
                        h,
                        0,
                        glow::RGBA,
                        glow::UNSIGNED_BYTE,
                        glow::PixelUnpackData::Slice(None),
                    );
                    gl.tex_parameter_i32(
                        glow::TEXTURE_2D,
                        glow::TEXTURE_MIN_FILTER,
                        glow::LINEAR as i32,
                    );
                    gl.tex_parameter_i32(
                        glow::TEXTURE_2D,
                        glow::TEXTURE_MAG_FILTER,
                        glow::LINEAR as i32,
                    );
                    gl.bind_texture(glow::TEXTURE_2D, None);
                    let fbo = gl.create_framebuffer().ok();
                    gl.bind_framebuffer(glow::FRAMEBUFFER, fbo);
                    gl.framebuffer_texture_2d(
                        glow::FRAMEBUFFER,
                        glow::COLOR_ATTACHMENT0,
                        glow::TEXTURE_2D,
                        texture,
                        0,
                    );
                    *t = Target {
                        fbo,
                        texture,
                        width: w,
                        height: h,
                    };
                }
                let Some(fbo) = t.fbo else { return };
                render.render(fbo.0.get() as i32, w, h, true);
                gl.bind_framebuffer(glow::READ_FRAMEBUFFER, Some(fbo));
                gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, painter.intermediate_fbo());
                gl.blit_framebuffer(
                    0,
                    0,
                    w,
                    h,
                    vp.left_px,
                    vp.from_bottom_px,
                    vp.left_px + w,
                    vp.from_bottom_px + h,
                    glow::COLOR_BUFFER_BIT,
                    glow::NEAREST,
                );
                gl.bind_framebuffer(glow::FRAMEBUFFER, painter.intermediate_fbo());
            }
        });
        ui.painter().add(egui::PaintCallback {
            rect,
            callback: Arc::new(callback),
        });
    }

    /// Must be called from `on_exit`, while the GL context is still current.
    pub fn destroy(&self, gl: Option<&glow::Context>) {
        if let Some(gl) = gl {
            unsafe { release(gl, &mut self.target.lock().unwrap()) };
        }
        self.render.destroy();
    }
}

unsafe fn release(gl: &glow::Context, t: &mut Target) {
    unsafe {
        if let Some(fbo) = t.fbo.take() {
            gl.delete_framebuffer(fbo);
        }
        if let Some(texture) = t.texture.take() {
            gl.delete_texture(texture);
        }
    }
}

/// Reads back the finished window contents; used for self-test screenshots.
pub type Capture = Arc<Mutex<Option<(u32, u32, Vec<u8>)>>>;

pub fn capture_callback(rect: egui::Rect, out: Capture) -> egui::PaintCallback {
    let callback = egui_glow::CallbackFn::new(move |info, painter| {
        let gl = painter.gl();
        let (w, h) = (info.screen_size_px[0] as i32, info.screen_size_px[1] as i32);
        let mut pixels = vec![0u8; (w * h * 4) as usize];
        unsafe {
            gl.bind_framebuffer(glow::READ_FRAMEBUFFER, painter.intermediate_fbo());
            gl.pixel_store_i32(glow::PACK_ALIGNMENT, 1);
            gl.read_pixels(
                0,
                0,
                w,
                h,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelPackData::Slice(Some(&mut pixels)),
            );
        }
        *out.lock().unwrap() = Some((w as u32, h as u32, pixels));
    });
    egui::PaintCallback {
        rect,
        callback: Arc::new(callback),
    }
}

pub fn save_png(
    path: &std::path::Path,
    width: u32,
    height: u32,
    rgba_bottom_up: &[u8],
) -> std::io::Result<()> {
    let row = (width * 4) as usize;
    let mut flipped = Vec::with_capacity(rgba_bottom_up.len());
    for line in rgba_bottom_up.chunks_exact(row).rev() {
        flipped.extend_from_slice(line);
    }
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut encoder = png::Encoder::new(file, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(std::io::Error::other)?;
    writer
        .write_image_data(&flipped)
        .map_err(std::io::Error::other)
}
