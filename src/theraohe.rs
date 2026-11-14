use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::ptr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

mod console;
mod display;
mod keyboard;
mod lua_env;
// 可選外掛: 刪除 src/mcp.rs 並移除此行與 mcp::start 呼叫即完成卸載; UI 不依賴此模組
mod mcp;
mod maps;
mod mem;
mod nb9;
mod screenshot;
mod sf_keys;
mod sf_touch;
mod touch_targets;
mod ui;

#[repr(C, align(16))]
struct Sp {
    ptr: *mut c_void,
    _pad: [u8; 24],
}

#[repr(C, align(16))]
struct Opaque1024([u8; 1024]);

#[repr(C)]
struct ANativeWindowBuffer {
    width: i32,
    height: i32,
    stride: i32,
    format: i32,
    bits: *mut c_void,
    reserved: [u32; 6],
}

#[link(name = "dl")]
extern "C" {
    fn dlopen(filename: *const c_char, flag: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn dlerror() -> *mut c_char;
}

#[link(name = "android")]
extern "C" {
    fn ANativeWindow_setBuffersGeometry(
        window: *mut c_void,
        width: i32,
        height: i32,
        format: i32,
    ) -> i32;
    fn ANativeWindow_lock(
        window: *mut c_void,
        buffer: *mut ANativeWindowBuffer,
        dirty: *const c_void,
    ) -> i32;
    fn ANativeWindow_unlockAndPost(window: *mut c_void) -> i32;
}

type AChoreographerFrameCallback = unsafe extern "C" fn(i64, *mut c_void);

#[link(name = "android")]
extern "C" {
    fn ALooper_prepare(opts: c_int) -> *mut c_void;
    fn ALooper_pollOnce(
        timeout_millis: c_int,
        out_fd: *mut c_int,
        out_events: *mut c_int,
        out_data: *mut *mut c_void,
    ) -> c_int;
    fn AChoreographer_getInstance() -> *mut c_void;
    fn AChoreographer_postFrameCallback(
        choreographer: *mut c_void,
        callback: Option<AChoreographerFrameCallback>,
        data: *mut c_void,
    );
}

type FnVoidThis = unsafe extern "C" fn(*mut c_void);
type FnString8Ctor = unsafe extern "C" fn(*mut c_void, *const c_char);
type FnIncStrong = unsafe extern "C" fn(*mut c_void, *const c_void);
type FnCreateSurface = unsafe extern "C" fn(
    *mut c_void,
    *mut c_void,
    u32,
    u32,
    i32,
    i32,
    *mut c_void,
    *mut c_void,
    *mut u32,
) -> Sp;
type FnGetSurface = unsafe extern "C" fn(*mut c_void) -> Sp;
type FnSetLayer = unsafe extern "C" fn(*mut c_void, *mut c_void, i32) -> *mut c_void;
type FnSetTrustedOverlay = unsafe extern "C" fn(*mut c_void, *mut c_void, bool) -> *mut c_void;
type FnSetPosition = unsafe extern "C" fn(*mut c_void, *mut c_void, f32, f32) -> *mut c_void;
type FnApply = unsafe extern "C" fn(*mut c_void, bool, bool) -> i32;

unsafe fn sym(handle: *mut c_void, name: &str) -> *mut c_void {
    let c = CString::new(name).unwrap();
    dlerror();
    let p = dlsym(handle, c.as_ptr());
    if p.is_null() {
        let err = dlerror();
        let msg = if err.is_null() {
            "unknown".to_string()
        } else {
            CStr::from_ptr(err).to_string_lossy().into_owned()
        };
        eprintln!("[sf] dlsym failed: {name}: {msg}");
        std::process::exit(2);
    }
    p
}

unsafe fn cast<F: Copy>(p: *mut c_void) -> F {
    std::mem::transmute_copy(&p)
}

struct Args {
    width: i32,
    height: i32,
    x: f32,
    y: f32,
    hold: u64,
    offset: Option<usize>,
    name: String,
    skip: bool,
    show: bool,
    touch: bool,
    keys: bool,
    font: String,
    font_index: Option<u32>,
    screen_w: i32,
    screen_h: i32,
    touch_device: Option<String>,
    key_device: Option<String>,
}

fn parse_args() -> Args {
    let mut a = Args {
        width: 1080,
        height: 2400,
        x: 0.0,
        y: 0.0,
        hold: 0,
        offset: None,
        name: "theraohe".to_string(),
        skip: true,
        show: false,
        touch: true,
        keys: true,
        font: "/system/fonts/NotoSansCJK-Regular.ttc".to_string(),
        font_index: None,
        screen_w: 1080,
        screen_h: 2400,
        touch_device: None,
        key_device: None,
    };
    let argv: Vec<String> = std::env::args().collect();
    let mut i = 1;
    while i < argv.len() {
        let key = argv[i].as_str();
        let mut next = || {
            i += 1;
            if i >= argv.len() {
                eprintln!("[sf] missing value for {key}");
                std::process::exit(2);
            }
            argv[i].clone()
        };
        match key {
            "--w" => a.width = next().parse().unwrap(),
            "--h" => a.height = next().parse().unwrap(),
            "--x" => a.x = next().parse().unwrap(),
            "--y" => a.y = next().parse().unwrap(),
            "--hold" => a.hold = next().parse().unwrap(),
            "--offset" => a.offset = Some(next().parse().unwrap()),
            "--name" => a.name = next(),
            "--skip" => a.skip = true,
            "--no-skip" => a.skip = false,
            "--show" => a.show = true,
            "--no-touch" => a.touch = false,
            "--no-keys" => a.keys = false,
            "--font" => a.font = next(),
            "--font-index" => a.font_index = Some(next().parse().unwrap()),
            "--screen-w" => a.screen_w = next().parse().unwrap(),
            "--screen-h" => a.screen_h = next().parse().unwrap(),
            "--touch-device" => a.touch_device = Some(next()),
            "--key-device" => a.key_device = Some(next()),
            "--help" | "-h" => {
                eprintln!(
                    "usage: theraohe [--w 1080] [--h 2400] [--x 0] [--y 0] [--hold 0] [--offset N (default: auto-detect)] [--name theraohe] [--skip|--no-skip] [--show] [--no-touch] [--no-keys] [--font PATH] [--font-index N] [--screen-w 1080] [--screen-h 2400] [--touch-device PATH] [--key-device PATH]"
                );
                std::process::exit(0);
            }
            other => {
                eprintln!("[sf] unknown arg: {other}");
                std::process::exit(2);
            }
        }
        i += 1;
    }
    a
}

struct FrameCtx<'a> {
    choreographer: *mut c_void,
    window: *mut c_void,
    ui: &'a mut ui::UiHost,
    app: Arc<Mutex<ui::AppState>>,
    screen: Arc<Mutex<display::ScreenInfo>>,
    args_w: usize,
    args_h: usize,
    side: usize,
    full_clear: u8,
    started: Instant,
    deadline: Duration,
    canvas: Vec<[u8; 4]>,
    canvas_dims: (usize, usize),
    frames: u64,
    errors: u64,
    last_beat_time: Instant,
    last_beat_frames: u64,
    last_map_retry: Instant,
    done: bool,
}

