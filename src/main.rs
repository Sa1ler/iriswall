mod config;
mod mem;
mod palette;
mod render;

use calloop::generic::Generic;
use calloop::timer::{TimeoutAction, Timer};
use calloop::{EventLoop, Interest, LoopHandle, LoopSignal, Mode, PostAction};
use calloop_wayland_source::WaylandSource;
use config::Config;
use khronos_egl as egl;
use palette::Palette;
use render::{DrawParams, Image, Renderer, Tex};
use std::error::Error;
use std::ffi::c_void;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use wayland_client::globals::{registry_queue_init, GlobalListContents};
use wayland_client::protocol::wl_callback::{self, WlCallback};
use wayland_client::protocol::wl_compositor::WlCompositor;
use wayland_client::protocol::wl_output::WlOutput;
use wayland_client::protocol::wl_registry::{self, WlRegistry};
use wayland_client::protocol::wl_surface::WlSurface;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_egl::WlEglSurface;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1;
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{
    self, WpFractionalScaleV1,
};
use wayland_protocols::wp::viewporter::client::wp_viewport::WpViewport;
use wayland_protocols::wp::viewporter::client::wp_viewporter::WpViewporter;
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_shell_v1::{self, ZwlrLayerShellV1};
use wayland_protocols_wlr::layer_shell::v1::client::zwlr_layer_surface_v1::{
    self, Anchor, KeyboardInteractivity, ZwlrLayerSurfaceV1,
};

type Egl = egl::Instance<egl::Static>;

// ───────────────────────── Состояние ─────────────────────────

struct Output {
    name: u32,
    wl_output: WlOutput,
    surface: WlSurface,
    layer: ZwlrLayerSurfaceV1,
    viewport: Option<WpViewport>,
    frac: Option<WpFractionalScaleV1>,
    egl_win: Option<WlEglSurface>,
    egl_surf: Option<egl::Surface>,
    lw: i32,
    lh: i32,
    scale120: u32,
    configured: bool,
    frame_pending: bool,
}

impl Output {
    /// Размер буфера в физических пикселях
    fn px(&self) -> (u32, u32) {
        let f = |v: i32| (((v as u64 * self.scale120 as u64 + 60) / 120) as u32).max(1);
        (f(self.lw), f(self.lh))
    }
}

struct Transition {
    start: Instant,
    dur: Duration,
    effect: i32,
    origin: [f32; 2],
    angle: f32,
    seed: f32,
}

struct App {
    qh: QueueHandle<App>,
    handle: LoopHandle<'static, App>,
    signal: LoopSignal,
    tx: calloop::channel::Sender<Result<Image, String>>,

    registry: WlRegistry,
    compositor: WlCompositor,
    layer_shell: ZwlrLayerShellV1,
    viewporter: Option<WpViewporter>,
    frac_mgr: Option<WpFractionalScaleManagerV1>,

    egl: Egl,
    egl_display: egl::Display,
    egl_config: egl::Config,
    egl_ctx: egl::Context,
    renderer: Option<Renderer>,
    renderer_failed: bool,

    outputs: Vec<Output>,

    cfg: Config,
    cfg_path: PathBuf,
    files: Vec<PathBuf>,
    cur_path: Option<PathBuf>,
    inflight: Option<PathBuf>,
    queued: Option<PathBuf>,
    pending: Option<Image>,

    cur: Option<Tex>,
    old: Option<Tex>,
    trash: Vec<Tex>,
    transition: Option<Transition>,
    transition_gen: u64,
    rot_gen: u64,
    rng: u64,
    last_effect: i32,

    history: Vec<PathBuf>,
    back_target: Option<PathBuf>,
    cur_palette: Option<Palette>,
    trim_gen: u64,
}

impl App {
    fn idx(&self, name: u32) -> Option<usize> {
        self.outputs.iter().position(|o| o.name == name)
    }

    // ───── Выходы (мониторы) ─────

