use crate::keyboard::Keyboard;
use crate::mem::{self, ChangeMode, MapRegion, ProcInfo, ResultEntry, ValueType};
use egui::{
    vec2, Align, Button, CollapsingHeader, Color32,     FontData, FontDefinitions, FontFamily,
    FontTweak, Frame, Layout, RadioButton, RichText, ScrollArea, TextWrapMode,
};
use egui::epaint::text::VariationCoords;
use egui_extras::{Column, TableBuilder};
use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

// 純黑背景 (AMOLED); 按鈕/控制項的預設樣式原封不動, 只有底色改黑
const HEX_LEN: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Tab {
    Proc,
    Maps,
    Scan,
    Hex,
    Lua,
    Settings,
}

impl Tab {
    pub const ALL: [Tab; 6] = [Tab::Proc, Tab::Maps, Tab::Scan, Tab::Hex, Tab::Lua, Tab::Settings];

    fn label(self) -> &'static str {
        match self {
            Tab::Proc => "行程",
            Tab::Maps => "映射",
            Tab::Scan => "掃描",
            Tab::Hex => "十六進位制轉儲",
            Tab::Lua => "Lua",
            Tab::Settings => "設置",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shortcut {
    Screenshot,
}

impl Shortcut {
    pub const ALL: [Shortcut; 1] = [Shortcut::Screenshot];

    fn label(self) -> &'static str {
        match self {
            Shortcut::Screenshot => "截圖",
        }
    }

    fn enabled(self, state: &AppState) -> bool {
        match self {
            Shortcut::Screenshot => state.shortcut_screenshot,
        }
    }

    fn set_enabled(self, state: &mut AppState, value: bool) {
        match self {
            Shortcut::Screenshot => state.shortcut_screenshot = value,
        }
    }

    fn action(self) -> UiAction {
        match self {
            Shortcut::Screenshot => UiAction::CaptureScreenshot,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TypeSlot {
    S32,
    S64,
    F32,
    F64,
    Bool,
    Bit,
    U8,
    Ptr,
    S16,
}

impl TypeSlot {
    pub const MAIN: [TypeSlot; 8] = [
        TypeSlot::S32,
        TypeSlot::S64,
        TypeSlot::F32,
        TypeSlot::F64,
        TypeSlot::Bool,
        TypeSlot::Bit,
        TypeSlot::U8,
        TypeSlot::Ptr,
    ];

    pub const ADVANCED: [TypeSlot; 1] = [TypeSlot::S16];

    pub fn ty(self, signed: bool) -> ValueType {
        match self {
            TypeSlot::S32 => {
                if signed {
                    ValueType::Int32
                } else {
                    ValueType::UInt32
                }
            }
            TypeSlot::S64 => {
                if signed {
                    ValueType::Int64
                } else {
                    ValueType::UInt64
                }
            }
            TypeSlot::S16 => {
                if signed {
                    ValueType::Int16
                } else {
                    ValueType::UInt16
                }
            }
            TypeSlot::F32 => ValueType::Float32,
            TypeSlot::F64 => ValueType::Double,
            TypeSlot::Bool => ValueType::Bool,
            TypeSlot::Bit => ValueType::BoolBit,
            TypeSlot::U8 => ValueType::UInt8,
            TypeSlot::Ptr => ValueType::Ptr,
        }
    }

    pub fn label(self, signed: bool) -> &'static str {
        match self {
            TypeSlot::S32 => {
                if signed {
                    "i32"
                } else {
                    "u32"
                }
            }
            TypeSlot::S64 => {
                if signed {
                    "i64"
                } else {
                    "u64"
                }
            }
            TypeSlot::S16 => {
                if signed {
                    "i16"
                } else {
                    "u16"
                }
            }
            TypeSlot::F32 => "f32",
            TypeSlot::F64 => "f64(Double)",
            TypeSlot::Bool => "bool",
            TypeSlot::Bit => "Bit(Bitfield bool)",
            TypeSlot::U8 => "u8/enum",
            TypeSlot::Ptr => "Ptr",
        }
    }
}

pub struct AppState {
    pub visible: bool,
    pub revision: u64,
    pub driver_ok: bool,
    pub status: String,
    pub pid: i32,
    pub proc_name: String,
    pub module_base: u64,
    pub procs: Vec<ProcInfo>,
    pub maps: Vec<MapRegion>,
    pub map_checked: Vec<bool>,
    pub tab: Tab,
    pub scan_slot: TypeSlot,
    pub signed: bool,
    pub change_mode: ChangeMode,
    pub input: String,
    pub proc_query: String,
    pub show_system: bool,
    pub results: Vec<ResultEntry>,
    // 結果集世代: 每次清空/整体替換 +1, 靜默刷新用它判斷自己手裡的副本是不是陳的
    pub(crate) results_epoch: u64,
    // 位址 -> 選中時的原始值, refresh 後比對即得「自選中後變更」
    pub selected: HashMap<u64, u64>,
    // agent 正在看的位址: 最後一次 results 請求起累加, 結果替換時清空
    pub(crate) agent_view: HashSet<u64>,
    pub scanning: bool,
    pub hex_addr: u64,
    pub hex: Vec<u8>,
    pub events: Vec<egui::Event>,
    pub hit_rects: Vec<egui::Rect>,
    pub advanced_open: bool,
    pub type_open: bool,
    pub cond_open: bool,
    pub refresh_interval: f32,
    pub shortcut_screenshot: bool,
    pub screenshot_pending: bool,
    pub screenshot_result: Option<Result<String, String>>,
    pub close_confirm_open: bool,
    pub collapse_toggle: std::sync::atomic::AtomicBool,
    /// 狀態欄像素高度 (開機時讀一次；熱旋轉不更新)
    pub status_bar_h: i32,
}

impl AppState {
    pub fn new(show: bool) -> Self {
        let driver_ok = crate::nb9::probe();
        let status_bar_h = crate::display::status_bar_height().unwrap_or(0);
        let procs = mem::list_processes();
        let status = if driver_ok {
            format!("核心驅動已連線，行程 {} 個", procs.len())
        } else {
            "核心驅動未連線：ioctl 618 失敗".to_string()
        };
        Self {
            visible: show,
            revision: 1,
            driver_ok,
            status,
            pid: 0,
            proc_name: String::new(),
            module_base: 0,
            procs,
            maps: Vec::new(),
            map_checked: Vec::new(),
            tab: Tab::Proc,
            scan_slot: TypeSlot::S32,
            signed: true,
            change_mode: ChangeMode::ExactValue,
            input: String::new(),
            proc_query: String::new(),
            show_system: false,
            results: Vec::new(),
            results_epoch: 1,
            selected: HashMap::new(),
            agent_view: HashSet::new(),
            scanning: false,
            hex_addr: 0,
            hex: Vec::new(),
            events: Vec::new(),
            hit_rects: Vec::new(),
            advanced_open: false,
            type_open: true,
            cond_open: true,
            refresh_interval: 1.0,
            shortcut_screenshot: true,
            screenshot_pending: false,
            screenshot_result: None,
            close_confirm_open: std::env::var_os("THERAOHE_DBG_CLOSE").is_some(),
            collapse_toggle: std::sync::atomic::AtomicBool::new(false),
            status_bar_h,
        }
    }

    pub(crate) fn bump(&mut self) {
        self.revision = self.revision.wrapping_add(1);
    }

    pub(crate) fn refresh_procs(&mut self) {
        self.procs = mem::list_processes();
        self.status = format!("{} 個行程", self.procs.len());
        self.bump();
    }

    pub(crate) fn attach(&mut self, pid: i32) {
        let name = self
            .procs
            .iter()
            .find(|p| p.pid == pid)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| format!("pid {pid}"));
        self.pid = pid;
        self.proc_name = name;
        self.maps = mem::read_maps(pid);
        self.map_checked = self.maps.iter().map(is_scannable).collect();
        self.module_base = mem::main_module(&self.maps);
        self.results.clear();
        self.results_epoch += 1;
        self.selected.clear();
        self.agent_view.clear();
        self.input.clear();
        self.tab = Tab::Maps;
        if self.maps.is_empty() {
            self.status = format!("讀取 PID {pid} 的映射失敗（需要 root？）");
        } else {
            self.status.clear();
        }
        self.bump();
    }

    fn open_hex(&mut self, addr: u64) {
        self.hex_addr = addr;
        self.hex = mem::hex_dump(self.pid, addr, HEX_LEN);
        self.tab = Tab::Hex;
        if self.hex.is_empty() {
            self.status = format!("無法讀取 0x{addr:x}");
        } else {
            self.status = format!("轉儲 0x{addr:x}");
        }
        self.bump();
    }

    fn select_slot(&mut self, slot: TypeSlot) {
        self.scan_slot = slot;
        self.results.clear();
        self.results_epoch += 1;
        self.selected.clear();
        self.agent_view.clear();
        self.input.clear();
        self.status = format!("掃描類型：{}", slot.label(self.signed));
        self.bump();
    }

    fn set_signed(&mut self, signed: bool) {
        self.signed = signed;
        self.results.clear();
        self.results_epoch += 1;
        self.selected.clear();
        self.agent_view.clear();
        self.status = format!("掃描類型：{}", self.scan_slot.label(signed));
        self.bump();
    }

    fn set_change_mode(&mut self, mode: ChangeMode) {
        self.change_mode = mode;
        self.bump();
    }

    fn toggle_advanced(&mut self) {
        self.advanced_open = !self.advanced_open;
        self.bump();
    }

    fn toggle_type_open(&mut self) {
        self.type_open = !self.type_open;
        self.bump();
    }

    fn toggle_cond_open(&mut self) {
        self.cond_open = !self.cond_open;
        self.bump();
    }

    fn toggle_map(&mut self, idx: usize) {
        if let Some(v) = self.map_checked.get_mut(idx) {
            *v = !*v;
            self.bump();
        }
    }

    fn set_input(&mut self, text: String) {
        self.input = text;
        self.bump();
    }

    fn set_close_confirm_open(&mut self, open: bool) {
        if !open {
            self.events.clear();
        }
        self.close_confirm_open = open;
        self.bump();
    }

    fn set_proc_query(&mut self, text: String) {
        self.proc_query = text;
        self.bump();
    }

    fn toggle_result(&mut self, index: usize) {
        let Some(entry) = self.results.get(index) else {
            return;
        };
        let addr = entry.addr;
        if self.selected.remove(&addr).is_none() {
            self.selected.insert(addr, entry.bits);
        }
        self.status = format!("已選中 {} 項", self.selected.len());
        self.bump();
    }

    fn refresh_hex(&mut self) {
        self.hex = mem::hex_dump(self.pid, self.hex_addr, HEX_LEN);
        self.status = format!("轉儲 0x{:x}", self.hex_addr);
        self.bump();
    }
}

pub fn push_touch(
    app: &Arc<Mutex<AppState>>,
    id: egui::TouchId,
    phase: egui::TouchPhase,
    x: f32,
    y: f32,
) {
    let mut state = match app.lock() {
        Ok(v) => v,
        Err(_) => return,
    };
    if !state.visible {
        return;
    }
    let pos = egui::pos2(x, y);
    let mut events = Vec::with_capacity(3);
    events.push(egui::Event::Touch {
        device_id: egui::TouchDeviceId(0),
        id,
        phase,
        pos,
        force: None,
    });
    match phase {
        egui::TouchPhase::Start => {
            events.push(egui::Event::PointerMoved(pos));
            events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: egui::Modifiers::default(),
            });
        }
        egui::TouchPhase::Move => {
            events.push(egui::Event::PointerMoved(pos));
        }
        egui::TouchPhase::End => {
            events.push(egui::Event::PointerMoved(pos));
            events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: false,
                modifiers: egui::Modifiers::default(),
            });
            events.push(egui::Event::PointerGone);
        }
        egui::TouchPhase::Cancel => {
            events.push(egui::Event::PointerGone);
        }
    }
    state.events.extend(events);
    state.bump();
}

