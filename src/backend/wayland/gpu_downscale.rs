// SPDX-License-Identifier: GPL-3.0-only
//
// Stage 2 of dmabuf capture: imports the compositor-written dmabuf as a GL
// texture and downscales it on the GPU, so only the small preview image
// (tens of KB) crosses back to the CPU instead of the full frame (tens of
// MB at 4K). Uses an isolated EGL/GLES context on the same GBM device the
// buffer was allocated from - entirely separate from iced's own wgpu
// renderer, so a driver hiccup here can't take down the UI.
//
// dmabuf-imported textures must be sampled as `GL_TEXTURE_EXTERNAL_OES`
// (the `GL_OES_EGL_image_external` extension), since the driver may need to
// detile/decompress on the fly (as is the case for our Intel Y-tiled
// buffers) - `GL_TEXTURE_2D` only supports a plain linear layout. External
// OES textures can't be attached to a framebuffer for a cheap
// `glBlitFramebuffer`, so a small shader pass samples it into a
// regular-sized destination texture instead, which we then read back.

use gbm::AsRaw;
use khronos_egl as egl;

use std::ffi::c_void;
use std::os::fd::AsRawFd;

use super::dmabuf::DmabufBuffer;

const GL_TEXTURE_EXTERNAL_OES: u32 = 0x8D65;

// EGL_EXT_image_dma_buf_import(_modifiers) - not core EGL, not wrapped by
// the `khronos-egl` crate, so the numeric values are hardcoded here (stable
// values from the Khronos EGL registry)
const EGL_LINUX_DMA_BUF_EXT: egl::Enum = 0x3270;
const EGL_LINUX_DRM_FOURCC_EXT: i32 = 0x3271;
const EGL_DMA_BUF_PLANE_FD_EXT: [i32; 3] = [0x3272, 0x3275, 0x3278];
const EGL_DMA_BUF_PLANE_OFFSET_EXT: [i32; 3] = [0x3273, 0x3276, 0x3279];
const EGL_DMA_BUF_PLANE_PITCH_EXT: [i32; 3] = [0x3274, 0x3277, 0x327A];
const EGL_DMA_BUF_PLANE_MODIFIER_LO_EXT: [i32; 3] = [0x3443, 0x3445, 0x3447];
const EGL_DMA_BUF_PLANE_MODIFIER_HI_EXT: [i32; 3] = [0x3444, 0x3446, 0x3448];
const EGL_PLATFORM_GBM_KHR: egl::Enum = 0x31D7;

type EglCreateImageKhrFn = unsafe extern "C" fn(
    egl::EGLDisplay,
    egl::EGLContext,
    egl::Enum,
    egl::EGLClientBuffer,
    *const i32,
) -> egl::EGLImage;
type EglDestroyImageKhrFn = unsafe extern "C" fn(egl::EGLDisplay, egl::EGLImage) -> u32;
type GlImageTargetTexture2dOesFn = unsafe extern "C" fn(u32, egl::EGLImage);

const VERTEX_SHADER: &str = "#version 300 es
layout(location = 0) in vec2 a_pos;
layout(location = 1) in vec2 a_uv;
out vec2 v_uv;
void main() {
    v_uv = a_uv;
    gl_Position = vec4(a_pos, 0.0, 1.0);
}
";

const FRAGMENT_SHADER: &str = "#version 300 es
#extension GL_OES_EGL_image_external_essl3 : require
precision mediump float;
uniform samplerExternalOES u_tex;
in vec2 v_uv;
out vec4 o_color;
void main() {
    o_color = texture(u_tex, v_uv);
}
";

// Fullscreen quad: interleaved (x, y, u, v). UV orientation is chosen so
// that, combined with `glReadPixels`' bottom-up row order, the final
// readback buffer ends up top-down again (matching the shm path)
#[rustfmt::skip]
const QUAD: [f32; 16] = [
    -1.0, -1.0, 0.0, 0.0,
     1.0, -1.0, 1.0, 0.0,
    -1.0,  1.0, 0.0, 1.0,
     1.0,  1.0, 1.0, 1.0,
];

struct Dest {
    width: u32,
    height: u32,
    fbo: glow::Framebuffer,
    tex: glow::Texture,
}