    fn add_output(&mut self, name: u32, version: u32) {
        if self.idx(name).is_some() {
            return;
        }
        let wl_output = self
            .registry
            .bind::<WlOutput, _, _>(name, version.min(4), &self.qh, name);
        let surface = self.compositor.create_surface(&self.qh, ());
        let layer = self.layer_shell.get_layer_surface(
            &surface,
            Some(&wl_output),
            zwlr_layer_shell_v1::Layer::Background,
            "iriswall".to_string(),
            &self.qh,
            name,
        );
        layer.set_anchor(Anchor::Top | Anchor::Bottom | Anchor::Left | Anchor::Right);
        layer.set_exclusive_zone(-1);
        layer.set_size(0, 0);
        layer.set_keyboard_interactivity(KeyboardInteractivity::None);
        let viewport = self
            .viewporter
            .as_ref()
            .map(|v| v.get_viewport(&surface, &self.qh, ()));
        let frac = self
            .frac_mgr
            .as_ref()
            .map(|m| m.get_fractional_scale(&surface, &self.qh, name));
        surface.commit();
        self.outputs.push(Output {
            name,
            wl_output,
            surface,
            layer,
            viewport,
            frac,
            egl_win: None,
            egl_surf: None,
            lw: 0,
            lh: 0,
            scale120: 120,
            configured: false,
            frame_pending: false,
        });
    }

    fn remove_output(&mut self, name: u32) {
        let Some(i) = self.idx(name) else { return };
        let mut o = self.outputs.remove(i);
        if let Some(s) = o.egl_surf.take() {
            let _ = self.egl.make_current(self.egl_display, None, None, None);
            let _ = self.egl.destroy_surface(self.egl_display, s);
        }
        o.egl_win = None;
        if let Some(f) = o.frac.take() {
            f.destroy();
        }
        if let Some(v) = o.viewport.take() {
            v.destroy();
        }
        o.layer.destroy();
        o.surface.destroy();
        if o.wl_output.version() >= 3 {
            o.wl_output.release();
        }
    }