impl FrameCtx<'_> {
    /// Track the visible screen size WITHOUT touching the layer or its buffers.
    /// The layer is a fixed side x side square created once (large enough for
    /// both orientations); resizing an existing buffer makes SurfaceFlinger
    /// rescale it into the old layer size, which stretches the UI.
    fn sync_screen(&mut self) {
        let info = match self.screen.lock() {
            Ok(v) => *v,
            Err(_) => return,
        };
        if info.width <= 0 || info.height <= 0 {
            return;
        }
        let w = (info.width as usize).min(self.side);
        let h = (info.height as usize).min(self.side);
        if (w, h) != (self.args_w, self.args_h) {
            if info.width as usize > self.side || info.height as usize > self.side {
                eprintln!(
                    "[sf] screen {}x{} exceeds layer side {}; clipped (restart to re-fit)",
                    info.width, info.height, self.side
                );
            }
            self.args_w = w;
            self.args_h = h;
            // Every buffer in the queue must get cleared margins + fresh content.
            self.full_clear = 3;
            eprintln!("[sf] visible area {w}x{h} (layer {0}x{0})", self.side);
        }
    }

    unsafe fn render_frame(&mut self) {
        // Lua Tick(): 每次垂直同步中斷都呼叫一次
        if let Ok(mut state) = self.app.lock() {
            // 附加成功(pid>0)但映射讀取失敗時, 反者掃描窗口結束後自動恢復, 否則 Lua 鏈路永久卡在 module
            if state.pid > 0 && state.maps.is_empty()
                && self.last_map_retry.elapsed() >= Duration::from_secs(2)
            {
                self.last_map_retry = Instant::now();
                let new_maps = maps::read_maps(state.pid);
                if !new_maps.is_empty() {
                    state.map_checked = new_maps.iter().map(maps::default_map_checked).collect();
                    state.maps = new_maps;
                    state.module_base = maps::main_module(&state.maps);
                    state.status.clear();
                    crate::lua_env::set_module_index(&state.maps);
                    eprintln!("[sf] maps 重試成功, 模組索引已重建");
                }
            }
            crate::lua_env::set_target_info(state.pid, state.module_base);
            for script in &mut state.lua_scripts {
                crate::lua_env::call_tick(script);
            }
            ui::apply_lua_cmds(&mut state);
        }
        self.sync_screen();
        let (cw, ch) = (self.args_w, self.args_h);
        if self.canvas_dims != (cw, ch) {
            self.canvas.clear();
            self.canvas.resize(cw * ch, [0, 0, 0, 0]);
            self.canvas_dims = (cw, ch);
        }
        let visible = self.app.lock().map(|state| state.visible).unwrap_or(false);
        let rendered = self.ui.render(&self.app, &mut self.canvas, cw, ch, cw, ch);

        // Nothing changed: keep the last posted buffer on screen.
        if !rendered && self.frames > 0 && self.full_clear == 0 {
            self.beat();
            return;
        }

        let mut buf = ANativeWindowBuffer {
            width: 0,
            height: 0,
            stride: 0,
            format: 0,
            bits: ptr::null_mut(),
            reserved: [0; 6],
        };
        let lr = ANativeWindow_lock(self.window, &mut buf, ptr::null());
        if lr != 0 || buf.bits.is_null() {
            self.errors += 1;
            if self.errors <= 5 {
                eprintln!("[sf] lock failed: {lr}, bits={:?}", buf.bits);
            }
            return;
        }

        let bw = buf.width.max(0) as usize;
        let bh = buf.height.max(0) as usize;
        let stride = if buf.stride > 0 { buf.stride as usize } else { bw };
        if self.full_clear > 0 {
            ptr::write_bytes(buf.bits as *mut u8, 0, bh * stride * 4);
            self.full_clear -= 1;
        }

        // Copy only the visible top-left area; the rest of the square stays transparent.
        let cols = cw.min(bw);
        let rows = ch.min(bh);
        let src =
            std::slice::from_raw_parts(self.canvas.as_ptr() as *const u8, self.canvas.len() * 4);
        let dst = std::slice::from_raw_parts_mut(buf.bits as *mut u8, bh * stride * 4);
        for row in 0..rows {
            let s = row * cw * 4;
            let d = row * stride * 4;
            dst[d..d + cols * 4].copy_from_slice(&src[s..s + cols * 4]);
        }

        let ur = ANativeWindow_unlockAndPost(self.window);
        if ur != 0 {
            self.errors += 1;
            if self.errors <= 5 {
                eprintln!("[sf] unlockAndPost failed: {ur}");
            }
        }
        self.frames += 1;
        if self.frames == 1 {
            eprintln!(
                "[sf] first frame posted: buffer {bw}x{bh} stride={stride} area={cw}x{ch} visible={visible} rendered={rendered}"
            );
        }
        self.beat();
    }

    fn beat(&mut self) {
        let elapsed = self.last_beat_time.elapsed();
        if elapsed >= Duration::from_secs(10) {
            let fps = (self.frames - self.last_beat_frames) as f64 / elapsed.as_secs_f64();
            eprintln!(
                "[sf] heartbeat: frames={} fps={:.1} errors={} ui_frames={}",
                self.frames,
                fps,
                self.errors,
                self.ui.frames()
            );
            self.last_beat_time = Instant::now();
            self.last_beat_frames = self.frames;
        }
    }
}

