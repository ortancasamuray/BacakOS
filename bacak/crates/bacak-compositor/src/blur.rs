//! Two-pass separable Gaussian blur primitive (render-to-texture).
//!
//! This is the *foundation* for real glassmorphism backdrop blur. It
//! is intentionally self-contained: give it a source `GlesTexture` and
//! it returns a blurred copy. Wiring it into the per-window compositing
//! path is a separate, larger milestone (see "Integration plan" below)
//! — this module deliberately stops at the reusable, correct kernel so
//! nothing half-finished lands in the render pipeline.
//!
//! # How it works
//!
//! A 2-D Gaussian is *separable*: blurring horizontally then vertically
//! is equivalent to a single 2-D convolution but costs `2·N` taps
//! instead of `N²`. So:
//!
//! 1. Allocate two offscreen textures (`Offscreen::create_buffer`).
//! 2. Pass 1 — sample `src` along X into `tmp`, custom 9-tap kernel.
//! 3. Pass 2 — sample `tmp` along Y into `dst`.
//!
//! Each pass overrides the renderer's default texture shader with a
//! Gaussian one ([`GlesFrame::override_default_tex_program`]) and draws
//! the source full-screen via [`Frame::render_texture_from_to`]. The
//! `dir` uniform is the per-axis texel step scaled by the blur radius,
//! so a bigger radius simply spreads the taps wider.
//!
//! # Integration plan (not done here)
//!
//! Real backdrop blur for passive windows needs, per output per frame:
//!
//! 1. Render the scene that should appear *behind* translucent windows
//!    into an offscreen `GlesTexture` (a manual `GlesRenderer` pass,
//!    separate from `DrmCompositor::render_frame`).
//! 2. [`Blur::blur`] that texture.
//! 3. Build the final `DrmCompositor` element list so each passive
//!    window emits a `TextureRenderElement` of the blurred texture
//!    clipped to its rect, then the live surface on top at reduced
//!    alpha. Focused window unblurred; overlay on top.
//!
//! Step 1 (a second full render pass + FBO lifecycle interacting with
//! `DrmCompositor`) is the heavy part and is tracked as its own task.

#![cfg(feature = "runtime")]

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{
    GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName, UniformType,
};
use smithay::backend::renderer::{Bind, Color32F, Frame, Offscreen, Renderer};
use smithay::utils::{Buffer as BufferCoord, Physical, Rectangle, Size, Transform};

/// Separable Gaussian fragment shader. Modelled on Smithay's default
/// `texture.frag` so it honours the same `//_DEFINES_` substitution
/// and the `EXTERNAL` / `NO_ALPHA` / `DEBUG_FLAGS` variants — only
/// `main` differs, replacing the single fetch with a 9-tap kernel
/// along the `dir` uniform. Smithay prepends `#version 100`, so we
/// must not.
const BLUR_SHADER_SRC: &str = r#"
//_DEFINES_
#if defined(EXTERNAL)
#extension GL_OES_EGL_image_external : require
#endif

precision mediump float;
#if defined(EXTERNAL)
uniform samplerExternalOES tex;
#else
uniform sampler2D tex;
#endif

uniform float alpha;
varying vec2 v_coords;
uniform vec2 dir;

#if defined(DEBUG_FLAGS)
uniform float tint;
#endif

void main() {
    // Normalised 9-tap Gaussian (sums to 1.0).
    vec4 c = texture2D(tex, v_coords) * 0.227027;
    c += texture2D(tex, v_coords + dir * 1.0) * 0.194595;
    c += texture2D(tex, v_coords - dir * 1.0) * 0.194595;
    c += texture2D(tex, v_coords + dir * 2.0) * 0.121622;
    c += texture2D(tex, v_coords - dir * 2.0) * 0.121622;
    c += texture2D(tex, v_coords + dir * 3.0) * 0.054054;
    c += texture2D(tex, v_coords - dir * 3.0) * 0.054054;
    c += texture2D(tex, v_coords + dir * 4.0) * 0.016216;
    c += texture2D(tex, v_coords - dir * 4.0) * 0.016216;

#if defined(NO_ALPHA)
    gl_FragColor = vec4(c.rgb, 1.0) * alpha;
#else
    gl_FragColor = c * alpha;
#endif

#if defined(DEBUG_FLAGS)
    if (tint == 1.0) {
        gl_FragColor = vec4(0.0, 0.2, 0.0, 0.2) + gl_FragColor * 0.8;
    }
#endif
}
"#;

/// A compiled separable-Gaussian blur. Cheap to clone (the program is
/// `Arc`-backed); compile once and reuse for the renderer's lifetime.
#[derive(Clone)]
pub struct Blur {
    program: GlesTexProgram,
}

