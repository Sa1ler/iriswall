//! Палитра обоев: доминирующие цвета, главный акцентный, фон и текст.
//!
//! После каждой смены обоев демон пишет `~/.cache/iriswall/palette.json` и (если задан `hook`)
//! запускает пользовательскую команду. Так можно красить Quickshell, Hyprland, терминал
//! и что угодно ещё под текущие обои.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Debug)]
pub struct Palette {
    /// доминирующие цвета, самые заметные первыми (до 6)
    pub colors: Vec<[u8; 3]>,
    /// самый «живой» цвет: подходит для акцента
    pub primary: [u8; 3],
    /// самый тёмный из доминирующих
    pub background: [u8; 3],
    /// самый светлый из доминирующих
    pub foreground: [u8; 3],
    /// обои в целом тёмные
    pub dark: bool,
}

fn luma(c: [u8; 3]) -> f32 {
    (0.2126 * c[0] as f32 + 0.7152 * c[1] as f32 + 0.0722 * c[2] as f32) / 255.0
}

/// (насыщенность, яркость) в модели HSV, обе 0..1
fn sat_val(c: [u8; 3]) -> (f32, f32) {
    let mx = c.iter().copied().max().unwrap_or(0) as f32;
    let mn = c.iter().copied().min().unwrap_or(0) as f32;
    let s = if mx > 0.0 { (mx - mn) / mx } else { 0.0 };
    (s, mx / 255.0)
}

fn dist2(a: [u8; 3], b: [u8; 3]) -> i32 {
    (0..3)
        .map(|i| {
            let d = a[i] as i32 - b[i] as i32;
            d * d
        })
        .sum()
}

fn scale(c: [u8; 3], k: f32) -> [u8; 3] {
    [
        (c[0] as f32 * k).clamp(0.0, 255.0) as u8,
        (c[1] as f32 * k).clamp(0.0, 255.0) as u8,
        (c[2] as f32 * k).clamp(0.0, 255.0) as u8,
    ]
}

fn mix(a: [u8; 3], b: [u8; 3], t: f32) -> [u8; 3] {
    let f = |i: usize| (a[i] as f32 * (1.0 - t) + b[i] as f32 * t).clamp(0.0, 255.0) as u8;
    [f(0), f(1), f(2)]
}

pub fn hex(c: [u8; 3]) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// Извлекает палитру из RGB8-буфера. Берёт ~6000 пикселей по сетке, стоит микросекунды.
pub fn extract(rgb: &[u8], w: u32, h: u32) -> Palette {
    let (w, h) = (w as usize, h as usize);
    let step = (((w * h) as f32 / 6000.0).sqrt().ceil() as usize).max(1);

    // гистограмма 16x16x16 со средним цветом внутри каждой корзины
    let mut cnt = vec![0u32; 4096];
    let mut sum = vec![[0u32; 3]; 4096];
    let mut luma_sum = 0.0f64;
    let mut n = 0u64;

    let mut y = 0;
    while y < h {
        let mut x = 0;
        while x < w {
            let i = (y * w + x) * 3;
            if i + 2 < rgb.len() {
                let (r, g, b) = (rgb[i], rgb[i + 1], rgb[i + 2]);
                let bin = (((r >> 4) as usize) << 8) | (((g >> 4) as usize) << 4) | ((b >> 4) as usize);
                cnt[bin] += 1;
                sum[bin][0] += r as u32;
                sum[bin][1] += g as u32;
                sum[bin][2] += b as u32;
                luma_sum += luma([r, g, b]) as f64;
                n += 1;
            }
            x += step;
        }
        y += step;
    }

    // кандидаты: вес = частота * (0.4 + насыщенность), чтобы серый фон не вытеснял цвет
    let mut cand: Vec<([u8; 3], f32)> = Vec::new();
    for b in 0..4096 {
        if cnt[b] == 0 {
            continue;
        }
        let c = [
            (sum[b][0] / cnt[b]) as u8,
            (sum[b][1] / cnt[b]) as u8,
            (sum[b][2] / cnt[b]) as u8,
        ];
        let (s, _) = sat_val(c);
        cand.push((c, cnt[b] as f32 * (0.4 + s)));
    }
    cand.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

    // жадный отбор заметно различающихся цветов
    let mut colors: Vec<[u8; 3]> = Vec::new();
    for (c, _) in &cand {
        if colors.iter().all(|k| dist2(*k, *c) > 40 * 40) {
            colors.push(*c);
            if colors.len() == 6 {
                break;
            }
        }
    }
    if colors.is_empty() {
        colors.push([0, 0, 0]);
    }

    // акцент: самый насыщенный цвет средней яркости; если все серые, берём главный
    let mut primary = colors[0];
    let mut best = 0.12f32;
    for c in &colors {
        let (s, v) = sat_val(*c);
        let score = s * (1.0 - (v - 0.65).abs());
        if s > 0.12 && score > best {
            best = score;
            primary = *c;
        }
    }

    let mut background = colors[0];
    let mut foreground = colors[0];
    for c in &colors {
        if luma(*c) < luma(background) {
            background = *c;
        }
        if luma(*c) > luma(foreground) {
            foreground = *c;
        }
    }
    if colors.len() < 2 || dist2(background, foreground) < 60 * 60 {
        // обои почти одноцветные: достраиваем контрастную пару из главного цвета
        background = scale(primary, 0.18);
        foreground = mix(primary, [255, 255, 255], 0.85);
    }

    let avg = if n > 0 { luma_sum / n as f64 } else { 0.0 };
    Palette {
        colors,
        primary,
        background,
        foreground,
        dark: avg < 0.5,
    }
}