#[derive(Clone, Debug)]
pub(crate) enum UiAction {
    Tab(Tab),
    RefreshProcs,
    Attach(i32),
    MapBase,
    ToggleMap(usize),
    ScanSlot(TypeSlot),
    SetSigned(bool),
    ChangeMode(ChangeMode),
    SetInput(String),
    SetProcQuery(String),
    SetShowSystem(bool),
    SetRefreshInterval(f32),
    SetShortcutEnabled(Shortcut, bool),
    CaptureScreenshot,
    ToggleAdvanced,
    ToggleTypeOpen,
    ToggleCondOpen,
    StartScan,
    StartRefine,
    ToggleResultSelect(usize),
    RefreshHex,
    RequestClose,
    CancelClose,
    ConfirmClose,
}

pub(crate) fn apply_action(app: &Arc<Mutex<AppState>>, action: UiAction) {
    match action {
        UiAction::StartScan => {
            start_scan(app);
            return;
        }
        UiAction::StartRefine => {
            start_refine(app);
            return;
        }
        _ => {}
    }
    let mut state = match app.lock() {
        Ok(v) => v,
        Err(_) => return,
    };
    match action {
        UiAction::Tab(tab) => {
            state.tab = tab;
            state.bump();
        }
        UiAction::RefreshProcs => state.refresh_procs(),
        UiAction::Attach(pid) => state.attach(pid),
        UiAction::MapBase => {
            let base = state.module_base;
            if base != 0 {
                state.open_hex(base);
            } else {
                state.status = "尚未找到模組基底".to_string();
                state.bump();
            }
        }
        UiAction::ToggleMap(idx) => state.toggle_map(idx),
        UiAction::ScanSlot(slot) => state.select_slot(slot),
        UiAction::SetSigned(signed) => state.set_signed(signed),
        UiAction::ChangeMode(mode) => state.set_change_mode(mode),
        UiAction::SetInput(text) => state.set_input(text),
        UiAction::SetProcQuery(text) => state.set_proc_query(text),
        UiAction::SetShowSystem(v) => {
            state.show_system = v;
            state.bump();
        }
        UiAction::SetRefreshInterval(v) => {
            state.refresh_interval = v;
            state.bump();
        }
        UiAction::SetShortcutEnabled(shortcut, enabled) => {
            shortcut.set_enabled(&mut state, enabled);
            state.bump();
        }
        UiAction::CaptureScreenshot => {
            state.screenshot_pending = true;
            state.bump();
        }
        UiAction::ToggleAdvanced => state.toggle_advanced(),
        UiAction::ToggleTypeOpen => state.toggle_type_open(),
        UiAction::ToggleCondOpen => state.toggle_cond_open(),
        UiAction::ToggleResultSelect(index) => state.toggle_result(index),
        UiAction::RefreshHex => state.refresh_hex(),
        UiAction::RequestClose => state.set_close_confirm_open(true),
        UiAction::CancelClose => state.set_close_confirm_open(false),
        UiAction::ConfirmClose => {
            std::process::exit(0);
        }
        UiAction::StartScan | UiAction::StartRefine => {}
    }
}