impl Blur {
    /// Compile the blur shader. `None` if the GLES driver rejects it —
    /// callers must treat blur as optional and fall back gracefully.
    pub fn new(renderer: &mut GlesRenderer) -> Option<Self> {
        let uniforms = [UniformName::new("dir", UniformType::_2f)];
        let program = renderer
            .compile_custom_texture_shader(BLUR_SHADER_SRC, &uniforms)
            .map_err(|e| tracing::warn!(?e, "blur shader compile failed"))
            .ok()?;
        Some(Self { program })
    }

    /// Blur `src` (which must be `size` pixels) and return a fresh
    /// texture with the result. `radius_px` is the effective Gaussian
    /// reach in source pixels; the taps are spread proportionally so a
    /// larger value is a stronger frost. `None` on any GL failure.
    pub fn blur(
        &self,
        renderer: &mut GlesRenderer,
        src: &GlesTexture,
        size: Size<i32, BufferCoord>,
        radius_px: f32,
    ) -> Option<GlesTexture> {
        let w = size.w.max(1) as f32;
        let h = size.h.max(1) as f32;
        // The kernel reaches ±4 taps, so a radius of `r` px wants each
        // tap step to be `r/4` px → in UV space that's `(r/4)/dim`.
        let step = (radius_px / 4.0).max(0.5);

        let mut tmp = renderer.create_buffer(Fourcc::Abgr8888, size).ok()?;
        let mut dst = renderer.create_buffer(Fourcc::Abgr8888, size).ok()?;

        // Pass 1: horizontal, src → tmp.
        self.pass(renderer, src, &mut tmp, size, [step / w, 0.0])?;
        // Pass 2: vertical, tmp → dst.
        self.pass(renderer, &tmp, &mut dst, size, [0.0, step / h])?;
        Some(dst)
    }

    /// One directional pass: draw `src` full-frame into `target`
    /// through the Gaussian program with the given `dir` (UV-space
    /// texel step along the blur axis).
    fn pass(
        &self,
        renderer: &mut GlesRenderer,
        src: &GlesTexture,
        target: &mut GlesTexture,
        size: Size<i32, BufferCoord>,
        dir: [f32; 2],
    ) -> Option<()> {
        let phys = Size::<i32, Physical>::from((size.w, size.h));
        let full = Rectangle::<i32, Physical>::from_size(phys);
        let src_rect =
            Rectangle::<f64, BufferCoord>::from_size(Size::from((size.w as f64, size.h as f64)));

        let mut fb = renderer.bind(target).ok()?;
        let mut frame = renderer.render(&mut fb, phys, Transform::Normal).ok()?;
        frame.clear(Color32F::TRANSPARENT, &[full]).ok()?;
        // GlesFrame's inherent method takes the program + uniforms
        // directly (no `override_default_tex_program` dance needed).
        frame
            .render_texture_from_to(
                src,
                src_rect,
                full,
                &[full],
                &[],
                Transform::Normal,
                1.0,
                Some(&self.program),
                &[Uniform::new("dir", dir)],
            )
            .ok()?;
        // The SyncPoint is fine to drop — the next pass binds a fresh
        // framebuffer and the final consumer fences as needed.
        let _sync = frame.finish().ok()?;
        Some(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// The blur shader has to keep Smithay's texture-program contract:
    /// the `tex` sampler, `v_coords` varying, the `//_DEFINES_` hook,
    /// and our `dir` uniform. A cheap guard against accidentally
    /// editing the shader into something that won't link (we can't
    /// compile it without a GL context in unit tests).
    #[test]
    fn shader_keeps_texture_program_contract() {
        for needle in [
            "//_DEFINES_",
            "uniform sampler2D tex;",
            "varying vec2 v_coords;",
            "uniform vec2 dir;",
            "gl_FragColor",
        ] {
            assert!(
                BLUR_SHADER_SRC.contains(needle),
                "blur shader is missing `{needle}`"
            );
        }
        // Smithay prepends `#version 100`; ours must not double it.
        assert!(!BLUR_SHADER_SRC.contains("#version"));
    }

    #[test]
    fn gaussian_weights_sum_to_one() {
        // Keep the kernel normalised, else the blur darkens/brightens.
        let w = [
            0.227027, 0.194595, 0.194595, 0.121622, 0.121622, 0.054054,
            0.054054, 0.016216, 0.016216,
        ];
        let sum: f64 = w.iter().sum();
        assert!((sum - 1.0).abs() < 1e-3, "weights sum to {sum}, expected 1.0");
    }
}