    fn configure(&mut self, name: u32, w: u32, h: u32) {
        let Some(i) = self.idx(name) else { return };
        {
            let o = &mut self.outputs[i];
            if w > 0 && h > 0 {
                o.lw = w as i32;
                o.lh = h as i32;
            }
            if o.lw == 0 || o.lh == 0 {
                return;
            }
            if let Some(v) = &o.viewport {
                v.set_destination(o.lw, o.lh);
            }
        }
        let (pw, ph) = self.outputs[i].px();
        if self.outputs[i].egl_win.is_none() {
            let id = self.outputs[i].surface.id();
            let win = match WlEglSurface::new(id, pw as i32, ph as i32) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("iriswall: wl_egl_window: {e}");
                    return;
                }
            };
            let surf = unsafe {
                self.egl.create_window_surface(
                    self.egl_display,
                    self.egl_config,
                    win.ptr() as egl::NativeWindowType,
                    None,
                )
            };
            match surf {
                Ok(s) => {
                    let o = &mut self.outputs[i];
                    o.egl_win = Some(win);
                    o.egl_surf = Some(s);
                    o.configured = true;
                }
                Err(e) => {
                    eprintln!("iriswall: eglCreateWindowSurface: {e}");
                    return;
                }
            }
            self.make_current(i);
            // без блокировки на vsync: темп задаёт frame callback
            let _ = self.egl.swap_interval(self.egl_display, 0);
        } else {
            if let Some(win) = self.outputs[i].egl_win.as_ref() {
                win.resize(pw as i32, ph as i32, 0, 0);
            }
            self.outputs[i].configured = true;
        }
        if !self.try_apply_pending() {
            self.draw_output(i);
        }
    }

    fn set_scale(&mut self, name: u32, s: u32) {
        let Some(i) = self.idx(name) else { return };
        // без viewporter дробный масштаб применить нельзя
        if self.outputs[i].viewport.is_none() || self.outputs[i].scale120 == s {
            return;
        }
        self.outputs[i].scale120 = s;
        if self.outputs[i].egl_win.is_some() {
            let (w, h) = self.outputs[i].px();
            if let Some(win) = self.outputs[i].egl_win.as_ref() {
                win.resize(w as i32, h as i32, 0, 0);
            }
            self.draw_output(i);
        }
    }

    fn max_px(&self) -> (u32, u32) {
        let (mut w, mut h) = (0, 0);
        for o in self.outputs.iter().filter(|o| o.configured) {
            let (a, b) = o.px();
            w = w.max(a);
            h = h.max(b);
        }
        if w == 0 {
            (3840, 2160)
        } else {
            (w, h)
        }
    }

    // ───── EGL / GL ─────

    fn make_current(&mut self, i: usize) {
        let Some(s) = self.outputs[i].egl_surf else { return };
        let _ = self
            .egl
            .make_current(self.egl_display, Some(s), Some(s), Some(self.egl_ctx));
        if self.renderer.is_none() && !self.renderer_failed {
            let egl = &self.egl;
            let loader = |name: &str| {
                egl.get_proc_address(name)
                    .map_or(std::ptr::null(), |p| p as *const c_void)
            };
            match Renderer::new(&loader) {
                Ok(r) => self.renderer = Some(r),
                Err(e) => {
                    self.renderer_failed = true;
                    eprintln!("iriswall: ошибка шейдера: {e}");
                }
            }
        }
        if let Some(r) = self.renderer.as_ref() {
            for t in self.trash.drain(..) {
                r.delete(t);
            }
        }
    }

    fn ensure_current(&mut self) -> bool {
        let Some(i) = self.outputs.iter().position(|o| o.egl_surf.is_some()) else {
            return false;
        };
        self.make_current(i);
        self.renderer.is_some()
    }

    // ───── Рисование ─────

    fn draw_output(&mut self, i: usize) {
        if i >= self.outputs.len() {
            return;
        }
        if !self.outputs[i].configured || self.outputs[i].egl_surf.is_none() {
            return;
        }
        self.make_current(i);
        let (pw, ph) = self.outputs[i].px();
        let bg = config::parse_color(&self.cfg.background);
        let mut animating = false;
        {
            let Some(r) = self.renderer.as_ref() else { return };
            match self.cur.as_ref() {
                None => r.clear(bg, pw, ph),
                Some(cur) => {
                    let (progress, has_old, effect, origin, angle, seed) = match &self.transition {
                        Some(t) => {
                            let x = (t.start.elapsed().as_secs_f32() / t.dur.as_secs_f32())
                                .clamp(0.0, 1.0);
                            animating = x < 1.0;
                            (
                                config::ease(&self.cfg.easing, x),
                                self.old.is_some(),
                                t.effect,
                                t.origin,
                                t.angle,
                                t.seed,
                            )
                        }
                        None => (1.0, false, 0, [0.5, 0.5], 0.0, 0.0),
                    };
                    let p = DrawParams {
                        width: pw,
                        height: ph,
                        progress,
                        fit: self.cfg.mode == "fit",
                        softness: self.cfg.softness,
                        zoom: self.cfg.zoom,
                        bg,
                        effect,
                        origin,
                        angle,
                        seed,
                    };
                    r.draw(&p, if has_old { self.old.as_ref() } else { None }, cur);
                }
            }
        }
        if animating && !self.outputs[i].frame_pending {
            let name = self.outputs[i].name;
            self.outputs[i].surface.frame(&self.qh, name);
            self.outputs[i].frame_pending = true;
        }
        if let Some(s) = self.outputs[i].egl_surf {
            let _ = self.egl.swap_buffers(self.egl_display, s);
        }
    }

    fn draw_all(&mut self) {
        for i in 0..self.outputs.len() {
            self.draw_output(i);
        }
    }

    fn frame_done(&mut self, name: u32) {
        let Some(i) = self.idx(name) else { return };
        self.outputs[i].frame_pending = false;
        if self.transition.is_some() {
            self.draw_output(i);
        }
    }

    // ───── Смена обоев ─────

    fn try_apply_pending(&mut self) -> bool {
        if self.pending.is_none() || !self.ensure_current() {
            return false;
        }
        let img = self.pending.take().unwrap();

        // mip-уровни только если картинка где-то показывается с заметным уменьшением
        let fit = self.cfg.mode == "fit";
        let mips = self.outputs.iter().filter(|o| o.configured).any(|o| {
            let (pw, ph) = o.px();
            let a = pw as f32 / img.w as f32;
            let b = ph as f32 / img.h as f32;
            (if fit { a.min(b) } else { a.max(b) }) < 0.75
        });
        let tex = self.renderer.as_ref().unwrap().upload(&img, mips);

        // пиксели на CPU больше не нужны: освобождаем сразу
        let Image {
            path,
            palette,
            rgb,
            ..
        } = img;
        drop(rgb);

        // история для `iriswall back`; возврат назад сам историю не пополняет
        let is_back = self.back_target.as_ref() == Some(&path);
        if is_back {
            self.back_target = None;
        }
        let prev = self.cur_path.replace(path.clone());
        if !is_back {
            if let Some(p) = prev {
                if p != path {
                    self.history.push(p);
                    if self.history.len() > 50 {
                        self.history.remove(0);
                    }
                }
            }
        }

        self.cfg.current = path.to_string_lossy().to_string();
        config::save_current(&self.cfg_path, &path);
        self.after_change(&path, palette);
        self.start_transition(tex);
        self.schedule_trim();
        true
    }

    /// Палитра в файл и пользовательский hook
    fn after_change(&mut self, path: &Path, palette: Palette) {
        palette::save(&palette, path);
        if !self.cfg.hook.trim().is_empty() {
            palette::run_hook(&self.cfg.hook, path, &palette);
        }
        self.cur_palette = Some(palette);
    }

    /// Через несколько секунд после конца перехода возвращаем память системе
    fn schedule_trim(&mut self) {
        self.trim_gen += 1;
        let g = self.trim_gen;
        let delay = Duration::from_millis(self.cfg.transition_ms + 3000);
        let _ = self
            .handle
            .insert_source(Timer::from_duration(delay), move |_, _, app: &mut App| {
                if app.trim_gen == g && app.transition.is_none() && app.inflight.is_none() {
                    mem::trim(app.cfg.trim_idle);
                }
                TimeoutAction::Drop
            });
    }

    fn start_transition(&mut self, tex: Tex) {
        // если прошлая анимация ещё идёт — её «старые» обои выбрасываем
        if let Some(o) = self.old.take() {
            self.trash.push(o);
        }
        let prev = self.cur.replace(tex);
        match prev {
            Some(p) if self.cfg.transition_ms > 0 => {
                self.old = Some(p);
                self.transition_gen += 1;
                let gen = self.transition_gen;
                let dur = Duration::from_millis(self.cfg.transition_ms);
                let (effect, origin, angle, seed) = self.pick_effect();
                self.transition = Some(Transition {
                    start: Instant::now(),
                    dur,
                    effect,
                    origin,
                    angle,
                    seed,
                });
                // страховка: финальный кадр и освобождение старой текстуры,
                // даже если compositor перестал присылать frame callback
                let _ = self.handle.insert_source(
                    Timer::from_duration(dur + Duration::from_millis(40)),
                    move |_, _, app: &mut App| {
                        app.finish_transition(gen);
                        TimeoutAction::Drop
                    },
                );
            }
            Some(p) => {
                self.trash.push(p);
                self.transition = None;
            }
            None => {
                self.transition = None;
            }
        }
        self.draw_all();
    }

    fn finish_transition(&mut self, gen: u64) {
        if self.transition_gen != gen || self.transition.is_none() {
            return;
        }
        self.transition = None;
        if let Some(o) = self.old.take() {
            self.trash.push(o);
        }
        self.draw_all();
    }

    fn request(&mut self, path: PathBuf) {
        if self.inflight.is_some() {
            self.queued = Some(path);
            return;
        }
        self.start_decode(path);
    }

    fn start_decode(&mut self, path: PathBuf) {
        let (mw, mh) = self.max_px();
        self.inflight = Some(path.clone());
        let tx = self.tx.clone();
        std::thread::spawn(move || {
            let r = render::decode(&path, mw, mh);
            let _ = tx.send(r);
        });
    }

    fn on_decoded(&mut self, res: Result<Image, String>) {
        self.inflight = None;
        if let Some(next) = self.queued.take() {
            // пришёл более свежий запрос — этот результат не нужен
            self.start_decode(next);
            return;
        }
        match res {
            Ok(img) => {
                self.pending = Some(img);
                self.try_apply_pending();
            }
            Err(e) => eprintln!("iriswall: {e}"),
        }
    }

    // ───── Выбор файлов ─────

    fn randf(&mut self) -> f32 {
        (self.rand() >> 40) as f32 / (1u64 << 24) as f32
    }

    /// Выбирает эффект для очередной смены (не повторяя предыдущий) и его случайные параметры
    fn pick_effect(&mut self) -> (i32, [f32; 2], f32, f32) {
        let mut pool: Vec<i32> = if self.cfg.effect == "random" {
            self.cfg
                .effects
                .iter()
                .filter_map(|n| config::effect_id(n))
                .collect()
        } else {
            config::effect_id(&self.cfg.effect).into_iter().collect()
        };
        if pool.is_empty() {
            pool.push(0);
        }
        let mut e = pool[(self.rand() % pool.len() as u64) as usize];
        if pool.len() > 1 {
            while e == self.last_effect {
                e = pool[(self.rand() % pool.len() as u64) as usize];
            }
        }
        self.last_effect = e;
        let origin = [0.12 + 0.76 * self.randf(), 0.18 + 0.64 * self.randf()];
        let angle = self.randf() * std::f32::consts::TAU;
        let seed = self.randf() * 100.0;
        (e, origin, angle, seed)
    }

    fn rescan(&mut self) {
        self.files = config::list_images(&config::expand(&self.cfg.dir));
    }

    fn rand(&mut self) -> u64 {
        let mut x = self.rng;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.rng = x;
        x
    }

    fn pick_random(&mut self, avoid: Option<usize>) -> usize {
        let n = self.files.len();
        if n <= 1 {
            return 0;
        }
        loop {
            let k = (self.rand() % n as u64) as usize;
            if Some(k) != avoid {
                return k;
            }
        }
    }

    fn cycle(&mut self, dir: i32, force_random: bool) {
        self.rescan();
        if self.files.is_empty() {
            eprintln!("iriswall: в {} нет картинок", self.cfg.dir);
            return;
        }
        let base = self
            .queued
            .clone()
            .or_else(|| self.inflight.clone())
            .or_else(|| self.cur_path.clone());
        let idx = base.and_then(|b| self.files.iter().position(|f| *f == b));
        let n = self.files.len() as i32;
        let next = if force_random || (self.cfg.random && dir > 0) {
            self.pick_random(idx)
        } else {
            match idx {
                Some(i) => (i as i32 + dir).rem_euclid(n) as usize,
                None => 0,
            }
        };
        let p = self.files[next].clone();
        self.request(p);
    }

    fn setup_rotation(&mut self) {
        self.rot_gen += 1;
        let g = self.rot_gen;
        if self.cfg.interval == 0 {
            return;
        }
        let d = Duration::from_secs(self.cfg.interval);
        let _ = self
            .handle
            .insert_source(Timer::from_duration(d), move |_, _, app: &mut App| {
                if app.rot_gen != g {
                    return TimeoutAction::Drop;
                }
                app.cycle(1, false);
                TimeoutAction::ToDuration(Duration::from_secs(app.cfg.interval.max(1)))
            });
    }

    fn reload(&mut self) -> String {
        match config::load(&self.cfg_path) {
            Ok(c) => self.cfg = c,
            Err(e) => return format!("ошибка конфига: {e}"),
        }
        self.rescan();
        self.setup_rotation();
        if !self.cfg.current.is_empty() {
            let want = config::expand(&self.cfg.current);
            if want.is_file() && Some(&want) != self.cur_path.as_ref() {
                self.request(want);
            }
        }
        self.draw_all();
        "ok".into()
    }

    // ───── IPC ─────

    fn handle_client(&mut self, mut stream: UnixStream) {
        let _ = stream.set_nonblocking(false);
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let mut line = String::new();
        if BufReader::new(&stream).read_line(&mut line).is_err() {
            return;
        }
        let reply = self.handle_cmd(&line);
        let _ = stream.write_all(reply.as_bytes());
        let _ = stream.write_all(b"\n");
    }

    fn handle_cmd(&mut self, line: &str) -> String {
        let line = line.trim();
        let (cmd, arg) = match line.split_once(' ') {
            Some((c, a)) => (c, a.trim()),
            None => (line, ""),
        };
        match cmd {
            "next" => {
                self.cycle(1, false);
                "ok".into()
            }
            "prev" => {
                self.cycle(-1, false);
                "ok".into()
            }
            "random" => {
                self.cycle(1, true);
                "ok".into()
            }
            "set" => {
                let p = config::expand(arg);
                if !p.is_file() {
                    return format!("нет такого файла: {arg}");
                }
                if Some(&p) == self.cur_path.as_ref() && self.inflight.is_none() {
                    return "уже установлено".into();
                }
                self.request(p);
                "ok".into()
            }
            "effect" => {
                if arg == "random" || config::effect_id(arg).is_some() {
                    self.cfg.effect = arg.to_string();
                    config::save_str(&self.cfg_path, "effect", arg);
                    "ok".into()
                } else {
                    format!(
                        "неизвестный эффект: {arg} (доступно: random, {})",
                        config::EFFECTS.join(", ")
                    )
                }
            }
            "back" => {
                while let Some(p) = self.history.pop() {
                    if p.is_file() {
                        self.back_target = Some(p.clone());
                        self.request(p);
                        return "ok".into();
                    }
                }
                "история пуста".into()
            }
            "current" => self.cfg.current.clone(),
            "palette" => match &self.cur_palette {
                Some(p) => p.to_json(&self.cfg.current).trim_end().to_string(),
                None => "палитры ещё нет".into(),
            },
            "reload" => self.reload(),
            "status" => format!(
                "current={}\ndir={}\nfiles={}\noutputs={}\ninterval={}s\nrandom={}\neffect={}\nhistory={}\n{}",
                self.cfg.current,
                self.cfg.dir,
                self.files.len(),
                self.outputs.len(),
                self.cfg.interval,
                self.cfg.random,
                self.cfg.effect,
                self.history.len(),
                mem::report()
            ),
            "quit" => {
                self.signal.stop();
                "bye".into()
            }
            other => format!("неизвестная команда: {other}"),
        }
    }
}