fn selected_maps(state: &AppState) -> Vec<MapRegion> {
    if state.map_checked.iter().any(|&v| v) {
        state
            .maps
            .iter()
            .enumerate()
            .filter(|(i, _)| state.map_checked.get(*i).copied().unwrap_or(false))
            .map(|(_, m)| m.clone())
            .collect()
    } else {
        state.maps.clone()
    }
}

pub(crate) fn start_scan(app: &Arc<Mutex<AppState>>) {
    let job = {
        let mut state = match app.lock() {
            Ok(v) => v,
            Err(_) => return,
        };
        if state.scanning {
            return;
        }
        if state.pid <= 0 {
            state.status = "尚未附加行程".to_string();
            state.bump();
            return;
        }
        if state.maps.is_empty() {
            state.status = "沒有可掃描的映射".to_string();
            state.bump();
            return;
        }
        let ty = state.scan_slot.ty(state.signed);
        let bits = match ty.parse(&state.input) {
            Some(v) => v,
            None => {
                state.status = "掃描數值格式錯誤".to_string();
                state.bump();
                return;
            }
        };
        state.scanning = true;
        state.status = format!("掃描 {} = {} ...", ty.label(), ty.format(bits));
        state.bump();
        (state.pid, ty, bits, selected_maps(&state))
    };
    let app = Arc::clone(app);
    std::thread::spawn(move || {
        let found = mem::scan_ranges(job.0, job.1, job.2, &job.3);
        if let Ok(mut state) = app.lock() {
            state.scanning = false;
            state.status = format!("掃描完成，找到 {} 個結果", found.len());
            state.results = found;
            state.results_epoch += 1;
            state.selected.clear();
            state.agent_view.clear();
            state.bump();
        }
    });
}

pub(crate) fn start_refine(app: &Arc<Mutex<AppState>>) {
    let job = {
        let mut state = match app.lock() {
            Ok(v) => v,
            Err(_) => return,
        };
        if state.scanning {
            return;
        }
        if state.results.is_empty() {
            state.status = "沒有可篩選的結果".to_string();
            state.bump();
            return;
        }
        let ty = state.scan_slot.ty(state.signed);
        let delta = match state.change_mode {
            ChangeMode::ExactValue | ChangeMode::IncreasedBy | ChangeMode::DecreasedBy => {
                match ty.parse(&state.input) {
                    Some(v) => v,
                    None => {
                        state.status = "掃描數值格式錯誤".to_string();
                        state.bump();
                        return;
                    }
                }
            }
            _ => 0,
        };
        state.scanning = true;
        state.status = format!("篩選中（{}）...", state.change_mode.label());
        state.bump();
        (state.pid, ty, state.change_mode, delta, state.results.clone())
    };
    let app = Arc::clone(app);
    std::thread::spawn(move || {
        let found = mem::refine_entries(job.0, job.1, job.2, job.3, &job.4);
        if let Ok(mut state) = app.lock() {
            state.scanning = false;
            state.status = format!("篩選完成，剩 {} 個結果", found.len());
            state.results = found;
            state.results_epoch += 1;
            state.selected.clear();
            state.agent_view.clear();
            state.bump();
        }
    });
}

/// 靜默重新讀取結果（自動重新整理用），不寫入狀態訊息、不切換 scanning
fn refresh_results_silent(app: &Arc<Mutex<AppState>>) {
    let job = {
        let state = match app.lock() {
            Ok(v) => v,
            Err(_) => return,
        };
        if state.scanning {
            return;
        }
        if state.results.is_empty() {
            return;
        }
        let ty = state.scan_slot.ty(state.signed);
        (state.pid, ty, state.results.clone(), state.results_epoch)
    };
    let app = Arc::clone(app);
    std::thread::spawn(move || {
        let entries = mem::refresh_entries(job.0, job.1, &job.2);
        if let Ok(mut state) = app.lock() {
            // 副本陳化守護: 快照期間結果被清空/替換過, 這次的刷新直接作廢
            if state.results_epoch != job.3 {
                return;
            }
            state.results = entries;
            // 同一輪: 以實刷新結果, 不變動世代
            state.bump();
        }
    });
}

pub struct UiHost {
    ctx: egui::Context,
    renderer: EguiSoftwareRender,
    keyboard: Keyboard,
    start: Instant,
    last_rev: u64,
    last_visible: bool,
    last_size: (usize, usize),
    last_screen: (usize, usize),
    last_auto_refresh: Instant,
    layer_offset: (f32, f32),
    frames: u64,
}