pub struct GpuDownscaler {
    egl: egl::DynamicInstance<egl::EGL1_5>,
    display: egl::Display,
    gl: glow::Context,
    create_image_khr: EglCreateImageKhrFn,
    destroy_image_khr: EglDestroyImageKhrFn,
    image_target_texture_2d_oes: GlImageTargetTexture2dOesFn,
    program: glow::Program,
    vao: glow::VertexArray,
    _vbo: glow::Buffer,
    dest: Option<Dest>,
}

impl GpuDownscaler {
    /// Creates an isolated GLES context on the given GBM device. Returns
    /// `None` (never panics) on any failure; callers keep using the CPU
    /// mmap path in that case.
    pub fn new(gbm: &gbm::Device<std::fs::File>) -> Option<Self> {
        let egl = unsafe { egl::DynamicInstance::<egl::EGL1_5>::load_required() }
            .inspect_err(|err| log::warn!("failed to load libEGL: {err}"))
            .ok()?;

        let display = unsafe {
            egl.get_platform_display(
                EGL_PLATFORM_GBM_KHR,
                gbm.as_raw() as *mut c_void,
                &[egl::ATTRIB_NONE],
            )
        }
        .inspect_err(|err| log::warn!("eglGetPlatformDisplay(GBM) failed: {err}"))
        .ok()?;
        egl.initialize(display)
            .inspect_err(|err| log::warn!("eglInitialize failed: {err}"))
            .ok()?;
        egl.bind_api(egl::OPENGL_ES_API)
            .inspect_err(|err| log::warn!("eglBindAPI(GLES) failed: {err}"))
            .ok()?;

        let config_attribs = [egl::RENDERABLE_TYPE, egl::OPENGL_ES3_BIT, egl::NONE];
        let config = egl
            .choose_first_config(display, &config_attribs)
            .ok()
            .flatten()
            .or_else(|| {
                log::warn!("no suitable EGL config for GBM display");
                None
            })?;

        let context_attribs = [
            egl::CONTEXT_MAJOR_VERSION,
            3,
            egl::CONTEXT_MINOR_VERSION,
            0,
            egl::NONE,
        ];
        let context = egl
            .create_context(display, config, None, &context_attribs)
            .inspect_err(|err| log::warn!("eglCreateContext failed: {err}"))
            .ok()?;

        // No window/pbuffer needed - only FBOs are rendered to (relies on
        // `EGL_KHR_surfaceless_context`, which Mesa has supported for years)
        egl.make_current(display, None, None, Some(context))
            .inspect_err(|err| log::warn!("eglMakeCurrent (surfaceless) failed: {err}"))
            .ok()?;

        let create_image_khr = unsafe { load_proc::<EglCreateImageKhrFn>(&egl, "eglCreateImageKHR") }?;
        let destroy_image_khr =
            unsafe { load_proc::<EglDestroyImageKhrFn>(&egl, "eglDestroyImageKHR") }?;
        let image_target_texture_2d_oes = unsafe {
            load_proc::<GlImageTargetTexture2dOesFn>(&egl, "glEGLImageTargetTexture2DOES")
        }?;

        let gl = unsafe {
            glow::Context::from_loader_function(|name| {
                egl.get_proc_address(name)
                    .map_or(std::ptr::null(), |f| f as *const c_void)
            })
        };

        let (program, vao, vbo) = unsafe { build_pipeline(&gl) }?;

        Some(Self {
            egl,
            display,
            gl,
            create_image_khr,
            destroy_image_khr,
            image_target_texture_2d_oes,
            program,
            vao,
            _vbo: vbo,
            dest: None,
        })
    }