fn json_str(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 2);
    o.push('"');
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push(' '),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

impl Palette {
    pub fn to_json(&self, wallpaper: &str) -> String {
        let colors = self
            .colors
            .iter()
            .map(|c| format!("\"{}\"", hex(*c)))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "{{\n  \"wallpaper\": {},\n  \"dark\": {},\n  \"primary\": \"{}\",\n  \"background\": \"{}\",\n  \"foreground\": \"{}\",\n  \"colors\": [{}]\n}}\n",
            json_str(wallpaper),
            self.dark,
            hex(self.primary),
            hex(self.background),
            hex(self.foreground),
            colors
        )
    }
}

pub fn file_path() -> PathBuf {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))
        .unwrap_or_else(|| PathBuf::from("/tmp"));
    base.join("iriswall").join("palette.json")
}

/// Пишет palette.json атомарно (через временный файл), чтобы наблюдатели
/// за файлом (FileView в Quickshell и т. п.) никогда не видели половину записи.
pub fn save(p: &Palette, wallpaper: &Path) {
    let path = file_path();
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, p.to_json(&wallpaper.to_string_lossy())).is_ok() {
        let _ = std::fs::rename(&tmp, &path);
    }
}

/// Запускает пользовательскую команду из `hook` через `sh -c`, не блокируя демон.
/// Переменные окружения: IRISWALL_PATH, IRISWALL_PRIMARY, IRISWALL_BACKGROUND,
/// IRISWALL_FOREGROUND, IRISWALL_DARK (1/0), IRISWALL_COLORS (через пробел), IRISWALL_PALETTE.
pub fn run_hook(cmd: &str, wallpaper: &Path, p: &Palette) {
    let colors = p
        .colors
        .iter()
        .map(|c| hex(*c))
        .collect::<Vec<_>>()
        .join(" ");
    let child = Command::new("sh")
        .arg("-c")
        .arg(cmd)
        .env("IRISWALL_PATH", wallpaper)
        .env("IRISWALL_PRIMARY", hex(p.primary))
        .env("IRISWALL_BACKGROUND", hex(p.background))
        .env("IRISWALL_FOREGROUND", hex(p.foreground))
        .env("IRISWALL_DARK", if p.dark { "1" } else { "0" })
        .env("IRISWALL_COLORS", colors)
        .env("IRISWALL_PALETTE", file_path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match child {
        // поток нужен только чтобы не оставлять зомби
        Ok(mut c) => {
            std::thread::spawn(move || {
                let _ = c.wait();
            });
        }
        Err(e) => eprintln!("iriswall: не удалось запустить hook: {e}"),
    }
}