impl UiHost {
    pub fn load(font_path: &str, font_index: Option<u32>) -> Result<Self, String> {
        let data = std::fs::read(font_path).map_err(|e| format!("read {font_path}: {e}"))?;
        let mut fonts = FontDefinitions::default();
        let index = font_index.unwrap_or(0);
        const GSC: &[u8] = include_bytes!("../assets/fonts/GoogleSansCode-VariableFont_MONO,wght.ttf");
        let noto = FontData::from_blob(Arc::new(data), index);
        let gsc_prop = FontData::from_static(GSC).tweak(FontTweak {
            coords: VariationCoords::new([(b"MONO", 0.0)]),
            ..Default::default()
        });
        let gsc_mono = FontData::from_static(GSC).tweak(FontTweak {
            coords: VariationCoords::new([(b"MONO", 1.0)]),
            ..Default::default()
        });
        fonts.font_data.insert("noto-cjk".to_owned(), Arc::new(noto));
        fonts.font_data.insert("google-sans-code-prop".to_owned(), Arc::new(gsc_prop));
        fonts.font_data.insert("google-sans-code-mono".to_owned(), Arc::new(gsc_mono));
        for (family, gsc) in [
            (FontFamily::Proportional, "google-sans-code-prop"),
            (FontFamily::Monospace, "google-sans-code-mono"),
        ] {
            fonts
                .families
                .entry(family)
                .or_default()
                .splice(0..0, [gsc.to_owned(), "noto-cjk".to_owned()]);
        }
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts);
        configure_style(&ctx);
        let renderer = EguiSoftwareRender::new(ColorFieldOrder::Rgba).with_caching(false);
        Ok(Self {
            ctx,
            renderer,
            keyboard: Keyboard::default(),
            start: Instant::now(),
            last_rev: 0,
            last_visible: false,
            last_size: (0, 0),
            last_screen: (0, 0),
            last_auto_refresh: Instant::now(),
            layer_offset: (0.0, 0.0),
            frames: 0,
        })
    }

    pub fn render(
        &mut self,
        app: &Arc<Mutex<AppState>>,
        buf: &mut [[u8; 4]],
        width: usize,
        height: usize,
        screen_w: usize,
        screen_h: usize,
    ) -> bool {
        if width == 0 || height == 0 || buf.len() < width * height {
            return false;
        }
        if screen_w == 0 || screen_h == 0 {
            return false;
        }
        let (revision, visible, events, scanning, want_capture, refresh_interval, results_len) = {
            let mut state = match app.lock() {
                Ok(v) => v,
                Err(_) => return false,
            };
            let want_capture = std::mem::take(&mut state.screenshot_pending);
            (
                state.revision,
                state.visible,
                std::mem::take(&mut state.events),
                state.scanning,
                want_capture,
                state.refresh_interval,
                state.results.len(),
            )
        };
        if want_capture {
            let canvas = buf[..width * height].to_vec();
            let offset = self.layer_offset;
            let app = Arc::clone(app);
            std::thread::spawn(move || {
                let res = crate::screenshot::capture_canvas(
                    &canvas,
                    width,
                    height,
                    (offset.0 as i32, offset.1 as i32),
                );
                match &res {
                    Ok(path) => {
                        let _ = std::process::Command::new("am")
                            .args(["broadcast", "-a", "android.intent.action.MEDIA_SCANNER_SCAN_FILE", "-d", &format!("file://{path}")])
                            .output();
                    }
                    Err(e) => eprintln!("[ui] screenshot failed: {e}"),
                }
                if let Ok(mut state) = app.lock() {
                    state.status = match &res {
                        Ok(path) => format!("截圖已保存 {path}"),
                        Err(e) => format!("截圖失敗: {e}"),
                    };
                    state.screenshot_result = Some(res);
                    state.bump();
                }
            });
        }

        if !visible {
            if self.last_visible {
                for px in buf[..width * height].iter_mut() {
                    *px = [0, 0, 0, 0];
                }
                self.last_visible = false;
                self.last_rev = revision;
                return true;
            }
            return false;
        }

        let auto_refresh_due = results_len > 0
            && !scanning
            && refresh_interval > 0.0
            && self.last_auto_refresh.elapsed().as_secs_f32() >= refresh_interval;
        let need = revision != self.last_rev
            || !self.last_visible
            || self.last_size != (width, height)
            || self.last_screen != (screen_w, screen_h)
            || !events.is_empty()
            || self.ctx.has_requested_repaint()
            || auto_refresh_due;
        if !need {
            return false;
        }

        self.last_visible = true;
        self.last_size = (width, height);
        self.last_screen = (screen_w, screen_h);
        self.last_rev = revision;

        if auto_refresh_due {
            self.last_auto_refresh = Instant::now();
            refresh_results_silent(app);
        }

        let spinner = if scanning {
            (self.start.elapsed().as_millis() as usize / 90) % 10
        } else {
            0
        };

        // pixels_per_point() 讀的是上一幀 pass 的 input；首幀沒有 pass，讀到 1.0，
        // raw 的 screen_rect 會比真實物理尺寸小一個 ppp 倍率，Resize 會把窗口撐爆並永久存下去。
        // native_pixels_per_point 固定為 1.0，直接從 options 讀 zoom 作為本幀的 ppp。
        let ppp = self.ctx.options(|o| o.zoom_factor);
        let raw = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                vec2(screen_w as f32 / ppp, screen_h as f32 / ppp),
            )),
            max_texture_side: Some(4096),
            time: Some(self.start.elapsed().as_secs_f64()),
            predicted_dt: 1.0 / 60.0,
            events,
            focused: true,
            ..Default::default()
        };

        let mut actions = Vec::new();
        let mut hit_rects: Vec<egui::Rect> = Vec::new();
        let out = {
            let state = match app.lock() {
                Ok(v) => v,
                Err(_) => return false,
            };
            self.ctx.run_pass(raw, |ctx| {
                self.keyboard.pump_events(ctx);
                draw_ui(ctx, &state, &mut actions, &mut hit_rects, spinner);
                if let Some(rect) = self.keyboard.show(ctx) {
                    hit_rects.push(rect.expand(8.0));
                }
            })
        };
        if let Ok(mut state) = app.lock() {
            state.hit_rects = hit_rects;
        }

        apply_actions(app, actions);

        for px in buf[..width * height].iter_mut() {
            *px = [0, 0, 0, 0];
        }
        let mut out = out;
        let primitives = self.ctx.tessellate(out.shapes, out.pixels_per_point);
        let mut buffer = BufferMutRef::new(buf, width, height);
        self.renderer.render(
            &mut buffer,
            &primitives,
            &out.textures_delta,
            out.pixels_per_point,
        );
        out.textures_delta.clear();
        self.frames += 1;
        true
    }

    pub fn context(&self) -> egui::Context {
        self.ctx.clone()
    }

    pub fn set_layer_offset(&mut self, x: f32, y: f32) {
        self.layer_offset = (x, y);
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }
}

fn configure_style(ctx: &egui::Context) {
    // 只改背景為純黑（AMOLED）；按鈕等控制項的預設樣式原封不動；主色改為 255,45,85
    // set_zoom_factor 會延遲到下一個 pass 才生效，且那次應用會用上一幀的 screen_rect
    // 做縮放換算；ctx 建立後第一幀沒有上一幀，會量到 egui 的默認 5000x5000，
    // 把窗口默認尺寸撐爆並永久 persist。直接寫 options，讓第一幀就拿到正確 ppp。
    ctx.options_mut(|o| o.zoom_factor = 2.0);
    ctx.set_theme(egui::ThemePreference::Dark);
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Color32::BLACK;
    visuals.window_fill = Color32::BLACK;
    visuals.window_shadow = egui::Shadow::NONE;
    visuals.selection.bg_fill = Color32::from_rgb(255, 45, 85);
    visuals.override_text_color = None;
    ctx.set_visuals(visuals);
}

pub(crate) fn apply_actions(app: &Arc<Mutex<AppState>>, actions: Vec<UiAction>) {
    for action in actions {
        apply_action(app, action);
    }
}

fn is_scannable(map: &MapRegion) -> bool {
    map.perms.contains('r') && map.perms.contains('w') && !map.path.starts_with("/dev/")
}

pub(crate) fn hex_bytes(bits: u64, size: usize) -> String {
    let mut s = String::new();
    for i in 0..size {
        if i > 0 {
            s.push(' ');
        }
        s.push_str(&format!("{:02x}", (bits >> (i * 8)) & 0xff));
    }
    s
}

fn format_size(bytes: u64) -> String {
    const KIB: f64 = 1024.0;
    const MIB: f64 = KIB * 1024.0;
    const GIB: f64 = MIB * 1024.0;
    if bytes >= GIB as u64 {
        format!("{:.1} GiB", bytes as f64 / GIB)
    } else if bytes >= MIB as u64 {
        format!("{:.1} MiB", bytes as f64 / MIB)
    } else if bytes >= KIB as u64 {
        format!("{} KiB", bytes / KIB as u64)
    } else {
        format!("{bytes} B")
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_string()
    } else {
        let mut out: String = text.chars().take(max.saturating_sub(3)).collect();
        out.push_str("...");
        out
    }
}