// ───────────────────────── Wayland: обработчики событий ─────────────────────────

macro_rules! noop_dispatch {
    ($iface:ty, $udata:ty) => {
        impl Dispatch<$iface, $udata> for App {
            fn event(
                _: &mut Self,
                _: &$iface,
                _: <$iface as Proxy>::Event,
                _: &$udata,
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    };
}

noop_dispatch!(WlCompositor, ());
noop_dispatch!(WlSurface, ());
noop_dispatch!(WlOutput, u32);
noop_dispatch!(ZwlrLayerShellV1, ());
noop_dispatch!(WpViewporter, ());
noop_dispatch!(WpViewport, ());
noop_dispatch!(WpFractionalScaleManagerV1, ());

impl Dispatch<WlRegistry, GlobalListContents> for App {
    fn event(
        app: &mut Self,
        _: &WlRegistry,
        event: wl_registry::Event,
        _: &GlobalListContents,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            wl_registry::Event::Global {
                name,
                interface,
                version,
            } => {
                if interface == "wl_output" {
                    app.add_output(name, version);
                }
            }
            wl_registry::Event::GlobalRemove { name } => app.remove_output(name),
            _ => {}
        }
    }
}

impl Dispatch<ZwlrLayerSurfaceV1, u32> for App {
    fn event(
        app: &mut Self,
        layer: &ZwlrLayerSurfaceV1,
        event: zwlr_layer_surface_v1::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_layer_surface_v1::Event::Configure {
                serial,
                width,
                height,
            } => {
                layer.ack_configure(serial);
                app.configure(*name, width, height);
            }
            zwlr_layer_surface_v1::Event::Closed => app.remove_output(*name),
            _ => {}
        }
    }
}