unsafe extern "C" fn frame_callback(_frame_time_ns: i64, data: *mut c_void) {
    if data.is_null() {
        return;
    }
    let ctx = &mut *(data as *mut FrameCtx);
    if ctx.started.elapsed() >= ctx.deadline {
        ctx.done = true;
        return;
    }
    ctx.render_frame();
    if !ctx.done && !ctx.choreographer.is_null() {
        AChoreographer_postFrameCallback(ctx.choreographer, Some(frame_callback), data);
    }
}

unsafe fn run_vsync(
    window: *mut c_void,
    ui: &mut ui::UiHost,
    app: Arc<Mutex<ui::AppState>>,
    screen: Arc<Mutex<display::ScreenInfo>>,
    args: &Args,
    side: usize,
) {
    ALooper_prepare(0);
    let choreographer = AChoreographer_getInstance();
    let started = Instant::now();
    let deadline = if args.hold == 0 {
        Duration::from_secs(86400 * 365)
    } else {
        Duration::from_secs(args.hold)
    };
    let initial = screen.lock().map(|s| *s).unwrap_or(display::ScreenInfo {
        width: args.width,
        height: args.height,
        rotation: 0,
    });
    let mut ctx = FrameCtx {
        choreographer,
        window,
        ui,
        app,
        screen,
        args_w: (initial.width.max(1) as usize).min(side),
        args_h: (initial.height.max(1) as usize).min(side),
        side,
        full_clear: 3,
        started,
        deadline,
        canvas: Vec::new(),
        canvas_dims: (0, 0),
        frames: 0,
        errors: 0,
        last_beat_time: started,
        last_beat_frames: 0,
        last_map_retry: started,
        done: false,
    };

    if choreographer.is_null() {
        eprintln!("[sf] AChoreographer unavailable, fallback 60Hz");
        ctx.choreographer = ptr::null_mut();
        while !ctx.done && ctx.started.elapsed() < ctx.deadline {
            frame_callback(0, &mut ctx as *mut FrameCtx as *mut c_void);
            if ctx.done {
                break;
            }
            std::thread::sleep(Duration::from_millis(16));
        }
    } else {
        AChoreographer_postFrameCallback(
            choreographer,
            Some(frame_callback),
            &mut ctx as *mut FrameCtx as *mut c_void,
        );
        let mut out_fd: c_int = 0;
        let mut out_events: c_int = 0;
        let mut out_data: *mut c_void = ptr::null_mut();
        while !ctx.done && ctx.started.elapsed() < ctx.deadline {
            ALooper_pollOnce(100, &mut out_fd, &mut out_events, &mut out_data);
        }
    }
    eprintln!("[sf] done: frames={}, errors={}", ctx.frames, ctx.errors);
}