fn draw_ui(
    ctx: &egui::Context,
    state: &AppState,
    actions: &mut Vec<UiAction>,
    hit_rects: &mut Vec<egui::Rect>,
    spinner: usize,
) {
    hit_rects.clear();
    let screen = ctx.content_rect();
    // content_rect 是邏輯尺寸 (= 物理尺寸 / zoom), 窗口按邏輯比例取值, 物理比例不隨 zoom 變
    // 主 UI 尺寸不分方向都以短邊 90% 為邊長的正方形, 物理大小旋轉後不變
    let side = screen.width().min(screen.height());
    let main_w = (side * 0.90).max(280.0);
    let main_h = (side * 0.90).max(320.0);
    // 狀態欄高度按物理像素記錄, 佈局以邏輯座標, 需除以縮放
    let main_pos = egui::pos2(0.0, state.status_bar_h as f32 / ctx.pixels_per_point());
    let main_id = egui::Id::unique("theraohe-main");
    if state.collapse_toggle.swap(false, std::sync::atomic::Ordering::Relaxed) {
        let collapsing_id = main_id.with("collapsing");
        if let Some(mut collapsed) =
            egui::containers::collapsing_header::CollapsingState::load(ctx, collapsing_id)
        {
            collapsed.set_open(!collapsed.is_open());
            collapsed.store(ctx);
            ctx.request_repaint();
        }
    }
    let mut open_local = state.visible;
    let r = egui::Window::new("The Raohe")
        .collapsible(true)
        .open(&mut open_local)
        .id(main_id)
        .default_pos(main_pos)
        .default_size(vec2(main_w, main_h))
        .min_size(vec2(245.0, 280.0))
        .max_size(vec2(screen.width(), screen.height()))
        .constrain(true)
        .show(ctx, |ui| {
            draw_toolbar(ui, state, actions);

            // 全屏底栏（所有页面共享）— 必须先加入，才能橫跨整個窗口寬度
            egui::Panel::bottom("window_status_bar")
                .resizable(false)
                .show(ui, |ui| draw_bottom_bar(ui, state));

            let footer_labels: &[(&str, UiAction)] = match state.tab {
                Tab::Hex => &[
                    ("重新整理", UiAction::RefreshHex),
                    ("模組基底", UiAction::MapBase),
                ],
                _ => &[],
            };
            if !footer_labels.is_empty() {
                egui::Panel::bottom("window_footer")
                    .resizable(false)
                    .show(ui, |ui| {
                        ui.set_min_width(ui.available_width());
                        let gap = 8.0;
                        let bw = (ui.available_width() - gap * (footer_labels.len() as f32 - 1.0))
                            / footer_labels.len() as f32;
                        hrow(ui, |ui| {
                            ui.spacing_mut().item_spacing.x = gap;
                            for (label, action) in footer_labels {
                                if ui
                                    .add_sized([bw, ui.spacing().interact_size.y], Button::new(*label))
                                    .clicked()
                                {
                                    actions.push(action.clone());
                                }
                            }
                        });
                    });
            }

            // 掃描控制面板（仿工具列直屬佈局，全屏分屏）
            // 依內容區比例切換: 闊/高 > 1 用左右分屏, 否則上下分屏
            if state.tab == Tab::Scan {
                if ui.available_width() > ui.available_height() {
                    egui::Panel::left("scan-controls")
                        .resizable(true)
                        .default_size((ui.available_width() * 0.42).max(320.0))
                        .min_size(260.0)
                        .max_size(520.0)
                        .frame(Frame::NONE)
                        .show(ui, |ui| draw_scan_controls(ui, state, actions, spinner));
                } else {
                    egui::Panel::top("scan-controls")
                        .resizable(true)
                        .default_size((ui.available_height() * 0.55).max(200.0))
                        .min_size(160.0)
                        .max_size(420.0)
                        .frame(Frame::NONE)
                        .show(ui, |ui| draw_scan_controls(ui, state, actions, spinner));
                }
            }

            egui::CentralPanel::default().show(ui, |ui| match state.tab {
                Tab::Proc => draw_procs(ui, state, actions),
                Tab::Maps => draw_maps(ui, state, actions),
                Tab::Scan => draw_results(ui, state, actions),
                Tab::Hex => draw_hex(ui, state),
                Tab::Lua => draw_lua(ui, state),
                Tab::Settings => draw_settings(ui, state, actions),
            });
        });
    if !open_local {
        actions.push(UiAction::RequestClose);
    }
    if let Some(r) = r {
        hit_rects.push(r.response.rect.expand(8.0));
    }

    // 常駐小窗口: 依設置裡的捷徑清單排按鈕, 冇任何輸入
    let enabled: Vec<Shortcut> = Shortcut::ALL
        .into_iter()
        .filter(|s| s.enabled(state))
        .collect();
    if !enabled.is_empty() {
        let shortcut_fill = Color32::from_black_alpha(128);
        let mut shortcut_style = (*ctx.style_of(egui::Theme::Dark)).clone();
        // 整窗半透明黑, 標題行不單獨上色 → 沿用窗口底色
        shortcut_style.visuals.window_fill = shortcut_fill;
        shortcut_style.visuals.widgets.open.weak_bg_fill = Color32::TRANSPARENT;
        let window_frame = egui::Frame::window(&shortcut_style).shadow(egui::Shadow::NONE);
        if let Some(r) = egui::Window::new("捷徑")
            .id(egui::Id::unique("theraohe-shortcuts"))
            .collapsible(true)
            .resizable(true)
            .movable(true)
            .default_pos(egui::pos2(screen.width() - 150.0, screen.height() * 0.5))
            .order(egui::Order::Foreground)
            .frame(window_frame)
            .show(ctx, |ui| {
                let s = ui.style_mut();
                    s.visuals.widgets.open.weak_bg_fill = Color32::TRANSPARENT;
                    for widget in [
                        &mut s.visuals.widgets.inactive,
                        &mut s.visuals.widgets.hovered,
                        &mut s.visuals.widgets.active,
                    ] {
                        widget.weak_bg_fill = widget.weak_bg_fill.gamma_multiply(0.5);
                    }
                    for shortcut in &enabled {
                        if ui.button(RichText::new(shortcut.label()).size(20.0)).clicked() {
                            actions.push(shortcut.action());
                        }
                    }
            })
        {
            hit_rects.push(r.response.rect.expand(8.0));
        }
    }

    if state.close_confirm_open {
        let id = egui::Id::unique("theraohe-close-confirm");
        let modal = egui::Modal::new(id);
        let resp = modal.show(ctx, |ui| {
            ui.set_min_width(360.0);
            ui.heading("關閉 The Raohe");
            ui.add_space(8.0);
            ui.label("確定要關閉 The Raohe?");
            ui.add_space(16.0);
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if ui.add(Button::new("關閉")).clicked() {
                        actions.push(UiAction::ConfirmClose);
                    }
                    ui.add_space(8.0);
                    if ui.add(Button::new("取消")).clicked() {
                        actions.push(UiAction::CancelClose);
                    }
                });
            });
        });
        hit_rects.clear();
        hit_rects.push(resp.response.rect.expand(8.0));
        if resp.should_close() {
            actions.push(UiAction::CancelClose);
        }
    }
}