    /// Import `buf`'s dmabuf as a GL texture (once; cheap to resample after)
    /// and downscale it into a `target`-wide RGBA buffer, tightly packed.
    /// Must be called from the thread `new()` was created on - the context
    /// is made current once and left bound there for the thread's lifetime.
    pub fn downscale(&mut self, buf: &DmabufBuffer, target: u32) -> Option<(u32, u32, Vec<u8>)> {
        use glow::HasContext;

        let (w, h) = buf.size;
        let target = target.max(64).min(w.max(1));
        let scale = w as f32 / target as f32;
        let sw = target;
        let sh = ((h as f32 / scale).round() as u32).max(1);

        let image = unsafe { self.import_image(buf) }?;
        let tex = unsafe {
            let tex = self.gl.create_texture().ok()?;
            self.gl.bind_texture(GL_TEXTURE_EXTERNAL_OES, Some(tex));
            (self.image_target_texture_2d_oes)(GL_TEXTURE_EXTERNAL_OES, image);
            self.gl
                .tex_parameter_i32(GL_TEXTURE_EXTERNAL_OES, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            self.gl
                .tex_parameter_i32(GL_TEXTURE_EXTERNAL_OES, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            self.gl
                .tex_parameter_i32(GL_TEXTURE_EXTERNAL_OES, glow::TEXTURE_WRAP_S, glow::CLAMP_TO_EDGE as i32);
            self.gl
                .tex_parameter_i32(GL_TEXTURE_EXTERNAL_OES, glow::TEXTURE_WRAP_T, glow::CLAMP_TO_EDGE as i32);
            tex
        };

        let result = unsafe { self.render_and_read(tex, sw, sh) };

        unsafe {
            self.gl.delete_texture(tex);
            (self.destroy_image_khr)(self.display.as_ptr(), image);
        }

        result.map(|pixels| (sw, sh, pixels))
    }

    unsafe fn import_image(&self, buf: &DmabufBuffer) -> Option<egl::EGLImage> {
        let bo = buf.bo();
        let plane_count = bo.plane_count() as usize;
        if plane_count == 0 || plane_count > 3 {
            log::warn!("unsupported dmabuf plane count {plane_count}");
            return None;
        }
        let modifier: u64 = bo.modifier().into();
        let mod_lo = (modifier & 0xffff_ffff) as i32;
        let mod_hi = (modifier >> 32) as i32;

        let mut attribs = vec![
            egl::WIDTH,
            bo.width() as i32,
            egl::HEIGHT,
            bo.height() as i32,
            EGL_LINUX_DRM_FOURCC_EXT,
            bo.format() as u32 as i32,
        ];
        let mut fds = Vec::with_capacity(plane_count);
        for plane in 0..plane_count {
            let fd = bo.fd_for_plane(plane as i32).ok()?;
            attribs.extend_from_slice(&[
                EGL_DMA_BUF_PLANE_FD_EXT[plane],
                fd.as_raw_fd(),
                EGL_DMA_BUF_PLANE_OFFSET_EXT[plane],
                bo.offset(plane as i32) as i32,
                EGL_DMA_BUF_PLANE_PITCH_EXT[plane],
                bo.stride_for_plane(plane as i32) as i32,
                EGL_DMA_BUF_PLANE_MODIFIER_LO_EXT[plane],
                mod_lo,
                EGL_DMA_BUF_PLANE_MODIFIER_HI_EXT[plane],
                mod_hi,
            ]);
            fds.push(fd); // keep alive until eglCreateImageKHR returns
        }
        attribs.push(egl::NONE);

        let image = unsafe {
            (self.create_image_khr)(
                self.display.as_ptr(),
                egl::NO_CONTEXT,
                EGL_LINUX_DMA_BUF_EXT,
                std::ptr::null_mut(),
                attribs.as_ptr(),
            )
        };
        if image.is_null() {
            log::warn!("eglCreateImageKHR failed: {:?}", self.egl.get_error());
            return None;
        }
        Some(image)
    }

    unsafe fn render_and_read(&mut self, tex: glow::Texture, sw: u32, sh: u32) -> Option<Vec<u8>> {
        use glow::HasContext;
        let gl = &self.gl;

        unsafe {
        if self.dest.as_ref().is_none_or(|d| d.width != sw || d.height != sh) {
            if let Some(old) = self.dest.take() {
                gl.delete_texture(old.tex);
                gl.delete_framebuffer(old.fbo);
            }
            let dtex = gl.create_texture().ok()?;
            gl.bind_texture(glow::TEXTURE_2D, Some(dtex));
            gl.tex_image_2d(
                glow::TEXTURE_2D,
                0,
                glow::RGBA8 as i32,
                sw as i32,
                sh as i32,
                0,
                glow::RGBA,
                glow::UNSIGNED_BYTE,
                glow::PixelUnpackData::Slice(None),
            );
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MIN_FILTER, glow::LINEAR as i32);
            gl.tex_parameter_i32(glow::TEXTURE_2D, glow::TEXTURE_MAG_FILTER, glow::LINEAR as i32);
            let fbo = gl.create_framebuffer().ok()?;
            gl.bind_framebuffer(glow::FRAMEBUFFER, Some(fbo));
            gl.framebuffer_texture_2d(glow::FRAMEBUFFER, glow::COLOR_ATTACHMENT0, glow::TEXTURE_2D, Some(dtex), 0);
            if gl.check_framebuffer_status(glow::FRAMEBUFFER) != glow::FRAMEBUFFER_COMPLETE {
                log::warn!("dest framebuffer incomplete");
                return None;
            }
            self.dest = Some(Dest { width: sw, height: sh, fbo, tex: dtex });
        }
        let dest = self.dest.as_ref().unwrap();

        gl.bind_framebuffer(glow::FRAMEBUFFER, Some(dest.fbo));
        gl.viewport(0, 0, sw as i32, sh as i32);
        gl.use_program(Some(self.program));
        gl.active_texture(glow::TEXTURE0);
        gl.bind_texture(GL_TEXTURE_EXTERNAL_OES, Some(tex));
        let loc = gl.get_uniform_location(self.program, "u_tex");
        gl.uniform_1_i32(loc.as_ref(), 0);
        gl.bind_vertex_array(Some(self.vao));
        gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);

        gl.pixel_store_i32(glow::PACK_ALIGNMENT, 4);
        let mut pixels = vec![0u8; (sw * sh * 4) as usize];
        gl.read_pixels(
            0,
            0,
            sw as i32,
            sh as i32,
            glow::RGBA,
            glow::UNSIGNED_BYTE,
            glow::PixelPackData::Slice(Some(&mut pixels)),
        );
        let err = gl.get_error();
        if err != glow::NO_ERROR {
            log::warn!("GL error during downscale: {err:#x}");
            return None;
        }
        Some(pixels)
        }
    }
}