impl Dispatch<WpFractionalScaleV1, u32> for App {
    fn event(
        app: &mut Self,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            app.set_scale(*name, scale);
        }
    }
}

impl Dispatch<WlCallback, u32> for App {
    fn event(
        app: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        name: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            app.frame_done(*name);
        }
    }
}

// ───────────────────────── Запуск ─────────────────────────

fn socket_path() -> PathBuf {
    std::env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
        .join("iriswall.sock")
}

fn run_daemon() -> Result<(), Box<dyn Error>> {
    let sock = socket_path();
    if UnixStream::connect(&sock).is_ok() {
        return Err("демон уже запущен".into());
    }
    let _ = std::fs::remove_file(&sock);
    let listener = UnixListener::bind(&sock)?;
    listener.set_nonblocking(true)?;

    let cfg_path = config::config_path();
    let cfg = config::load_or_create(&cfg_path);

    let mut event_loop: EventLoop<'static, App> = EventLoop::try_new()?;
    let handle = event_loop.handle();
    let signal = event_loop.get_signal();

    // результаты декодера из рабочего потока
    let (tx, rx) = calloop::channel::channel::<Result<Image, String>>();
    handle
        .insert_source(rx, |event, _, app: &mut App| {
            if let calloop::channel::Event::Msg(m) = event {
                app.on_decoded(m);
            }
        })
        .map_err(|_| "не удалось подключить канал декодера")?;

    // управляющий сокет
    handle
        .insert_source(
            Generic::new(listener, Interest::READ, Mode::Level),
            |_, l, app: &mut App| {
                while let Ok((s, _)) = l.accept() {
                    app.handle_client(s);
                }
                Ok(PostAction::Continue)
            },
        )
        .map_err(|_| "не удалось подключить сокет")?;

    // Wayland
    let conn = Connection::connect_to_env()?;
    let (globals, mut event_queue) = registry_queue_init::<App>(&conn)?;
    let qh = event_queue.handle();
    let compositor: WlCompositor = globals.bind(&qh, 4..=6, ())?;
    let layer_shell: ZwlrLayerShellV1 = globals.bind(&qh, 1..=4, ())?;
    let viewporter: Option<WpViewporter> = globals.bind(&qh, 1..=1, ()).ok();
    let frac_mgr: Option<WpFractionalScaleManagerV1> = globals.bind(&qh, 1..=1, ()).ok();
    if viewporter.is_none() || frac_mgr.is_none() {
        eprintln!("iriswall: нет viewporter/fractional-scale — дробный масштаб не поддерживается");
    }

    // EGL + GLES3 (перед инициализацией драйвера просим Mesa не тратить память зря)
    let mesa_keys = mem::mesa_env();
    let egl = egl::Instance::new(egl::Static);
    let egl_display = unsafe { egl.get_display(conn.backend().display_ptr() as *mut c_void) }
        .ok_or("eglGetDisplay вернул пусто")?;
    egl.initialize(egl_display)?;
    egl.bind_api(egl::OPENGL_ES_API)?;
    let attribs = [
        egl::SURFACE_TYPE,
        egl::WINDOW_BIT,
        egl::RENDERABLE_TYPE,
        0x0040, // EGL_OPENGL_ES3_BIT
        egl::RED_SIZE,
        8,
        egl::GREEN_SIZE,
        8,
        egl::BLUE_SIZE,
        8,
        egl::NONE,
    ];
    let egl_config = egl
        .choose_first_config(egl_display, &attribs)?
        .ok_or("нет подходящей EGL-конфигурации (нужен GLES3)")?;
    let egl_ctx = egl.create_context(
        egl_display,
        egl_config,
        None,
        &[egl::CONTEXT_CLIENT_VERSION, 3, egl::NONE],
    )?;
    // драйвер уже прочитал настройки; не передаём их процессам из hook
    mem::unset_env(&mesa_keys);

    let seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        | 1;

    let mut app = App {
        qh: qh.clone(),
        handle: handle.clone(),
        signal,
        tx,
        registry: globals.registry().clone(),
        compositor,
        layer_shell,
        viewporter,
        frac_mgr,
        egl,
        egl_display,
        egl_config,
        egl_ctx,
        renderer: None,
        renderer_failed: false,
        outputs: Vec::new(),
        cfg,
        cfg_path,
        files: Vec::new(),
        cur_path: None,
        inflight: None,
        queued: None,
        pending: None,
        cur: None,
        old: None,
        trash: Vec::new(),
        transition: None,
        transition_gen: 0,
        rot_gen: 0,
        rng: seed,
        last_effect: -1,
        history: Vec::new(),
        back_target: None,
        cur_palette: None,
        trim_gen: 0,
    };

    // уже существующие мониторы
    let mut existing = Vec::new();
    globals.contents().with_list(|list| {
        for g in list {
            if g.interface == "wl_output" {
                existing.push((g.name, g.version));
            }
        }
    });
    for (name, version) in existing {
        app.add_output(name, version);
    }
    // дождаться configure, чтобы знать размеры мониторов до декодирования
    event_queue.roundtrip(&mut app)?;
    event_queue.roundtrip(&mut app)?;

    // стартовые обои: current из конфига, иначе первый файл из папки
    app.rescan();
    let want = config::expand(&app.cfg.current);
    let first = if !app.cfg.current.is_empty() && want.is_file() {
        Some(want)
    } else {
        app.files.first().cloned()
    };
    match first {
        Some(p) => app.request(p),
        None => eprintln!(
            "iriswall: нет обоев — положи картинки в {} и выполни `iriswall next`",
            app.cfg.dir
        ),
    }
    app.setup_rotation();

    if WaylandSource::new(conn.clone(), event_queue)
        .insert(handle.clone())
        .is_err()
    {
        return Err("не удалось подключить wayland к циклу событий".into());
    }

    let res = event_loop.run(None, &mut app, |_| {});
    let _ = std::fs::remove_file(&sock);
    res.map_err(|e| e.to_string())?;
    Ok(())
}

