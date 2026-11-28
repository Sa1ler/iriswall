use crate::palette::{self, Palette};
use std::ffi::{c_void, CString};
use std::path::{Path, PathBuf};

// ───────────────────────── Загрузка картинки (в отдельном потоке) ─────────────────────────

pub struct Image {
    pub path: PathBuf,
    pub w: u32,
    pub h: u32,
    /// RGB8 без альфы: шейдер использует только .rgb, лишний канал стоил бы +33% памяти
    pub rgb: Vec<u8>,
    pub palette: Palette,
}

/// Декодирует картинку (с учётом EXIF-ориентации) и заранее уменьшает её до размера,
/// которого хватает для заполнения самого большого монитора. Всё делается в RGB8,
/// без промежуточной RGBA-копии: пик памяти при декодировании 4K/8K заметно ниже.
pub fn decode(path: &Path, max_w: u32, max_h: u32) -> Result<Image, String> {
    use image::{DynamicImage, ImageDecoder, ImageReader};

    let fail = |e: &dyn std::fmt::Display| format!("{}: {e}", path.display());
    let reader = ImageReader::open(path)
        .and_then(|r| r.with_guessed_format())
        .map_err(|e| fail(&e))?;
    let mut dec = reader.into_decoder().map_err(|e| fail(&e))?;
    let orient = dec
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(dec).map_err(|e| fail(&e))?;
    img.apply_orientation(orient);

    let mut rgb = img.into_rgb8();
    let (iw, ih) = rgb.dimensions();
    let s = (max_w as f32 / iw as f32).max(max_h as f32 / ih as f32);
    if s < 1.0 {
        let nw = ((iw as f32 * s).ceil() as u32).max(1);
        let nh = ((ih as f32 * s).ceil() as u32).max(1);
        // исходник освобождается сразу после присваивания
        rgb = image::imageops::resize(&rgb, nw, nh, image::imageops::FilterType::Lanczos3);
    }
    let (w, h) = rgb.dimensions();
    let palette = palette::extract(rgb.as_raw(), w, h);
    Ok(Image {
        path: path.to_path_buf(),
        w,
        h,
        rgb: rgb.into_raw(),
        palette,
    })
}

// ───────────────────────── GL ─────────────────────────

pub struct Tex {
    id: u32,
    pub w: u32,
    pub h: u32,
}

pub struct DrawParams {
    pub width: u32,
    pub height: u32,
    /// 0..1 после easing: радиус круга
    pub progress: f32,
    pub fit: bool,
    pub softness: f32,
    pub zoom: f32,
    pub bg: [f32; 3],
    pub effect: i32,
    pub origin: [f32; 2],
    pub angle: f32,
    pub seed: f32,
}

struct Locs {
    old: i32,
    new: i32,
    surf: i32,
    old_size: i32,
    new_size: i32,
    fit: i32,
    has_old: i32,
    progress: i32,
    soft: i32,
    zoom: i32,
    bg: i32,
    effect: i32,
    origin: i32,
    angle: i32,
    seed: i32,
}

pub struct Renderer {
    prog: u32,
    vao: u32,
    _vbo: u32,
    loc: Locs,
}

const VERT: &str = r##"#version 300 es
layout(location = 0) in vec2 pos;
out vec2 v_uv;
void main() {
    v_uv = vec2(pos.x * 0.5 + 0.5, 0.5 - pos.y * 0.5);
    gl_Position = vec4(pos, 0.0, 1.0);
}
"##;

const FRAG: &str = r##"#version 300 es
precision highp float;
in vec2 v_uv;
out vec4 frag;

uniform sampler2D u_old;
uniform sampler2D u_new;
uniform vec2 u_surf;
uniform vec2 u_old_size;
uniform vec2 u_new_size;
uniform int u_fit;
uniform int u_has_old;
uniform int u_effect;     // 0 iris, 1 sweep, 2 ripple, 3 mosaic, 4 zoom, 5 ink
uniform vec2 u_origin;    // случайная точка старта (0..1)
uniform float u_angle;    // случайное направление (sweep)
uniform float u_seed;     // случайное зерно шума
uniform float u_progress; // после easing
uniform float u_soft;
uniform float u_zoom;
uniform vec3 u_bg;

// Цвет картинки в точке p (пиксели поверхности) с учётом fill/fit.
// В fill-режиме края растягиваются, поэтому сдвиги и зум не показывают поля.
vec3 pic(sampler2D t, vec2 isz, vec2 p) {
    float sc = (u_fit == 1) ? min(u_surf.x / isz.x, u_surf.y / isz.y)
                            : max(u_surf.x / isz.x, u_surf.y / isz.y);
    vec2 uv = ((p - 0.5 * u_surf) / sc + 0.5 * isz) / isz;
    vec3 c = texture(t, uv).rgb;
    if (u_fit == 1) {
        vec2 inside = step(vec2(0.0), uv) * step(uv, vec2(1.0));
        c = mix(u_bg, c, inside.x * inside.y);
    }
    return c;
}