// android_native_base_t { int magic; int version; ... } is the first member of
// ANativeWindow. magic = '_wnd', version = sizeof(ANativeWindow).
const ANATIVEWINDOW_MAGIC: u32 = 0x5f77_6e64;

unsafe fn find_native_window(surf: *const u8) -> Option<usize> {
    let mut off = 0usize;
    while off <= 256 {
        let magic = ptr::read_unaligned(surf.add(off) as *const u32);
        if magic == ANATIVEWINDOW_MAGIC {
            let version = ptr::read_unaligned(surf.add(off + 4) as *const i32);
            if (64..=1024).contains(&version) {
                eprintln!("[sf] ANativeWindow magic at +{off}, version(sizeof)={version}");
                return Some(off);
            }
        }
        off += 4;
    }
    None
}

unsafe fn dump_head(p: *const u8, len: usize) {
    for i in (0..len).step_by(8) {
        let v = ptr::read_unaligned(p.add(i) as *const u64);
        eprintln!("[sf]   +{i:3}: {v:#018x}");
    }
}

unsafe fn run(args: &Args) {
    let libgui = dlopen(CString::new("/system/lib64/libgui.so").unwrap().as_ptr(), 1);
    let libutils = dlopen(
        CString::new("/system/lib64/libutils.so").unwrap().as_ptr(),
        1,
    );
    if libgui.is_null() || libutils.is_null() {
        eprintln!("[sf] dlopen failed: libgui={libgui:?} libutils={libutils:?}");
        std::process::exit(2);
    }

    let string8_ctor: FnString8Ctor = cast(sym(libutils, "_ZN7android7String8C2EPKc"));
    let inc_strong: FnIncStrong = cast(sym(libutils, "_ZNK7android7RefBase9incStrongEPKv"));

    let client_ctor: FnVoidThis = cast(sym(libgui, "_ZN7android21SurfaceComposerClientC2Ev"));
    let lm_ctor: FnVoidThis = cast(sym(libgui, "_ZN7android3gui13LayerMetadataC2Ev"));
    let create_surface: FnCreateSurface = cast(sym(
        libgui,
        "_ZN7android21SurfaceComposerClient13createSurfaceERKNS_7String8EjjiiRKNS_2spINS_7IBinderEEENS_3gui13LayerMetadataEPj",
    ));
    let get_surface: FnGetSurface = cast(sym(libgui, "_ZN7android14SurfaceControl10getSurfaceEv"));

    let tx_ctor: FnVoidThis = cast(sym(
        libgui,
        "_ZN7android21SurfaceComposerClient11TransactionC2Ev",
    ));
    let tx_set_layer: FnSetLayer = cast(sym(
        libgui,
        "_ZN7android21SurfaceComposerClient11Transaction8setLayerERKNS_2spINS_14SurfaceControlEEEi",
    ));
    let tx_set_trusted: FnSetTrustedOverlay = cast(sym(
        libgui,
        "_ZN7android21SurfaceComposerClient11Transaction17setTrustedOverlayERKNS_2spINS_14SurfaceControlEEEb",
    ));
    let tx_set_position: FnSetPosition = cast(sym(
        libgui,
        "_ZN7android21SurfaceComposerClient11Transaction11setPositionERKNS_2spINS_14SurfaceControlEEEff",
    ));
    let tx_apply: FnApply = cast(sym(
        libgui,
        "_ZN7android21SurfaceComposerClient11Transaction5applyEbb",
    ));

    let client = Box::leak(Box::new(Opaque1024([0u8; 1024])));
    let mut name8 = Opaque1024([0u8; 1024]);
    let mut metadata = Opaque1024([0u8; 1024]);

    let client_ptr = client.0.as_mut_ptr() as *mut c_void;
    client_ctor(client_ptr);
    inc_strong(client_ptr, client_ptr as *const c_void);

    let name_c = CString::new(args.name.as_str()).unwrap();
    string8_ctor(name8.0.as_mut_ptr() as *mut c_void, name_c.as_ptr());
    lm_ctor(metadata.0.as_mut_ptr() as *mut c_void);

    let initial_screen = display::query().unwrap_or(display::ScreenInfo {
        width: args.width,
        height: args.height,
        rotation: 0,
    });
    eprintln!(
        "[sf] display: {}x{} rotation={}",
        initial_screen.width, initial_screen.height, initial_screen.rotation
    );
    let screen = Arc::new(Mutex::new(initial_screen));
    // One square layer, big enough for both orientations; never resized afterwards.
    let side = initial_screen.width.max(initial_screen.height).max(1);

    let mut parent: usize = 0;
    let flags = if args.skip { 0x40 } else { 0 };
    let sc_sp = create_surface(
        client_ptr,
        name8.0.as_mut_ptr() as *mut c_void,
        side as u32,
        side as u32,
        1,
        flags,
        &mut parent as *mut usize as *mut c_void,
        metadata.0.as_mut_ptr() as *mut c_void,
        ptr::null_mut(),
    );
    let sc = sc_sp.ptr;
    if sc.is_null() {
        eprintln!("[sf] createSurface returned null");
        std::process::exit(3);
    }
    eprintln!("[sf] SurfaceControl = {sc:?}");

    let mut sc_ref: usize = sc as usize;
    let sc_arg = &mut sc_ref as *mut usize as *mut c_void;

    let mut tx = Opaque1024([0u8; 1024]);
    tx_ctor(tx.0.as_mut_ptr() as *mut c_void);
    tx_set_trusted(tx.0.as_mut_ptr() as *mut c_void, sc_arg, true);
    tx_set_layer(tx.0.as_mut_ptr() as *mut c_void, sc_arg, i32::MAX);
    let r = tx_apply(tx.0.as_mut_ptr() as *mut c_void, false, true);
    eprintln!("[sf] trusted/layer apply = {r}");

    let mut tx2 = Opaque1024([0u8; 1024]);
    tx_ctor(tx2.0.as_mut_ptr() as *mut c_void);
    tx_set_position(tx2.0.as_mut_ptr() as *mut c_void, sc_arg, args.x, args.y);
    let r2 = tx_apply(tx2.0.as_mut_ptr() as *mut c_void, true, false);
    eprintln!("[sf] position apply = {r2}");

    let surf_sp = get_surface(sc);
    let surf = surf_sp.ptr;
    if surf.is_null() {
        eprintln!("[sf] getSurface returned null");
        std::process::exit(4);
    }
    let (window, offset) = match args.offset {
        Some(off) => ((surf as usize + off) as *mut c_void, off),
        None => match find_native_window(surf as *const u8) {
            Some(off) => ((surf as usize + off) as *mut c_void, off),
            None => {
                eprintln!("[sf] ANativeWindow magic not found in Surface; dump:");
                dump_head(surf as *const u8, 128);
                eprintln!("[sf] pass --offset N manually");
                std::process::exit(5);
            }
        },
    };
    eprintln!(
        "[sf] Surface = {surf:?}, ANativeWindow = {window:?} (offset {offset}, {})",
        if args.offset.is_some() { "manual" } else { "auto" }
    );

    let g = ANativeWindow_setBuffersGeometry(
        window,
        side,
        side,
        1,
    );
    eprintln!("[sf] setBuffersGeometry = {g}");
    if g != 0 {
        eprintln!("[sf] geometry failed at offset {offset}; try --offset N");
        std::process::exit(5);
    }

    let mut ui = match ui::UiHost::load(&args.font, args.font_index) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("[sf] font load failed: {e}");
            std::process::exit(6);
        }
    };
    ui.set_layer_offset(args.x, args.y);

    let app = Arc::new(Mutex::new(ui::AppState::new(args.show)));
    {
        let state = app.lock().unwrap();
        eprintln!(
            "[sf] ready: driver={} procs={} skip={} show={} touch={} keys={}",
            state.driver_ok,
            state.procs.len(),
            args.skip,
            args.show,
            args.touch,
            args.keys
        );
    }

    touch_targets::set_layer_offset(args.x, args.y);

    if args.touch {
        sf_touch::start(
            Arc::clone(&app),
            Arc::clone(&screen),
            args.x,
            args.y,
            args.touch_device.clone(),
            ui.context(),
        );
    }
    if args.keys {
        sf_keys::start(Arc::clone(&app), args.key_device.clone());
    }

    mcp::start(Arc::clone(&app), 8080);
    display::start_watch(Arc::clone(&screen));
    run_vsync(window, &mut ui, app, screen, args, side as usize);
}

fn main() {
    // 首行執行, 截獲 stdout/stderr 供終端台窗口顯示
    console::init();
    let args = parse_args();
    unsafe { run(&args) }
}