fn client(cmd: &str) -> i32 {
    match UnixStream::connect(socket_path()) {
        Ok(mut s) => {
            let _ = writeln!(s, "{cmd}");
            let mut out = String::new();
            let _ = s.read_to_string(&mut out);
            print!("{out}");
            0
        }
        Err(_) => {
            eprintln!("демон не запущен (запусти: iriswall)");
            1
        }
    }
}

fn usage() {
    println!(
        "iriswall — демон обоев для Hyprland\n\n\
         iriswall [daemon]      запустить демон\n\
         iriswall next | prev   следующие / предыдущие обои из папки\n\
         iriswall back          вернуться к обоям, что были до этого (история)\n\
         iriswall random        случайные обои\n\
         iriswall set <файл>    поставить конкретную картинку\n\
         iriswall current       путь к текущим обоям\n\
         iriswall palette       палитра текущих обоев (JSON)\n\
         iriswall reload        перечитать конфиг и папку\n\
         iriswall effect <имя>  эффект: random, iris, sweep, ripple, mosaic, zoom, ink\n\
         iriswall status        показать состояние\n\
         iriswall quit          остановить демон"
    );
}

fn main() {
    mem::tune();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let code = match args.first().map(|s| s.as_str()) {
        None | Some("daemon") => match run_daemon() {
            Ok(_) => 0,
            Err(e) => {
                eprintln!("iriswall: {e}");
                1
            }
        },
        Some("-h") | Some("--help") | Some("help") => {
            usage();
            0
        }
        Some("set") => match args.get(1) {
            Some(p) => match std::fs::canonicalize(config::expand(p)) {
                Ok(abs) => client(&format!("set {}", abs.display())),
                Err(e) => {
                    eprintln!("{p}: {e}");
                    1
                }
            },
            None => {
                usage();
                1
            }
        },
        Some("effect") => match args.get(1) {
            Some(name) => client(&format!("effect {name}")),
            None => {
                usage();
                1
            }
        },
        Some(
            cmd @ ("next" | "prev" | "back" | "random" | "current" | "palette" | "reload"
            | "status" | "quit"),
        ) => client(cmd),
        Some(other) => {
            eprintln!("неизвестная команда: {other}\n");
            usage();
            1
        }
    };
    std::process::exit(code);
}
