use serde::Deserialize;
use std::path::{Path, PathBuf};

pub const DEFAULT_CONFIG: &str = r##"# iriswall — конфиг
# После правки: iriswall reload

# Папка с обоями (jpg, png, webp, bmp, gif; без вложенных папок)
dir = "~/Pictures/Wallpapers"

# Текущие обои. Демон сам обновляет эту строку при каждой смене.
# Можно поменять руками и выполнить `iriswall reload`.
current = ""

# fill — заполнить экран с обрезкой, fit — вписать целиком (поля цвета background)
mode = "fill"

# Длительность перехода (мс). 0 — без анимации
transition_ms = 1400

# ease-in-out | ease-out | linear
easing = "ease-in-out"

# Эффект перехода: random | iris | sweep | ripple | mosaic | zoom | ink
# iris — круг из центра, sweep — волна под случайным углом, ripple — круг с рябью,
# mosaic — плитки, zoom — пролёт с размытием, ink — растекание по шуму
effect = "random"

# Из каких эффектов выбирать при effect = "random" (подряд один и тот же не повторяется)
effects = ["iris", "sweep", "ripple", "mosaic", "zoom", "ink"]

# Мягкость края (iris, ripple): доля от половины диагонали экрана (0.0 — резкий край)
softness = 0.18

# Лёгкий «разворот»: новые обои стартуют увеличенными на эту долю и оседают (0.0 — выкл)
zoom = 0.05

# Автосмена каждые N секунд (0 — выключено)
interval = 0

# true — случайный выбор при автосмене и команде next
random = false

# Цвет фона (поля в режиме fit и пока нет картинки)
background = "#000000"

# Команда, которая запускается после каждой смены обоев (через `sh -c`, не блокирует демон).
# Получает переменные: IRISWALL_PATH, IRISWALL_PRIMARY, IRISWALL_BACKGROUND,
# IRISWALL_FOREGROUND, IRISWALL_DARK (1/0), IRISWALL_COLORS, IRISWALL_PALETTE.
# Пример: hook = "~/.config/scripts/theme-from-wallpaper.sh"
hook = ""

# Освобождать память в простое: возвращать кучу системе и вытеснять анонимные
# страницы в swap/zram (если он есть). Через ~3 с после смены обоев
trim_idle = true
"##;

pub const EFFECTS: [&str; 6] = ["iris", "sweep", "ripple", "mosaic", "zoom", "ink"];

pub fn effect_id(name: &str) -> Option<i32> {
    EFFECTS.iter().position(|e| *e == name).map(|i| i as i32)
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub dir: String,
    pub current: String,
    pub mode: String,
    pub transition_ms: u64,
    pub easing: String,
    pub effect: String,
    pub effects: Vec<String>,
    pub softness: f32,
    pub zoom: f32,
    pub interval: u64,
    pub random: bool,
    pub background: String,
    pub hook: String,
    pub trim_idle: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            dir: "~/Pictures/Wallpapers".into(),
            current: String::new(),
            mode: "fill".into(),
            transition_ms: 1400,
            easing: "ease-in-out".into(),
            effect: "random".into(),
            effects: EFFECTS.iter().map(|s| s.to_string()).collect(),
            softness: 0.18,
            zoom: 0.05,
            interval: 0,
            random: false,
            background: "#000000".into(),
            hook: String::new(),
            trim_idle: true,
        }
    }
}

pub fn expand(s: &str) -> PathBuf {
    if s == "~" || s.starts_with("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut p = PathBuf::from(home);
            if s.len() > 2 {
                p.push(&s[2..]);
            }
            return p;
        }
    }
    PathBuf::from(s)
}

pub fn config_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("iriswall").join("config.toml")
}

pub fn load(path: &Path) -> Result<Config, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    toml::from_str::<Config>(&text).map_err(|e| format!("{}: {e}", path.display()))
}

/// Читает конфиг; если файла нет — создаёт его со значениями по умолчанию.
pub fn load_or_create(path: &Path) -> Config {
    if !path.exists() {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(path, DEFAULT_CONFIG) {
            Ok(_) => eprintln!("iriswall: создан конфиг {}", path.display()),
            Err(e) => eprintln!("iriswall: не удалось создать конфиг: {e}"),
        }
    }
    match load(path) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("iriswall: ошибка конфига ({e}), использую значения по умолчанию");
            Config::default()
        }
    }
}

/// Обновляет только строку `current`, сохраняя комментарии и остальной конфиг.
pub fn save_current(path: &Path, cur: &Path) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(mut doc) = text.parse::<toml_edit::DocumentMut>() else {
        eprintln!("iriswall: не могу разобрать конфиг для записи current");
        return;
    };
    doc["current"] = toml_edit::value(cur.to_string_lossy().to_string());
    if let Err(e) = std::fs::write(path, doc.to_string()) {
        eprintln!("iriswall: не удалось записать конфиг: {e}");
    }
}

/// Записывает строковое значение в конфиг, сохраняя комментарии и остальные ключи.
pub fn save_str(path: &Path, key: &str, val: &str) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let Ok(mut doc) = text.parse::<toml_edit::DocumentMut>() else {
        return;
    };
    doc[key] = toml_edit::value(val);
    let _ = std::fs::write(path, doc.to_string());
}

fn is_image(p: &Path) -> bool {
    match p.extension().and_then(|e| e.to_str()) {
        Some(e) => matches!(
            e.to_ascii_lowercase().as_str(),
            "jpg" | "jpeg" | "png" | "webp" | "bmp" | "gif"
        ),
        None => false,
    }
}

pub fn list_images(dir: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = match std::fs::read_dir(dir) {
        Ok(rd) => rd
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.is_file() && is_image(p))
            .collect(),
        Err(_) => Vec::new(),
    };
    v.sort();
    v
}

pub fn parse_color(s: &str) -> [f32; 3] {
    let h = s.trim().trim_start_matches('#');
    if h.len() == 6 {
        if let Ok(v) = u32::from_str_radix(h, 16) {
            return [
                ((v >> 16) & 0xff) as f32 / 255.0,
                ((v >> 8) & 0xff) as f32 / 255.0,
                (v & 0xff) as f32 / 255.0,
            ];
        }
    }
    [0.0, 0.0, 0.0]
}

pub fn ease(name: &str, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    match name {
        "linear" => t,
        "ease-out" => 1.0 - (1.0 - t).powi(4),
        _ => {
            if t < 0.5 {
                4.0 * t * t * t
            } else {
                1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
            }
        }
    }
}
