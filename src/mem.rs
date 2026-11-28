//! Управление памятью демона.
//!
//! Раньше для экономии памяти приходилось запускать демон с переменными `MALLOC_*`.
//! Теперь всё делается самим процессом: настройки glibc malloc, возврат памяти системе
//! после декодирования картинки и «сброс» страниц в простое.

use std::ffi::c_void;

#[cfg(all(target_os = "linux", target_env = "gnu"))]
extern "C" {
    fn mallopt(param: i32, value: i32) -> i32;
    fn malloc_trim(pad: usize) -> i32;
}

#[cfg(target_os = "linux")]
extern "C" {
    fn madvise(addr: *mut c_void, len: usize, advice: i32) -> i32;
}

/// Вызывать в самом начале `main`, до создания потоков.
pub fn tune() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        // M_ARENA_MAX: одна арена на все потоки (декодер не плодит свои кучи)
        mallopt(-8, 1);
        // M_MMAP_THRESHOLD: всё от 128 КБ идёт через mmap и возвращается системе сразу после free.
        // Явная установка ещё и отключает «подрастающий» порог, из-за которого после
        // декодирования большой картинки куча раздувалась и не сжималась
        mallopt(-3, 128 * 1024);
        // M_TRIM_THRESHOLD: отдавать свободный хвост кучи системе
        mallopt(-1, 128 * 1024);
    }
}

/// Переменные Mesa, которые экономят память драйвера. Ставятся перед инициализацией EGL,
/// только если пользователь не задал их сам. Возвращает список реально выставленных ключей.
pub fn mesa_env() -> Vec<&'static str> {
    let mut set = Vec::new();
    for (k, v) in [
        // дисковый кеш шейдеров: отдельные потоки и отображаемый индекс ради одного шейдера
        ("MESA_SHADER_CACHE_DISABLE", "true"),
        ("MESA_GLSL_CACHE_DISABLE", "true"),
        // отдельный поток-диспетчер GL не нужен
        ("mesa_glthread", "false"),
    ] {
        if std::env::var_os(k).is_none() {
            std::env::set_var(k, v);
            set.push(k);
        }
    }
    set
}

/// Убирает переменные, выставленные `mesa_env`, чтобы они не утекли в процессы хука.
pub fn unset_env(keys: &[&str]) {
    for k in keys {
        std::env::remove_var(k);
    }
}

/// Возвращает свободную память системе. Дёшево, можно вызывать после каждой смены обоев.
pub fn trim_heap() {
    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    unsafe {
        malloc_trim(0);
    }
}

/// Просит ядро вытеснить анонимные страницы процесса (куча и анонимные mmap).
/// Это только подсказка: содержимое не теряется, страницы вернутся при первом обращении.
/// Эффект есть, если в системе включён swap или zram. Файловые отображения
/// (библиотеки Mesa), GPU-буферы и разделяемая память не трогаются.
pub fn page_out() {
    #[cfg(target_os = "linux")]
    {
        const MADV_PAGEOUT: i32 = 21;
        let Ok(maps) = std::fs::read_to_string("/proc/self/maps") else {
            return;
        };
        for line in maps.lines() {
            let mut it = line.split_whitespace();
            let (Some(range), Some(perms)) = (it.next(), it.next()) else {
                continue;
            };
            let _ = (it.next(), it.next(), it.next()); // offset, dev, inode
            let name = it.collect::<Vec<_>>().join(" ");
            let pb = perms.as_bytes();
            if pb.len() < 4 || pb[0] != b'r' || pb[1] != b'w' || pb[3] != b'p' {
                continue; // нужны только приватные записываемые отображения
            }
            if !(name.is_empty() || name == "[heap]") {
                continue; // анонимные области и куча, остальное не трогаем
            }
            let Some((a, b)) = range.split_once('-') else {
                continue;
            };
            let (Ok(a), Ok(b)) = (usize::from_str_radix(a, 16), usize::from_str_radix(b, 16))
            else {
                continue;
            };
            if b > a {
                unsafe {
                    madvise(a as *mut c_void, b - a, MADV_PAGEOUT);
                }
            }
        }
    }
}

/// Освобождение памяти в простое.
pub fn trim(page_out_too: bool) {
    trim_heap();
    if page_out_too {
        page_out();
    }
}

/// Строки для `iriswall status`: сколько памяти занимает демон (в МБ).
pub fn report() -> String {
    let Ok(t) = std::fs::read_to_string("/proc/self/smaps_rollup") else {
        return String::new();
    };
    let get = |key: &str| -> f32 {
        t.lines()
            .find(|l| l.starts_with(key))
            .and_then(|l| l.split_whitespace().nth(1))
            .and_then(|v| v.parse::<f32>().ok())
            .unwrap_or(0.0)
            / 1024.0
    };
    format!(
        "mem_pss={:.1} MB\nmem_anon={:.1} MB\nmem_file={:.1} MB",
        get("Pss:"),
        get("Pss_Anon:"),
        get("Pss_File:")
    )
}