fn draw_toolbar(ui: &mut egui::Ui, state: &AppState, actions: &mut Vec<UiAction>) {
    let gap = 6.0;
    let n = Tab::ALL.len() as f32;
    let bw = (ui.available_width() - gap * (n - 1.0)) / n;
    hrow(ui, |ui| {
        ui.spacing_mut().item_spacing.x = gap;
        for tab in Tab::ALL {
            let selected = state.tab == tab;
            let button = Button::selectable(selected, RichText::new(tab.label()))
                .wrap_mode(TextWrapMode::Truncate);
            if ui
                .add_sized([bw, ui.spacing().interact_size.y], button)
                .clicked()
            {
                actions.push(UiAction::Tab(tab));
            }
        }
    });
}

fn hrow(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let w = ui.available_width();
    ui.allocate_ui_with_layout(
        egui::vec2(w, ui.spacing().interact_size.y),
        Layout::left_to_right(Align::Center),
        add,
    );
}

fn draw_procs(ui: &mut egui::Ui, state: &AppState, actions: &mut Vec<UiAction>) {
    let query = state.proc_query.trim().to_lowercase();
    let filtered: Vec<(String, i32)> = state
        .procs
        .iter()
        .filter(|p| {
            (state.show_system || !p.system)
                && (query.is_empty()
                    || p.name.to_lowercase().contains(&query)
                    || p.pid.to_string().contains(&query))
        })
        .map(|p| (p.name.clone(), p.pid))
        .collect();

    let mut text = state.proc_query.clone();
    let resp = ui.add(
        egui::TextEdit::singleline(&mut text)
            .hint_text("搜尋行程名稱或 PID")
            .char_limit(64)
            .desired_width(f32::INFINITY),
    );
    if resp.changed() {
        actions.push(UiAction::SetProcQuery(text));
    }
    ui.add_space(4.0);
    ui.horizontal(|ui| {
        let button = Button::selectable(state.show_system, "顯示系統行程");
        let w = (ui.available_width() - 8.0) / 2.0;
        if ui
            .add_sized([w, ui.spacing().interact_size.y], button)
            .clicked()
        {
            actions.push(UiAction::SetShowSystem(!state.show_system));
        }
        if ui
            .add_sized([w, ui.spacing().interact_size.y], Button::new("重新整理"))
            .clicked()
        {
            actions.push(UiAction::RefreshProcs);
        }
    });
    ui.add_space(4.0);
    ui.label(format!("顯示 {} / {}", filtered.len(), state.procs.len()));
    ui.add_space(4.0);
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if filtered.is_empty() {
                ui.label("沒有符合的行程");
                return;
            }
            let w = ui.available_width();
            ui.with_layout(Layout::top_down(Align::Min), |ui| {
                for (name, pid) in filtered {
                    let selected = state.pid == pid;
                    let text = format!("{name} | {pid}");
                    if ui
                        .add_sized([w, ui.spacing().interact_size.y], Button::selectable(selected, text))
                        .clicked()
                    {
                        actions.push(UiAction::Attach(pid));
                    }
                }
            });
        });
}

fn draw_maps(ui: &mut egui::Ui, state: &AppState, actions: &mut Vec<UiAction>) {
    if state.maps.is_empty() {
        ui.label("尚未附加行程");
        return;
    }
    let checked_bytes: u64 = state
        .maps
        .iter()
        .zip(state.map_checked.iter())
        .filter(|(_, checked)| **checked)
        .map(|(m, _)| m.end - m.start)
        .sum();
    ui.label(format!(
        "映射 {} 個，共 {}，不含 {} 個不可掃描段",
        state.maps.len(),
        format_size(checked_bytes),
        state.maps.len() - state.map_checked.iter().filter(|&&c| c).count()
    ));
    ui.spacing_mut().item_spacing.y = 4.0;
    let row_h = ui.spacing().interact_size.y;
    TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .vscroll(true)
        .drag_to_scroll(egui::scroll_area::DragScroll::Always)
        .column(Column::auto().at_most(36.0))
        .column(Column::exact(210.0))
        .column(Column::auto().at_most(150.0))
        .column(Column::auto().at_most(60.0))
        .column(Column::remainder())
        .header(20.0, |mut header| {
            header.col(|ui| {
                ui.label("");
            });
            header.col(|ui| {
                ui.label("位址範圍");
            });
            header.col(|ui| {
                ui.label("大小");
            });
            header.col(|ui| {
                ui.label("權限");
            });
            header.col(|ui| {
                ui.label("名稱");
            });
        })
        .body(|body| {
            body.rows(row_h, state.maps.len(), |mut row| {
                let idx = row.index();
                let map = &state.maps[idx];
                let scannable = is_scannable(map);
                row.col(|ui| {
                    let mut checked = state.map_checked.get(idx).copied().unwrap_or(false);
                    if ui
                        .add_enabled(scannable, egui::Checkbox::new(&mut checked, ""))
                        .changed()
                    {
                        actions.push(UiAction::ToggleMap(idx));
                    }
                });
                row.col(|ui| {
                    let text = format!("{:012x}-{:012x}", map.start, map.end);
                    ui.add(egui::Label::new(egui::RichText::new(text).monospace()).truncate());
                });
                row.col(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(format!("{}", map.end - map.start));
                    });
                });
                row.col(|ui| {
                    ui.horizontal(|ui| {
                        for c in map.perms.chars() {
                            let color = match c {
                                'r' => Color32::from_rgb(0x30, 0xd1, 0x58),
                                'w' => Color32::from_rgb(0xff, 0xd6, 0x0a),
                                'x' => Color32::from_rgb(0xff, 0x45, 0x3a),
                                _ => Color32::from_rgba_unmultiplied(235, 235, 245, 77),
                            };
                            ui.label(egui::RichText::new(c.to_string()).monospace().color(color));
                        }
                    });
                });
                row.col(|ui| {
                    ui.label(truncate(&map.path, 48));
                });
            });
        });
}

fn draw_type_grid(
    ui: &mut egui::Ui,
    state: &AppState,
    actions: &mut Vec<UiAction>,
    slots: &[TypeSlot],
) {
    ui.horizontal_wrapped(|ui| {
        for slot in slots {
            let selected = state.scan_slot == *slot;
            let button = Button::selectable(selected, slot.label(state.signed));
            if ui.add(button).clicked() {
                actions.push(UiAction::ScanSlot(*slot));
            }
        }
    });
}

fn draw_typed_input(
    ui: &mut egui::Ui,
    state: &AppState,
    actions: &mut Vec<UiAction>,
) {
    if state.scan_slot == TypeSlot::Bool || state.scan_slot == TypeSlot::Bit {
        let mut checked = state.input == "1";
        if ui.checkbox(&mut checked, "True").changed() {
            actions.push(UiAction::SetInput(if checked {
                "1".to_string()
            } else {
                "0".to_string()
            }));
        }
        return;
    }
    let mut text = state.input.clone();
    let resp = ui.add(
        egui::TextEdit::singleline(&mut text)
            .hint_text("點此輸入")
            .char_limit(32)
            .desired_width(f32::INFINITY),
    );
    if resp.changed() {
        actions.push(UiAction::SetInput(text));
    }
}

