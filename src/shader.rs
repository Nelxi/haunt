//! GLSL background, drawn through an egui_glow paint callback.
//!
//! The "night" behind the floating panels, after the Ghostly app and site: a deep sky with a
//! violet breath at the horizon, moonlight, twinkling stars, slow fog and three depths of
//! falling snow, plus a vignette and grain against banding. Colours come from the theme.

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
    /// 0 = no snow, 1 = full snowfall.
    pub snow: f32,
    pub snow_time: f32,
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
uniform float u_snow;
uniform float u_snow_time;

float hash(vec2 p) {
    p = fract(p * vec2(123.34, 456.21));
    p += dot(p, p + 45.32);
    return fract(p.x * p.y);
}

float noise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    vec2 u = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), u.x),
               mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), u.x), u.y);
}

float fbm(vec2 p) {
    float v = 0.0;
    float a = 0.5;
    for (int i = 0; i < 5; i++) {
        v += a * noise(p);
        p = p * 2.03 + vec2(17.0, 9.0);
        a *= 0.5;
    }
    return v;
}

void main() {
    vec2 frag = gl_FragCoord.xy - u_origin;
    vec2 uv = frag / u_size;              // y goes up: 0 = bottom, 1 = top
    float aspect = u_size.x / max(u_size.y, 1.0);
    vec2 p = vec2(uv.x * aspect, uv.y);
    float t = u_time;
    float k = u_intensity;

    // Sky: deep ink at the top, a violet breath towards the horizon.
    vec3 col = mix(mix(u_base, u_c2, 0.14 * k), u_base * 0.8, smoothstep(0.0, 0.9, uv.y));
    float horizon = exp(-pow((uv.y - 0.0) / 0.32, 2.0));
    col += u_c1 * horizon * 0.09 * k;

    // Moonlight from the upper right.
    vec2 moon = vec2(aspect * 0.84, 0.92);
    float md = length(p - moon);
    col += mix(u_c1, vec3(1.0), 0.3) * 0.05 * exp(-md * md / 0.08) * k;

    // Stars: sparse, twinkling, fading out near the fog.
    vec2 cell = floor(frag / 44.0);
    vec2 cf = fract(frag / 44.0);
    float h = hash(cell);
    if (h > 0.84) {
        vec2 sp = vec2(0.15 + 0.7 * hash(cell + 3.1), 0.15 + 0.7 * hash(cell + 7.7));
        float d = length(cf - sp) * 44.0;
        float tw = 0.55 + 0.45 * sin(t * (0.5 + h * 1.8) + h * 40.0);
        float size = 0.6 + 0.9 * hash(cell + 1.7);
        float s = smoothstep(size + 1.0, 0.0, d) * tw * smoothstep(0.2, 0.65, uv.y);
        col += vec3(0.86, 0.83, 1.0) * s * 0.5 * k;
    }

    // Fog: two slow layers rolling along the bottom.
    float f1 = fbm(vec2(p.x * 1.3 + t * 0.010, p.y * 2.6 - t * 0.003));
    float f2 = fbm(vec2(p.x * 2.4 - t * 0.016, p.y * 4.5 + 3.0));
    float fog = smoothstep(0.42, 0.95, f1) * (1.0 - smoothstep(0.0, 0.65, uv.y))
              + smoothstep(0.55, 1.0, f2) * 0.55 * (1.0 - smoothstep(0.05, 0.45, uv.y));
    vec3 fogc = u_base + mix(u_c2, u_c1, 0.45) * 0.5;
    col = mix(col, fogc, clamp(fog * 0.24 * k, 0.0, 1.0));

    // Snow: three depths, the near flakes bigger and faster. Winter only, on its own clock
    // so it keeps falling even when the sky itself is static.
    float st = u_snow_time;
    for (int i = 0; i < 3 && u_snow > 0.0; i++) {
        float fi = float(i);
        float sz = 74.0 - fi * 20.0;
        float speed = 10.0 + fi * 8.0;
        vec2 q = frag + vec2(sin(st * 0.25 + fi * 1.7) * 14.0 + st * (2.0 + fi * 1.5), st * speed);
        vec2 c = floor(q / sz);
        vec2 ff = fract(q / sz);
        float hh = hash(c + fi * 13.0);
        if (hh > 0.70) {
            vec2 sp = vec2(0.2 + 0.6 * hash(c + 1.3 + fi), 0.2 + 0.6 * hash(c + 5.1 + fi));
            float d = length(ff - sp) * sz;
            float r = 0.7 + fi * 0.45;
            col += vec3(0.9, 0.88, 1.0) * smoothstep(r + 1.1, r - 0.3, d) * (0.09 + 0.06 * fi) * k * u_snow;
        }
    }

    // Vignette and grain (against banding in the dark gradients).
    vec2 v = uv - 0.5;
    col *= 1.0 - dot(v, v) * 0.6;
    col += (hash(frag + fract(t)) - 0.5) / 255.0 * 2.0;

    OUT_COLOR = vec4(col * u_alpha, u_alpha);
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
            gl.uniform_1_f32(loc("u_snow").as_ref(), p.snow);
            gl.uniform_1_f32(loc("u_snow_time").as_ref(), p.snow_time);
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