vec3 newc(vec2 p) { return pic(u_new, u_new_size, p); }
vec3 oldc(vec2 p) { return (u_has_old == 1) ? pic(u_old, u_old_size, p) : u_bg; }

vec2 zoomAt(vec2 p, vec2 c, float s) { return c + (p - c) / s; }
float px() { return max(min(u_surf.x, u_surf.y) / 1080.0, 0.5); }

float hash21(vec2 p) {
    p = fract(p * vec2(123.34, 456.21));
    p += dot(p, p + 45.32);
    return fract(p.x * p.y);
}

float vnoise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    float a = hash21(i);
    float b = hash21(i + vec2(1.0, 0.0));
    float c = hash21(i + vec2(0.0, 1.0));
    float d = hash21(i + vec2(1.0, 1.0));
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

float fbm(vec2 p) {
    float v = 0.0;
    float a = 0.5;
    for (int i = 0; i < 4; i++) {
        v += a * vnoise(p);
        p = p * 2.03 + vec2(17.1, 9.2);
        a *= 0.5;
    }
    return v / 0.9375;
}

// 0. IRIS: круг из центра с мягким краем, новые обои «оседают»
vec3 fx_iris(vec2 p, vec2 c, float t) {
    vec3 n = newc(zoomAt(p, c, 1.0 + u_zoom * (1.0 - t)));
    vec3 o = oldc(p);
    float maxr = length(c);
    float soft = max(u_soft * maxr, 1.0);
    float r = t * (maxr + soft);
    float m = 1.0 - smoothstep(r - soft, r, length(p - c));
    return mix(o, n, m);
}

// 1. SWEEP: мягкая волна под случайным углом, обе картинки слегка едут (параллакс)
vec3 fx_sweep(vec2 p, vec2 c, float t) {
    vec2 d = vec2(cos(u_angle), sin(u_angle));
    float e = 0.5 * (abs(u_surf.x * d.x) + abs(u_surf.y * d.y));
    float q = (dot(p - c, d) + e) / (2.0 * e);
    float w = 0.30;
    float m = 1.0 - smoothstep(t * (1.0 + w) - w, t * (1.0 + w), q);
    vec2 sh = d * e * 0.14;
    vec3 n = newc(zoomAt(p + sh * (1.0 - t), c, 1.0 + 0.07 * (1.0 - t)));
    vec3 o = oldc(zoomAt(p - sh * t, c, 1.0 + 0.03 * t));
    return mix(o, n, m);
}

// 2. RIPPLE: круг от случайной точки с кольцом-рябью, преломляющим картинку
vec3 fx_ripple(vec2 p, vec2 c, float t) {
    vec2 org = u_origin * u_surf;
    float maxr = length(max(org, u_surf - org));
    float soft = max(u_soft * maxr, 1.0);
    float r = t * (maxr + soft);
    vec2 v = p - org;
    float d = length(v);
    vec2 dir = v / max(d, 1e-3);
    float x = (d - r) / (0.14 * maxr);
    float env = exp(-x * x);
    float ph = (d - r) * (6.2831853 / (70.0 * px()));
    vec2 pw = p + dir * sin(ph) * env * 16.0 * px() * (1.0 - t);
    vec3 n = newc(zoomAt(pw, c, 1.0 + u_zoom * (1.0 - t)));
    vec3 o = oldc(pw);
    float m = 1.0 - smoothstep(r - soft, r, d);
    vec3 col = mix(o, n, m);
    col *= 1.0 + 0.10 * env * cos(ph) * (1.0 - t);
    return col;
}

// 3. MOSAIC: плитки «проявляются» волной от случайной точки, каждая оседает с зумом
vec3 fx_mosaic(vec2 p, vec2 c, float t) {
    vec2 org = u_origin * u_surf;
    float maxr = length(max(org, u_surf - org));
    float cs = max(72.0 * px(), 24.0);
    vec2 id = floor(p / cs);
    vec2 cc = (id + 0.5) * cs;
    float th = length(cc - org) / maxr * 0.72 + hash21(id + u_seed) * 0.28;
    float cp = clamp((t - th * 0.72) / 0.28, 0.0, 1.0);
    float e = cp * cp * (3.0 - 2.0 * cp);
    vec3 n = newc(zoomAt(p, cc, 1.0 + 0.22 * (1.0 - e)));
    vec3 o = oldc(p);
    vec3 col = mix(o, n, e);
    // тонкий шов между плитками, виден только пока плитка переворачивается
    vec2 lp = abs(p - cc) / (cs * 0.5);
    float seam = smoothstep(0.90, 1.0, max(lp.x, lp.y));
    col *= 1.0 - 0.30 * seam * sin(3.14159265 * e);
    return col;
}