fn draw_scan_controls(
    ui: &mut egui::Ui,
    state: &AppState,
    actions: &mut Vec<UiAction>,
    spinner: usize,
) {
    let strip_h = ui.spacing().interact_size.y + 16.0;
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height((ui.available_height() - strip_h).max(80.0))
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());

            let type_section = CollapsingHeader::new("型別")
                .open(Some(state.type_open))
                .show(ui, |ui| {
                    draw_type_grid(ui, state, actions, &TypeSlot::MAIN);
                    ui.add_space(8.0);

                    ui.horizontal(|ui| {
                        if ui.add(RadioButton::new(state.signed, "有符號")).clicked() {
                            actions.push(UiAction::SetSigned(true));
                        }
                        if ui.add(RadioButton::new(!state.signed, "無符號")).clicked() {
                            actions.push(UiAction::SetSigned(false));
                        }
                    });
                    ui.add_space(8.0);

                    let advanced = CollapsingHeader::new("Advanced")
                        .open(Some(state.advanced_open))
                        .show(ui, |ui| {
                            draw_type_grid(ui, state, actions, &TypeSlot::ADVANCED);
                        });
                    if advanced.header_response.clicked() {
                        actions.push(UiAction::ToggleAdvanced);
                    }
                });
            if type_section.header_response.clicked() {
                actions.push(UiAction::ToggleTypeOpen);
            }

            let cond_section = CollapsingHeader::new("條件")
                .open(Some(state.cond_open))
                .show(ui, |ui| {
                    let bool_slot = state.scan_slot == TypeSlot::Bool || state.scan_slot == TypeSlot::Bit;
                    if bool_slot
                        && !matches!(
                            state.change_mode,
                            ChangeMode::ExactValue
                                | ChangeMode::AnyChange
                                | ChangeMode::Unchanged
                        )
                    {
                        actions.push(UiAction::ChangeMode(ChangeMode::ExactValue));
                    }

                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        condition_row(ui, state, actions, ChangeMode::ExactValue);
                        for mode in [
                            ChangeMode::AnyChange,
                            ChangeMode::Unchanged,
                            ChangeMode::Increased,
                            ChangeMode::Decreased,
                        ] {
                            if bool_slot
                                && !matches!(mode, ChangeMode::AnyChange | ChangeMode::Unchanged)
                            {
                                continue;
                            }
                            let selected = state.change_mode == mode;
                            let button = Button::selectable(selected, mode.label());
                            if ui.add(button).clicked() {
                                actions.push(UiAction::ChangeMode(mode));
                            }
                        }
                        if !bool_slot {
                            condition_row(ui, state, actions, ChangeMode::IncreasedBy);
                            condition_row(ui, state, actions, ChangeMode::DecreasedBy);
                        }
                    });
                });
            if cond_section.header_response.clicked() {
                actions.push(UiAction::ToggleCondOpen);
            }
            ui.add_space(4.0);
            if state.scanning {
                ui.label(format!("對中… {} 處理中", SPINNER[spinner]));
            }
        });

    egui::Panel::bottom("scan-buttons")
        .resizable(false)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let w = (ui.available_width() - 6.0) / 2.0;
                let refine_enabled = !state.results.is_empty();
                ui.add_enabled_ui(refine_enabled, |ui| {
                    if ui.add_sized([w, ui.spacing().interact_size.y], Button::new("篩選結果")).clicked()
                    {
                        actions.push(UiAction::StartRefine);
                    }
                });
                let scan_label = if state.results.is_empty() {
                    "首次掃描(依目標值)"
                } else {
                    "新一輪掃描(依目標值)"
                };
                if ui.add_sized([w, ui.spacing().interact_size.y], Button::new(scan_label)).clicked()
                {
                    actions.push(UiAction::StartScan);
                }
            });
        });
}

const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

fn condition_row(
    ui: &mut egui::Ui,
    state: &AppState,
    actions: &mut Vec<UiAction>,
    mode: ChangeMode,
) {
    let bool_slot = state.scan_slot == TypeSlot::Bool || state.scan_slot == TypeSlot::Bit;
    let label = mode.label();
    let selected = state.change_mode == mode;
    let show_input = matches!(
        mode,
        ChangeMode::ExactValue | ChangeMode::IncreasedBy | ChangeMode::DecreasedBy
    );
    // Frame::group 會直接 advance_cursor_after_rect，繞過換行佈局的協商；
    // 這裡手工量尺寸並 allocate_space，空間不足時才會整組換行，排法與 horizontal_wrapped 內的按鈕一致
    const MARGIN: f32 = 6.0;
    let (button_padding_x, interact_h, icon_spacing_x, icon_width, item_spacing_x) = {
        let s = ui.style();
        (
            s.spacing.button_padding.x,
            s.spacing.interact_size.y,
            s.spacing.icon_spacing,
            s.spacing.icon_width,
            s.spacing.item_spacing.x,
        )
    };
    let body_font = egui::TextStyle::Body.resolve(ui.style());
    let text_color = ui.visuals().text_color();
    let measure = |ui: &mut egui::Ui, text: &str| {
        ui.fonts_mut(|f| f.layout_no_wrap(text.to_string(), body_font.clone(), text_color).size().x)
    };
    let button_w = measure(ui, label) + 2.0 * button_padding_x;
    let input_w = if !show_input {
        0.0
    } else if bool_slot {
        measure(ui, "True") + icon_spacing_x + icon_width
    } else {
        let text_w = measure(ui, &state.input.clone());
        let hint_w = measure(ui, "點此輸入");
        text_w.max(hint_w).max(40.0) + 20.0
    };
    let inner_w = button_w + if show_input { item_spacing_x + input_w } else { 0.0 };
    let (_id, rect) = ui.allocate_space(vec2(inner_w + 2.0 * MARGIN, interact_h + 2.0 * MARGIN));
    // 等價於 Frame::group 的邊框
    let corner_radius = ui.visuals().widgets.noninteractive.corner_radius;
    let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
    ui.painter().rect_stroke(rect, corner_radius, stroke, egui::StrokeKind::Inside);
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink(MARGIN))
            .sense(egui::Sense::hover())
            .layout(Layout::left_to_right(Align::Center)),
    );
    content.spacing_mut().item_spacing.x = item_spacing_x;
    if content.add(Button::selectable(selected, label)).clicked() {
        actions.push(UiAction::ChangeMode(mode));
    }
    if show_input {
        if bool_slot {
            draw_typed_input(&mut content, state, actions);
        } else {
            let mut text = state.input.clone();
            let resp = content.add(
                egui::TextEdit::singleline(&mut text)
                    .hint_text("點此輸入")
                    .char_limit(32)
                    .desired_width(input_w),
            );
            if resp.changed() {
                actions.push(UiAction::SetInput(text));
            }
        }
    }
}