unsafe fn load_proc<F: Copy>(egl: &egl::DynamicInstance<egl::EGL1_5>, name: &str) -> Option<F> {
    let ptr = egl.get_proc_address(name)?;
    // SAFETY: `F` must be the correct `extern "C"` function pointer type for
    // `name`; enforced by callers only invoking this for the fixed set of
    // extension functions declared above.
    Some(unsafe { std::mem::transmute_copy::<*const (), F>(&(ptr as *const ())) })
}

unsafe fn build_pipeline(gl: &glow::Context) -> Option<(glow::Program, glow::VertexArray, glow::Buffer)> {
    use glow::HasContext;
    unsafe {

    let compile = |ty: u32, src: &str| -> Option<glow::Shader> {
        let shader = gl.create_shader(ty).ok()?;
        gl.shader_source(shader, src);
        gl.compile_shader(shader);
        if !gl.get_shader_compile_status(shader) {
            log::warn!("shader compile failed: {}", gl.get_shader_info_log(shader));
            return None;
        }
        Some(shader)
    };
    let vs = compile(glow::VERTEX_SHADER, VERTEX_SHADER)?;
    let fs = compile(glow::FRAGMENT_SHADER, FRAGMENT_SHADER)?;

    let program = gl.create_program().ok()?;
    gl.attach_shader(program, vs);
    gl.attach_shader(program, fs);
    gl.link_program(program);
    if !gl.get_program_link_status(program) {
        log::warn!("program link failed: {}", gl.get_program_info_log(program));
        return None;
    }
    gl.delete_shader(vs);
    gl.delete_shader(fs);

    let vao = gl.create_vertex_array().ok()?;
    gl.bind_vertex_array(Some(vao));
    let vbo = gl.create_buffer().ok()?;
    gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
    gl.buffer_data_u8_slice(glow::ARRAY_BUFFER, bytemuck_cast(&QUAD), glow::STATIC_DRAW);
    gl.enable_vertex_attrib_array(0);
    gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 16, 0);
    gl.enable_vertex_attrib_array(1);
    gl.vertex_attrib_pointer_f32(1, 2, glow::FLOAT, false, 16, 8);

    Some((program, vao, vbo))
    }
}

fn bytemuck_cast(data: &[f32]) -> &[u8] {
    // SAFETY: `f32` has no padding/alignment issues that matter for a
    // read-only reinterpret into bytes for `glBufferData`
    unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), std::mem::size_of_val(data)) }
}