// 4. ZOOM: кроссфейд с радиальным размытием, как «пролёт» сквозь картинку
vec3 fx_zoom(vec2 p, vec2 c, float t) {
    float blur = 0.07 * sin(3.14159265 * t);
    float zn = 1.0 + 0.12 * (1.0 - t);
    float zo = 1.0 + 0.07 * t;
    vec3 n = vec3(0.0);
    vec3 o = vec3(0.0);
    for (int i = 0; i < 8; i++) {
        float k = float(i) / 7.0;
        vec2 q = c + (p - c) * (1.0 - blur * k);
        n += newc(zoomAt(q, c, zn));
        o += oldc(zoomAt(q, c, zo));
    }
    return mix(o / 8.0, n / 8.0, smoothstep(0.0, 1.0, t));
}

// 5. INK: органичное растекание по шуму от случайной точки, тёмная кромка как чернила
vec3 fx_ink(vec2 p, vec2 c, float t) {
    vec2 org = u_origin * u_surf;
    float maxr = length(max(org, u_surf - org));
    float dn = length(p - org) / maxr;
    float nz = fbm(p / (380.0 * px()) + vec2(u_seed, u_seed * 1.7));
    float field = clamp(0.5 * dn + 0.5 * nz, 0.0, 1.0);
    float soft = 0.22;
    float f = t * (1.0 + soft);
    float m = 1.0 - smoothstep(f - soft, f, field);
    float edge = smoothstep(f - soft, f - soft * 0.55, field)
               * (1.0 - smoothstep(f - soft * 0.55, f, field));
    vec3 n = newc(zoomAt(p, c, 1.0 + u_zoom * (1.0 - t)));
    vec3 o = oldc(p);
    return mix(o, n, m) * (1.0 - 0.22 * edge);
}

void main() {
    vec2 p = v_uv * u_surf;
    vec2 c = 0.5 * u_surf;
    float t = u_progress;

    if (t >= 1.0) {
        frag = vec4(newc(p), 1.0);
        return;
    }

    vec3 col;
    if (u_effect == 1) col = fx_sweep(p, c, t);
    else if (u_effect == 2) col = fx_ripple(p, c, t);
    else if (u_effect == 3) col = fx_mosaic(p, c, t);
    else if (u_effect == 4) col = fx_zoom(p, c, t);
    else if (u_effect == 5) col = fx_ink(p, c, t);
    else col = fx_iris(p, c, t);

    frag = vec4(col, 1.0);
}
"##;

unsafe fn compile(kind: u32, src: &str) -> Result<u32, String> {
    let sh = gl::CreateShader(kind);
    let c = CString::new(src).map_err(|e| e.to_string())?;
    gl::ShaderSource(sh, 1, &c.as_ptr(), std::ptr::null());
    gl::CompileShader(sh);
    let mut ok = 0;
    gl::GetShaderiv(sh, gl::COMPILE_STATUS, &mut ok);
    if ok == 0 {
        let mut len = 0;
        gl::GetShaderiv(sh, gl::INFO_LOG_LENGTH, &mut len);
        let mut buf = vec![0u8; len.max(1) as usize];
        gl::GetShaderInfoLog(sh, len, std::ptr::null_mut(), buf.as_mut_ptr() as *mut _);
        gl::DeleteShader(sh);
        return Err(String::from_utf8_lossy(&buf).trim_end_matches('\0').to_string());
    }
    Ok(sh)
}

unsafe fn uloc(prog: u32, name: &str) -> i32 {
    let c = CString::new(name).unwrap();
    gl::GetUniformLocation(prog, c.as_ptr())
}

