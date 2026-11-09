//! GLSL aurora background, drawn through an egui_glow paint callback.
//!
//! It is the "wallpaper" behind the floating panels: slow drifting blobs in the
//! theme's accent colors, film grain against banding, and a vignette.

use std::sync::Arc;

use eframe::egui::{self, Color32, Rect};
use eframe::egui_glow;
use eframe::glow::{self, HasContext as _};

pub struct Aurora {
    program: glow::Program,
    vao: glow::VertexArray,
}

#[derive(Clone, Copy)]
pub struct AuroraParams {
    pub time: f32,
    pub base: Color32,
    pub c1: Color32,
    pub c2: Color32,
    pub c3: Color32,
    /// Overall alpha: < 1 lets the acrylic window blur show through.
    pub alpha: f32,
    pub intensity: f32,
}

const VERTEX: &str = r#"
const vec2 verts[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
void main() {
    gl_Position = vec4(verts[gl_VertexID], 0.0, 1.0);
}
"#;

const FRAGMENT: &str = r#"
uniform vec2 u_origin;
uniform vec2 u_size;
uniform float u_time;
uniform vec3 u_base;
uniform vec3 u_c1;
uniform vec3 u_c2;
uniform vec3 u_c3;
uniform float u_alpha;
uniform float u_intensity;

float hash(vec2 p) {
    p = fract(p * vec2(123.34, 456.21));
    p += dot(p, p + 45.32);
    return fract(p.x * p.y);
}

float blob(vec2 uv, vec2 c, float r) {
    float d = length(uv - c);
    return exp(-(d * d) / (r * r));
}

void main() {
    vec2 frag = gl_FragCoord.xy - u_origin;
    vec2 uv = frag / u_size;
    float aspect = u_size.x / max(u_size.y, 1.0);
    vec2 p = vec2(uv.x * aspect, uv.y);
    float t = u_time * 0.045;

    vec2 a = vec2(aspect * (0.22 + 0.16 * sin(t * 1.7)), 0.78 + 0.10 * cos(t * 1.3));
    vec2 b = vec2(aspect * (0.80 + 0.12 * cos(t * 1.1)), 0.30 + 0.14 * sin(t * 1.9));
    vec2 c = vec2(aspect * (0.55 + 0.20 * sin(t * 0.8 + 2.0)), 0.95 + 0.08 * sin(t * 1.5));

    vec3 col = u_base;
    col = mix(col, u_c1, blob(p, a, 0.55) * 0.55 * u_intensity);
    col = mix(col, u_c2, blob(p, b, 0.50) * 0.45 * u_intensity);
    col = mix(col, u_c3, blob(p, c, 0.40) * 0.35 * u_intensity);

    // Soft diagonal sheen.
    float sheen = smoothstep(0.0, 1.0, 1.0 - abs(uv.x + uv.y - 1.0 - 0.25 * sin(t)));
    col += u_c1 * sheen * 0.025 * u_intensity;

    // Vignette.
    vec2 q = uv - 0.5;
    col *= 1.0 - dot(q, q) * 0.55;

    // Grain against banding.
    col += (hash(frag + fract(u_time)) - 0.5) / 255.0 * 2.0;

    vec4 outc = vec4(col * u_alpha, u_alpha);
    OUT_COLOR = outc;
}
"#;

fn rgb(c: Color32) -> [f32; 3] {
    [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0]
}

impl Aurora {
    pub fn new(gl: &glow::Context) -> Result<Self, String> {
        let version = egui_glow::ShaderVersion::get(gl);
        // gl_VertexID and `out` variables need GLSL 1.40 / ES 3.0. Older drivers just
        // get the flat background instead.
        if !version.is_new_shader_interface() {
            return Err(format!("{version:?} is too old for the aurora shader"));
        }
        let decl = version.version_declaration();
        let vs_src = format!("{decl}\n{VERTEX}");
        let fs_src = format!(
            "{decl}\nprecision highp float;\nout vec4 haunt_out;\n#define OUT_COLOR haunt_out\n{FRAGMENT}"
        );

        unsafe {
            let program = gl.create_program()?;
            let mut shaders = Vec::new();
            for (kind, src) in [(glow::VERTEX_SHADER, vs_src), (glow::FRAGMENT_SHADER, fs_src)] {
                let shader = gl.create_shader(kind)?;
                gl.shader_source(shader, &src);
                gl.compile_shader(shader);
                if !gl.get_shader_compile_status(shader) {
                    let log = gl.get_shader_info_log(shader);
                    gl.delete_shader(shader);
                    gl.delete_program(program);
                    return Err(format!("aurora shader failed to compile: {log}"));
                }
                gl.attach_shader(program, shader);
                shaders.push(shader);
            }
            gl.link_program(program);
            let ok = gl.get_program_link_status(program);
            for s in shaders {
                gl.detach_shader(program, s);
                gl.delete_shader(s);
            }
            if !ok {
                let log = gl.get_program_info_log(program);
                gl.delete_program(program);
                return Err(format!("aurora shader failed to link: {log}"));
            }
            let vao = gl.create_vertex_array()?;
            Ok(Self { program, vao })
        }
    }

    fn paint(&self, gl: &glow::Context, info: &egui::PaintCallbackInfo, p: &AuroraParams) {
        let vp = info.viewport_in_pixels();
        unsafe {
            gl.use_program(Some(self.program));
            let loc = |n: &str| gl.get_uniform_location(self.program, n);
            gl.uniform_2_f32(loc("u_origin").as_ref(), vp.left_px as f32, vp.from_bottom_px as f32);
            gl.uniform_2_f32(loc("u_size").as_ref(), vp.width_px as f32, vp.height_px as f32);
            gl.uniform_1_f32(loc("u_time").as_ref(), p.time);
            gl.uniform_3_f32_slice(loc("u_base").as_ref(), &rgb(p.base));
            gl.uniform_3_f32_slice(loc("u_c1").as_ref(), &rgb(p.c1));
            gl.uniform_3_f32_slice(loc("u_c2").as_ref(), &rgb(p.c2));
            gl.uniform_3_f32_slice(loc("u_c3").as_ref(), &rgb(p.c3));
            gl.uniform_1_f32(loc("u_alpha").as_ref(), p.alpha);
            gl.uniform_1_f32(loc("u_intensity").as_ref(), p.intensity);
            gl.bind_vertex_array(Some(self.vao));
            gl.draw_arrays(glow::TRIANGLES, 0, 3);
        }
    }

    pub fn destroy(&self, gl: &glow::Context) {
        unsafe {
            gl.delete_program(self.program);
            gl.delete_vertex_array(self.vao);
        }
    }
}

/// Queues the aurora to be drawn over `rect` in the current layer.
pub fn paint(painter: &egui::Painter, rect: Rect, aurora: &Arc<Aurora>, params: AuroraParams) {
    let aurora = aurora.clone();
    let cb = egui_glow::CallbackFn::new(move |info, glow_painter| {
        aurora.paint(glow_painter.gl(), &info, &params);
    });
    painter.add(egui::PaintCallback { rect, callback: Arc::new(cb) });
}