fn draw_results(ui: &mut egui::Ui, state: &AppState, actions: &mut Vec<UiAction>) {
    egui::Panel::bottom("scan-results-bottom")
        .resizable(false)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let theme = ui.visuals().selection.bg_fill;
            ui.label(egui::RichText::new("主題色=選中").background_color(theme));
        });

    if state.results.is_empty() {
        ui.label("尚無結果，請先附加行程並掃描");
        return;
    }
    ui.label(format!("{} 個結果", state.results.len()));
    let ty = state.scan_slot.ty(state.signed);
    let size = ty.size();
    let w = ui.available_width();
    let addr_w = (w * 0.36).min(200.0);
    let raw_w = (w * 0.30).min(260.0);
    // TableBuilder 的欄分隔線以父 ui 的 min_rect 為底, 而 Panel::show 會把底欄 rect
    // 併進父 min_rect, 分隔線會穿透底欄, 故表格包一層 scope 隔離
    ui.scope(|ui| {
        TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .vscroll(true)
        .sense(egui::Sense::click())
        .drag_to_scroll(egui::scroll_area::DragScroll::Always)
        .column(Column::initial(addr_w).at_least(40.0).clip(true))
        .column(Column::initial(raw_w).at_least(40.0).clip(true))
        .column(Column::remainder().at_least(40.0).clip(true))
        .header(20.0, |mut header| {
            header.col(|ui| {
                ui.label("位址");
            });
            header.col(|ui| {
                ui.label("原始內容");
            });
            header.col(|ui| {
                ui.label("解析值");
            });
        })
        .body(|body| {
            body.rows(24.0, state.results.len(), |mut row| {
                let idx = row.index();
                let entry = &state.results[idx];
                let addr = entry.addr;
                // 自選中後變更: 黃字
                let changed = state
                    .selected
                    .get(&addr)
                    .is_some_and(|&bits| bits != entry.bits);
                row.set_selected(
                    state.selected.contains_key(&addr) || state.agent_view.contains(&addr),
                );
                row.col(|ui| {
                    let text = format!("0x{:012x}", entry.addr);
                    let resp = ui.add(
                        egui::Label::new(egui::RichText::new(text).monospace())
                            .truncate()
                            .sense(egui::Sense::click()),
                    );
                    if resp.clicked() {
                        let copy = format!("0x{:x} = {}", entry.addr, ty.format(entry.bits));
                        ui.ctx().copy_text(copy);
                    }
                });
                row.col(|ui| {
                    let text = egui::RichText::new(hex_bytes(entry.bits, size)).monospace();
                    let text = if changed {
                        text.color(CHANGED_COLOR)
                    } else {
                        text
                    };
                    ui.add(egui::Label::new(text).truncate());
                });
                row.col(|ui| {
                    let text = egui::RichText::new(ty.format(entry.bits));
                    let text = if changed {
                        text.color(CHANGED_COLOR)
                    } else {
                        text
                    };
                    ui.add(egui::Label::new(text).truncate());
                });
                if row.response().clicked() {
                    actions.push(UiAction::ToggleResultSelect(idx));
                }
            });
        });
    });
}

const CHANGED_COLOR: Color32 = Color32::from_rgb(255, 200, 0);

fn draw_hex(ui: &mut egui::Ui, state: &AppState) {
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            if state.hex.is_empty() {
                ui.label("尚無資料");
                return;
            }
            for (row, chunk) in state.hex.chunks(16).enumerate() {
                let addr = state.hex_addr + (row * 16) as u64;
                let mut line = format!("{addr:012x}  ");
                for i in 0..16 {
                    if let Some(b) = chunk.get(i) {
                        line.push_str(&format!("{b:02x} "));
                    } else {
                        line.push_str("   ");
                    }
                }
                line.push(' ');
                for b in chunk {
                    line.push(if (0x20..=0x7e).contains(b) {
                        *b as char
                    } else {
                        '.'
                    });
                }
                ui.add(egui::Label::new(egui::RichText::new(line).monospace()).truncate());
            }
        });
}

fn draw_lua(ui: &mut egui::Ui, _state: &AppState) {
    ui.heading("Lua");
    ui.add_space(6.0);
    ui.label("Lua 支援尚未實裝。");
    ui.label("預留給腳本化的內存操作（讀寫、掃描、斷點）。");
}

fn draw_settings(ui: &mut egui::Ui, state: &AppState, actions: &mut Vec<UiAction>) {
    egui::CollapsingHeader::new("介面")
        .default_open(true)
        .show(ui, |ui| {
            settings_row(ui, "介面縮放", |ui| {
                let mut zoom = ui.ctx().zoom_factor();
                let changed = ui
                    .add(
                        egui::DragValue::new(&mut zoom)
                            .speed(0.02)
                            .range(0.5..=3.0)
                            .suffix("x"),
                    )
                    .changed();
                if changed {
                    ui.ctx().set_zoom_factor(zoom);
                }
            });
        });

    egui::CollapsingHeader::new("捷徑")
        .default_open(true)
        .show(ui, |ui| {
            settings_row(ui, "截圖", |ui| {
                let mut enabled = state.shortcut_screenshot;
                if ui.checkbox(&mut enabled, "").changed() {
                    actions.push(UiAction::SetShortcutEnabled(Shortcut::Screenshot, enabled));
                }
            });
        });

    egui::CollapsingHeader::new("掃描結果")
        .default_open(true)
        .show(ui, |ui| {
            settings_row(ui, "重新整理間隙", |ui| {
                let mut secs = state.refresh_interval;
                if ui
                    .add(
                        egui::DragValue::new(&mut secs)
                            .speed(0.1)
                            .range(0.1..=10.0)
                            .suffix("s"),
                    )
                    .changed()
                {
                    actions.push(UiAction::SetRefreshInterval(secs));
                }
            });
        });
}

fn settings_row(ui: &mut egui::Ui, label: &str, add_value: impl FnOnce(&mut egui::Ui)) {
    ui.horizontal(|ui| {
        ui.label(label);
        ui.with_layout(Layout::right_to_left(Align::Center), add_value);
    });
}

fn draw_bottom_bar(ui: &mut egui::Ui, state: &AppState) {
    let mut items: Vec<String> = Vec::new();
    if state.pid > 0 {
        items.push(format!("行程: {} | {}", state.proc_name, state.pid));
    } else {
        items.push("行程: -".to_string());
    }
    if state.module_base != 0 {
        items.push(format!("基底位址: 0x{:x}", state.module_base));
    }
    if !state.maps.is_empty() {
        let checked_bytes: u64 = state
            .maps
            .iter()
            .zip(state.map_checked.iter())
            .filter(|(_, checked)| **checked)
            .map(|(m, _)| m.end - m.start)
            .sum();
        items.push(format!(
            "映射: {} 個，共 {}",
            state.maps.len(),
            format_size(checked_bytes)
        ));
    }
    items.push(if state.driver_ok {
        "核心驅動已連接： RT-hook_ioctl".to_string()
    } else {
        "核心驅動未連接".to_string()
    });
    if !state.status.is_empty() {
        items.push(truncate(&state.status, 48));
    }
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 8.0;
        for item in &items {
            ui.add(Button::new(RichText::new(item)).sense(egui::Sense::hover()));
        }
    });
}