impl Renderer {
    /// Вызывать при активном EGL-контексте.
    pub fn new(loader: &dyn Fn(&str) -> *const c_void) -> Result<Self, String> {
        gl::load_with(|s| loader(s));
        unsafe {
            let vs = compile(gl::VERTEX_SHADER, VERT)?;
            let fs = compile(gl::FRAGMENT_SHADER, FRAG)?;
            let prog = gl::CreateProgram();
            gl::AttachShader(prog, vs);
            gl::AttachShader(prog, fs);
            gl::LinkProgram(prog);
            let mut ok = 0;
            gl::GetProgramiv(prog, gl::LINK_STATUS, &mut ok);
            if ok == 0 {
                let mut len = 0;
                gl::GetProgramiv(prog, gl::INFO_LOG_LENGTH, &mut len);
                let mut buf = vec![0u8; len.max(1) as usize];
                gl::GetProgramInfoLog(prog, len, std::ptr::null_mut(), buf.as_mut_ptr() as *mut _);
                return Err(String::from_utf8_lossy(&buf).trim_end_matches('\0').to_string());
            }
            gl::DeleteShader(vs);
            gl::DeleteShader(fs);

            let verts: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];
            let (mut vao, mut vbo) = (0u32, 0u32);
            gl::GenVertexArrays(1, &mut vao);
            gl::BindVertexArray(vao);
            gl::GenBuffers(1, &mut vbo);
            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
            gl::BufferData(
                gl::ARRAY_BUFFER,
                std::mem::size_of_val(&verts) as isize,
                verts.as_ptr() as *const _,
                gl::STATIC_DRAW,
            );
            gl::EnableVertexAttribArray(0);
            gl::VertexAttribPointer(0, 2, gl::FLOAT, gl::FALSE, 0, std::ptr::null());

            let loc = Locs {
                old: uloc(prog, "u_old"),
                new: uloc(prog, "u_new"),
                surf: uloc(prog, "u_surf"),
                old_size: uloc(prog, "u_old_size"),
                new_size: uloc(prog, "u_new_size"),
                fit: uloc(prog, "u_fit"),
                has_old: uloc(prog, "u_has_old"),
                progress: uloc(prog, "u_progress"),
                soft: uloc(prog, "u_soft"),
                zoom: uloc(prog, "u_zoom"),
                bg: uloc(prog, "u_bg"),
                effect: uloc(prog, "u_effect"),
                origin: uloc(prog, "u_origin"),
                angle: uloc(prog, "u_angle"),
                seed: uloc(prog, "u_seed"),
            };
            Ok(Self {
                prog,
                vao,
                _vbo: vbo,
                loc,
            })
        }
    }

    /// `mips`: строить ли mip-уровни. Нужны только если картинка показывается с
    /// заметным уменьшением (монитор заметно меньше текстуры); иначе это +33% памяти впустую.
    pub fn upload(&self, img: &Image, mips: bool) -> Tex {
        unsafe {
            let mut id = 0u32;
            gl::GenTextures(1, &mut id);
            gl::BindTexture(gl::TEXTURE_2D, id);
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGB8 as i32,
                img.w as i32,
                img.h as i32,
                0,
                gl::RGB,
                gl::UNSIGNED_BYTE,
                img.rgb.as_ptr() as *const _,
            );
            let min = if mips { gl::LINEAR_MIPMAP_LINEAR } else { gl::LINEAR };
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, min as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
            if mips {
                gl::GenerateMipmap(gl::TEXTURE_2D);
            }
            Tex {
                id,
                w: img.w,
                h: img.h,
            }
        }
    }

    pub fn delete(&self, t: Tex) {
        unsafe {
            gl::DeleteTextures(1, &t.id);
        }
    }

    pub fn clear(&self, bg: [f32; 3], w: u32, h: u32) {
        unsafe {
            gl::Viewport(0, 0, w as i32, h as i32);
            gl::ClearColor(bg[0], bg[1], bg[2], 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }
    }

    pub fn draw(&self, p: &DrawParams, old: Option<&Tex>, new: &Tex) {
        let o = old.unwrap_or(new);
        unsafe {
            gl::Viewport(0, 0, p.width as i32, p.height as i32);
            gl::UseProgram(self.prog);

            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, o.id);
            gl::ActiveTexture(gl::TEXTURE1);
            gl::BindTexture(gl::TEXTURE_2D, new.id);

            gl::Uniform1i(self.loc.old, 0);
            gl::Uniform1i(self.loc.new, 1);
            gl::Uniform2f(self.loc.surf, p.width as f32, p.height as f32);
            gl::Uniform2f(self.loc.old_size, o.w as f32, o.h as f32);
            gl::Uniform2f(self.loc.new_size, new.w as f32, new.h as f32);
            gl::Uniform1i(self.loc.fit, p.fit as i32);
            gl::Uniform1i(self.loc.has_old, old.is_some() as i32);
            gl::Uniform1f(self.loc.progress, p.progress);
            gl::Uniform1f(self.loc.soft, p.softness);
            gl::Uniform1f(self.loc.zoom, p.zoom);
            gl::Uniform3f(self.loc.bg, p.bg[0], p.bg[1], p.bg[2]);
            gl::Uniform1i(self.loc.effect, p.effect);
            gl::Uniform2f(self.loc.origin, p.origin[0], p.origin[1]);
            gl::Uniform1f(self.loc.angle, p.angle);
            gl::Uniform1f(self.loc.seed, p.seed);

            gl::BindVertexArray(self.vao);
            gl::DrawArrays(gl::TRIANGLE_STRIP, 0, 4);
        }
    }
}
