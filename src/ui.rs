use crate::keyboard::Keyboard;
use crate::lua_env;
use crate::maps::{self, MapRegion};
use crate::mem::{self, ChangeMode, ProcInfo, ResultEntry, ValueType};
use egui::{
    vec2, Align, Button, CollapsingHeader, Color32,     FontData, FontDefinitions, FontFamily,
    FontTweak, Frame, Layout, RadioButton, RichText, ScrollArea, TextWrapMode,
};
use egui::epaint::text::VariationCoords;
use egui_extras::{Column, TableBuilder};
use egui_software_backend::{BufferMutRef, ColorFieldOrder, EguiSoftwareRender};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::Instant;

// 純黑背景 (AMOLED); 按鈕/控制項的預設樣式原封不動, 只有底色改黑
const HEX_LEN: usize = 4096;

/// 大組 header 回傳給外層的事件, 避開借用檢查
#[derive(Default)]
struct GroupFlags {
    start_rename: bool,
    exit_rename: bool,
    delete: bool,
    toggle_free_position: bool,
    add_row: bool,
    delete_rows: Vec<usize>,
}

/// Sentinel used as a drop index: append at the very end.
const SHORTCUT_END: usize = usize::MAX;

/// Location of a bar inside a 大組.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct RowLocation {
    group: usize,
    row: usize,
}

/// Location of an item inside a bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ChipLocation {
    group: usize,
    row: usize,
    chip: usize,
}

/// Unified payload for the vertical (bar) and horizontal (chip) dnd,
/// so a payload that reaches the "wrong" handler is never silently discarded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DragPayload {
    /// A whole bar — the vertical dnd.
    Row(RowLocation),
    /// An item inside a bar — the horizontal dnd.
    Chip(ChipLocation),
}

/// 捷徑大組: 一個項目是 (label) 一個字串; 每個大組會以獨立懸浮窗呈現 N×M 按鈕
#[derive(Clone, PartialEq, Eq)]
pub struct ShortcutGroup {
    /// Stable identity for window id; survives renames and re-sorting.
    pub id: u64,
    /// Name shown as the CollapsingHeader label; also used as the window title.
    pub name: String,
    /// rows -> items; rows reorder vertically, items reorder horizontally inside each row.
    pub rows: Vec<Vec<String>>,
    /// 可否自由移動此懸浮窗; false = 鎖位置不可拖動
    pub free_position: bool,
}

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
            Tab::Hex => "Hex Dump",
            Tab::Lua => "Lua",
            Tab::Settings => "設置",
        }
    }
}

/// 捷徑內建候補動作 (Lua 反射出的函式在 shortcut_sync 時自動加入候選區)
pub const SHORTCUT_BUILTIN: [&str; 2] = ["截圖", "終端台"];

impl ShortcutGroup {
    fn new(id: u64, name: &str) -> Self {
        ShortcutGroup {
            id,
            name: name.to_owned(),
            rows: Vec::new(),
            free_position: true,
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
    Range,
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

    pub const ADVANCED: [TypeSlot; 2] = [TypeSlot::S16, TypeSlot::Range];

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
            TypeSlot::Range => ValueType::Range,
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
            TypeSlot::Range => "256B",
            TypeSlot::Bool => "bool",
            TypeSlot::Bit => "Bit(Bitfield bool)",
            TypeSlot::U8 => "u8/enum",
            TypeSlot::Ptr => "Ptr",
        }
    }

    pub fn is_interval(self) -> bool {
        matches!(self, TypeSlot::Range)
    }

    pub fn interval_block_bytes(self) -> Option<u64> {
        match self {
            TypeSlot::Range => Some(256),
            _ => None,
        }
    }
}

const FUZZY_SCAN_BUFFER: u64 = 1 << 20;
const FUZZY_RECORD_BYTES: u64 = 16;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum StructViewKind {
    None,
    Vector3,
    Rotator,
}

impl StructViewKind {
    const ALL: [StructViewKind; 3] = [
        StructViewKind::None,
        StructViewKind::Vector3,
        StructViewKind::Rotator,
    ];

    fn label(self) -> &'static str {
        match self {
            StructViewKind::None => "None",
            StructViewKind::Vector3 => "Vector(3)",
            StructViewKind::Rotator => "Rotator",
        }
    }

    fn axis_labels(self) -> [&'static str; 3] {
        match self {
            StructViewKind::None | StructViewKind::Vector3 => ["X", "Y", "Z"],
            StructViewKind::Rotator => ["Pitch", "Yaw", "Roll"],
        }
    }

    fn axis_prefixes(self) -> [&'static str; 3] {
        match self {
            StructViewKind::None | StructViewKind::Vector3 => ["X", "Y", "Z"],
            StructViewKind::Rotator => ["P", "Y", "R"],
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ResultsSource {
    Empty,
    Typed,
    BlockConverted,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Authority {
    Typed,
    Block,
}

impl Authority {
    pub(crate) fn label(self) -> &'static str {
        match self {
            Authority::Typed => "常規",
            Authority::Block => "分塊",
        }
    }
}

// 常規/分塊兩種結果合一; 權威源標記誰是不可替代的資料源
// 權威源僅在掃描/篩選時翻轉; 常規與分塊的互轉只是投影, 不翻轉權威源,
// epoch 決定目標側快取是否仍新鮮, 反覆切換不搬移資料
pub(crate) struct ScanResults {
    pub authority: Authority,
    pub typed: Vec<ResultEntry>,
    pub blocks: Option<mem::FuzzyBase>,
    pub(crate) data_epoch: u64,
    pub(crate) typed_epoch: u64,
    pub(crate) blocks_epoch: u64,
}

impl ScanResults {
    fn new() -> Self {
        Self {
            authority: Authority::Typed,
            typed: Vec::new(),
            blocks: None,
            data_epoch: 1,
            typed_epoch: 1,
            blocks_epoch: 1,
        }
    }

    // 權威側被掃描/篩選/清空寫入: 翻轉權威源並使投影快取失效
    fn replace_typed(&mut self, entries: Vec<ResultEntry>) {
        self.typed = entries;
        self.authority = Authority::Typed;
        self.data_epoch += 1;
        self.typed_epoch = self.data_epoch;
    }

    fn replace_blocks(&mut self, base: Option<mem::FuzzyBase>) {
        self.blocks = base;
        self.authority = Authority::Block;
        self.data_epoch += 1;
        self.blocks_epoch = self.data_epoch;
    }

    fn clear(&mut self) {
        self.typed.clear();
        self.blocks = None;
        self.authority = Authority::Typed;
        self.data_epoch += 1;
        self.typed_epoch = self.data_epoch;
        self.blocks_epoch = self.data_epoch;
    }

    // 目標=分塊: 快取失效時從 typed 重新推導對齊視圖
    fn ensure_blocks(&mut self, pid: i32, maps: &[MapRegion], block_bytes: u64) {
        if self.blocks_epoch == self.data_epoch {
            return;
        }
        self.blocks = if self.typed.is_empty() {
            None
        } else {
            Some(mem::blocks_from_entries(pid, maps, &self.typed, block_bytes))
        };
        self.blocks_epoch = self.data_epoch;
    }

    // 目標=常規: 快取失效時從 blocks 展開
    // Ok(true) = 本次展開(新資料, 呼叫端需更新 results_epoch 等); Ok(false) = 快取新鮮
    // Err(()) = 權威為分塊但 blocks 已告罄, 無可顯示
    fn ensure_typed(&mut self, pid: i32, ty: ValueType) -> Result<bool, ()> {
        if self.typed_epoch == self.data_epoch {
            return Ok(false);
        }
        match &self.blocks {
            Some(_) => {
                let base = self.blocks.as_ref().expect("checked above");
                let (entries, _) = mem::fuzzy_to_entries(pid, base, ty);
                self.typed = entries;
                self.typed_epoch = self.data_epoch;
                Ok(true)
            }
            None => Err(()),
        }
    }
}

#[derive(Clone)]
pub(crate) struct StructWindow {
    pub ty: ValueType,
    pub anchor: usize,
    pub epoch: u64,
    pub values: [Option<u64>; 3],
    at: Instant,
}

pub struct AppState {
    pub visible: bool,
    pub revision: u64,
    pub driver_ok: bool,
    pub status: String,
    pub pid: i32,
    pub proc_name: String,
    pub module_base: u64,
    pub last_scan_bytes: Option<u64>,
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
    pub(crate) scan: ScanResults,
    // 結果集世代: 每次清空/整体替換 +1, 靜默刷新用它判斷自己手裡的副本是不是陳的
    pub(crate) results_epoch: u64,
    pub(crate) results_source: ResultsSource,
    // 選中的位址 (Agent 瀏覽 results 與人類點選共用), 結果被清空/替換時重置
    pub selected: HashSet<u64>,
    // 預覽輪詢到的最新值 (display-only); results 本體不可變, 兩者差即「自上次掃描後變更」
    pub(crate) live_bits: HashMap<u64, u64>,
    pub struct_view_kind: StructViewKind,
    pub struct_anchor: usize,
    pub struct_open: bool,
    pub(crate) struct_cache: HashMap<u64, StructWindow>,
    pub(crate) struct_pending: HashMap<u64, u64>,
    pub(crate) struct_epoch: u64,
    pub range_min: String,
    pub range_max: String,
    pub scanning: bool,
    pub hex_addr: u64,
    pub hex: Vec<u8>,
    pub events: Vec<egui::Event>,
    pub hit_rects: Vec<egui::Rect>,
    pub advanced_open: bool,
    pub type_open: bool,
    pub cond_open: bool,
    pub refresh_interval: f32,
    /// hex 頁專用的重新整理間隙, 與掃描結果的 refresh_interval 各自獨立
    pub hex_refresh_interval: f32,
    /// 待交給鍵盤/剪切板組件的複製內容; 組件取得後清空
    pub clip_pending: Option<String>,
    /// hex 頁頂部的位址跳轉滑塊值(選擇行程時自動塡入主模組基座位址)
    pub hex_addr_input: f64,
    /// hex 頁自動重新整理開關
    pub hex_autorefresh: bool,
    // 捷徑: 每個大組是一個獨立懸浮窗, 內按鈕呈 N×M 填滿; 候選區為未配置的截圖/終端台/反射 Lua 函式
    pub shortcut_groups: Vec<ShortcutGroup>,
    pub shortcut_trailing: Vec<String>,
    pub shortcut_renaming: Option<usize>,
    pub shortcut_rename_focus: bool,
    pub shortcut_id_next: u64,
    /// 上一幀主按鈕按住的捷徑清單 (Lua 標籤), 每幀由 render() 重填; render_frame 在 Lua Tick 時消費
    pub shortcut_held: Vec<String>,
    pub console_open: bool,
    /// 每腳本一個獨立 Lua 狀態; 載入時與每次呼叫/寫回後重算反射
    pub lua_scripts: Vec<lua_env::LuaScript>,
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
        let mut state = Self {
            visible: show,
            revision: 1,
            driver_ok,
            status,
            pid: 0,
            proc_name: String::new(),
            module_base: 0,
            last_scan_bytes: None,
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
            scan: ScanResults::new(),
            results_epoch: 1,
            results_source: ResultsSource::Empty,
            selected: HashSet::new(),
            live_bits: HashMap::new(),
            struct_view_kind: StructViewKind::None,
            struct_anchor: 0,
            struct_open: true,
            struct_cache: HashMap::new(),
            struct_pending: HashMap::new(),
            struct_epoch: 1,
            range_min: String::new(),
            range_max: String::new(),
            scanning: false,
            hex_addr: 0,
            hex: Vec::new(),
            events: Vec::new(),
            hit_rects: Vec::new(),
            advanced_open: false,
            type_open: true,
            cond_open: true,
            refresh_interval: 0.25,
            hex_refresh_interval: 0.25,
            hex_addr_input: 0.0,
            hex_autorefresh: false,
            clip_pending: None,
            console_open: false,
            lua_scripts: lua_env::load_all(),
            shortcut_groups: vec![ShortcutGroup::new(1, "捷徑 1")],
            shortcut_trailing: Vec::new(),
            shortcut_renaming: None,
            shortcut_rename_focus: false,
            shortcut_id_next: 2,
            shortcut_held: Vec::new(),
            screenshot_pending: false,
            screenshot_result: None,
            close_confirm_open: std::env::var_os("THERAOHE_DBG_CLOSE").is_some(),
            collapse_toggle: std::sync::atomic::AtomicBool::new(false),
            status_bar_h,
        };
        // 此時 egui 宿主已建立、行程表已掃描完: 自動呼叫各腳本 Begin(), 再排空 Lua 發出的命令
        lua_env::set_target_info(0, 0);
        lua_env::set_module_index(&state.maps);
        for script in &mut state.lua_scripts {
            lua_env::call_begin(script);
        }
        apply_lua_cmds(&mut state);
        state
    }

    fn reset_struct_view(&mut self) {
        self.struct_cache.clear();
        self.struct_pending.clear();
        self.struct_epoch = self.struct_epoch.wrapping_add(1);
    }

    pub(crate) fn clear_results(&mut self) {
        self.scan.clear();
        self.results_epoch += 1;
        self.selected.clear();
        self.live_bits.clear();
        self.reset_struct_view();
        self.results_source = ResultsSource::Empty;
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
        self.maps = maps::read_maps(pid);
        lua_env::set_module_index(&self.maps);
        self.map_checked = self.maps.iter().map(maps::default_map_checked).collect();
        self.module_base = maps::main_module(&self.maps);
        // 選擇行程時自動塡入主模組基座位址作爲跳轉預設值
        self.hex_addr_input = self.module_base as f64;
        self.last_scan_bytes = None;
        self.clear_results();
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
        if self.scan_slot == slot {
            return;
        }
        if slot.is_interval() {
            // 目標=分塊: 快取失效(權威源被掃描/篩選動過)才重推導; 常規側原封不動
            let block_bytes = slot.interval_block_bytes().expect("slot is interval");
            self.scan.ensure_blocks(self.pid, &self.maps, block_bytes);
            self.scan_slot = slot;
            self.bump();
            return;
        }
        let target_family_changed = self.scan_slot.is_interval();
        if target_family_changed {
            // 目標=常規, 源=分塊: 快取失效時展開; 權威為分塊但 blocks 告罄則清空
            match self.scan.ensure_typed(self.pid, slot.ty(self.signed)) {
                Ok(true) => {
                    // 分塊權威展開的是新資料: 整批替換常規側, 源篩選結果不丟
                    self.results_source = ResultsSource::BlockConverted;
                    self.results_epoch += 1;
                    self.selected.clear();
                    self.live_bits.clear();
                    self.reset_struct_view();
                    self.input.clear();
                }
                Ok(false) => {}
                Err(()) => {
                    // 分塊權威下 blocks 已被篩光: 常規側無可顯示
                    self.scan.typed.clear();
                    self.scan.typed_epoch = self.scan.data_epoch;
                    self.results_source = ResultsSource::Empty;
                    self.results_epoch += 1;
                    self.selected.clear();
                    self.live_bits.clear();
                    self.reset_struct_view();
                }
            }
            self.scan_slot = slot;
            self.bump();
            return;
        }
        // 跨入 Bit: 每位元一行; 跨出 Bit: 同位址多行併一
        if matches!(slot, TypeSlot::Bit) != matches!(self.scan_slot, TypeSlot::Bit) {
            let ty = slot.ty(self.signed);
            self.refit_bits(ty);
        }
        self.scan_slot = slot;
        self.bump();
    }

    /// 跨 Bit 邊界後舊結果語意不可沿用: 依位址重讀, 跨入展開為每位元一行,
    /// 跨出同位址多行併一, 已篩出的候選集合保留
    fn refit_bits(&mut self, ty: ValueType) {
        let pid = self.pid;
        let mut addrs: Vec<u64> = self.scan.typed.iter().map(|e| e.addr).collect();
        addrs.sort_unstable();
        addrs.dedup();
        let mut out = Vec::with_capacity(addrs.len());
        if ty == ValueType::BoolBit {
            for addr in addrs {
                if let Some(v) = crate::nb9::read_value(pid, addr, 1) {
                    for k in 0..8u8 {
                        out.push(ResultEntry {
                            addr,
                            bits: (v >> k) & 1,
                            bit: k,
                        });
                    }
                }
            }
        } else {
            for addr in addrs {
                if let Some(v) = crate::nb9::read_value(pid, addr, ty.size()) {
                    out.push(ResultEntry {
                        addr,
                        bits: v,
                        bit: 0,
                    });
                }
            }
        }
        self.scan.typed = out;
        self.selected.clear();
        self.live_bits.clear();
        self.results_epoch += 1;
        self.results_source = ResultsSource::Typed;
        self.reset_struct_view();
    }

    fn set_signed(&mut self, signed: bool) {
        self.signed = signed;
        self.clear_results();
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
        if self
            .maps
            .get(idx)
            .map(maps::is_absolute_excluded)
            .unwrap_or(true)
        {
            return;
        }
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
        let Some(entry) = self.scan.typed.get(index) else {
            return;
        };
        let key = crate::mem::entry_key(entry.addr, entry.bit);
        if !self.selected.remove(&key) {
            self.selected.insert(key);
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
    SetHexAddrInput(f64),
    JumpHexAddr,
    ToggleHexAutorefresh,
    CopyText(String),
    ToggleMap(usize),
    ScanSlot(TypeSlot),
    SetSigned(bool),
    ChangeMode(ChangeMode),
    SetInput(String),
    SetProcQuery(String),
    SetShowSystem(bool),
    SetRefreshInterval(f32),
    SetHexRefreshInterval(f32),
    CaptureScreenshot,
    ToggleConsole,
    ToggleAdvanced,
    ToggleTypeOpen,
    ToggleCondOpen,
    StartScan,
    StartRefine,
    ToggleResultSelect(usize),
    SetStructViewKind(StructViewKind),
    SetStructAnchor(usize),
    ToggleStructOpen,
    SetRangeMin(String),
    SetRangeMax(String),
    PrefetchStructWindows(Vec<u64>),
    RefreshHex,
    RequestClose,
    CancelClose,
    ConfirmClose,
}

/// `[<script-stem>]<function>` → (stem, function); 非合法格式回傳 None
pub(crate) fn parse_shortcut_label(item: &str) -> Option<(String, String)> {
    let rest = item.strip_prefix('[')?;
    let (stem, func) = rest.split_once(']')?;
    if stem.is_empty() || func.is_empty() {
        return None;
    }
    Some((stem.to_string(), func.to_string()))
}

/// 內建捷徑 → UiAction; Lua 函式標籤一律交給 render_frame 在 Lua Tick 時呼叫 (不經前端點擊), 回傳 None
fn shortcut_trigger(label: &str) -> Option<UiAction> {
    match label {
        "截圖" => return Some(UiAction::CaptureScreenshot),
        "終端台" => return Some(UiAction::ToggleConsole),
        _ => {}
    }
    None
}

/// 依現有 Lua 反射重算候選清單: 內定的截圖/終端台 + 每腳本零參全局函式（Tick 除外）,
/// 未被指派到任何大組/候選區的新函式自動入列, 已消失的反射項自動移除
fn sync_shortcut_candidates(state: &mut AppState) {
    let mut cand: Vec<String> = SHORTCUT_BUILTIN.iter().map(|s| s.to_string()).collect();
    for script in &state.lua_scripts {
        let stem = script
            .file_name
            .strip_suffix(".lua")
            .unwrap_or(&script.file_name)
            .to_string();
        for f in &script.functions {
            if f == "Tick" {
                continue;
            }
            cand.push(format!("[{stem}]{f}"));
        }
    }
    let set: std::collections::HashSet<&str> = cand.iter().map(|s| s.as_str()).collect();
    for group in &mut state.shortcut_groups {
        for row in &mut group.rows {
            row.retain(|s| set.contains(s.as_str()));
        }
    }
    state.shortcut_trailing.retain(|s| set.contains(s.as_str()));
    let mut placed: HashSet<String> = state.shortcut_trailing.iter().map(|s| s.clone()).collect();
    for group in &state.shortcut_groups {
        for row in &group.rows {
            for s in row {
                placed.insert(s.clone());
            }
        }
    }
    for c in cand {
        if !placed.contains(&c) {
            state.shortcut_trailing.push(c);
        }
    }
}

/// 取出 chip 容器; 目標行不存在時自動補一行空的, 使空大組的「佔位行」也能接收拖放
fn chip_vec(state: &mut AppState, loc: RowLocation) -> Option<&mut Vec<String>> {
    if loc.group == SHORTCUT_END {
        return Some(&mut state.shortcut_trailing);
    }
    let group = state.shortcut_groups.get_mut(loc.group)?;
    if loc.row >= group.rows.len() {
        group.rows.resize(loc.row + 1, Vec::new());
    }
    Some(&mut group.rows[loc.row])
}

fn move_chip(state: &mut AppState, from: ChipLocation, mut to: ChipLocation) {
    if to.group == SHORTCUT_END && to.row == SHORTCUT_END {
        // 丟在空白處: 附加到候選區尾端
        to.row = 0;
    }
    if from.group != SHORTCUT_END && from.group >= state.shortcut_groups.len() {
        return;
    }
    if to.group != SHORTCUT_END && to.group >= state.shortcut_groups.len() {
        return;
    }
    if from.group == to.group && from.row == to.row {
        // 同列插入: 若目標在原位之右, 先行移除會讓目標左移一格
        to.chip = to.chip.saturating_sub((from.chip < to.chip) as usize);
    }
    let chip = match chip_vec(state, RowLocation { group: from.group, row: from.row })
        .and_then(|v| if from.chip < v.len() { Some(v.remove(from.chip)) } else { None })
    {
        Some(c) => c,
        None => return,
    };
    if let Some(dest) = chip_vec(state, RowLocation { group: to.group, row: to.row }) {
        to.chip = to.chip.min(dest.len());
        dest.insert(to.chip, chip);
    }
}

fn move_row(state: &mut AppState, from: RowLocation, mut to: RowLocation) {
    if to.group == SHORTCUT_END {
        // 大組被整條拖到候選區: 拆成 chip 附加到尾端
        if from.group < state.shortcut_groups.len() {
            let group = &mut state.shortcut_groups[from.group];
            if from.row < group.rows.len() {
                let row = group.rows.remove(from.row);
                state.shortcut_trailing.extend(row);
            }
        }
        return;
    }
    if from.group >= state.shortcut_groups.len() || to.group >= state.shortcut_groups.len() {
        return;
    }
    if from.group == to.group && from.row == to.row {
        return;
    }
    let row = {
        let group = &mut state.shortcut_groups[from.group];
        if from.row >= group.rows.len() {
            return;
        }
        group.rows.remove(from.row)
    };
    let dest = &mut state.shortcut_groups[to.group].rows;
    if from.group == to.group && from.row < to.row {
        // 同大組下移: 移除一列後目標上移一格
        to.row -= 1;
    }
    to.row = to.row.min(dest.len());
    dest.insert(to.row, row);
}

/// 排空 Lua 發出的命令並套用到 state; 與 apply_actions 同一個模式(鎖外收集、鎖內套用),
/// Begin() 在 AppState::new 裡呼叫時走這裡, 按鈕觸發時 render() 每幀呼叫一次
pub(crate) fn apply_lua_cmds(state: &mut AppState) {
    for cmd in lua_env::drain_cmds() {
        match cmd {
            lua_env::LuaCmd::SelectPid(pid) => {
                if state.procs.iter().any(|p| p.pid == pid) {
                    state.attach(pid);
                } else {
                    state.status = format!("Lua: 找不到 pid {pid}");
                    state.bump();
                }
            }
            lua_env::LuaCmd::SelectName(name) => {
                let found = state
                    .procs
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case(&name))
                    .or_else(|| {
                        let needle = name.to_ascii_lowercase();
                        state.procs.iter().find(|p| p.name.to_ascii_lowercase().contains(&needle))
                    })
                    .map(|p| p.pid);
                match found {
                    Some(pid) => state.attach(pid),
                    None => {
                        state.status = format!("Lua: 找不到行程 {name}");
                        state.bump();
                    }
                }
            }
        }
    }
}

pub(crate) fn apply_action(app: &Arc<Mutex<AppState>>, action: UiAction) {
    match action {
        UiAction::StartScan => {
            let interval = app
                .lock()
                .map(|state| state.scan_slot.is_interval())
                .unwrap_or(false);
            if interval {
                start_fuzzy_scan(app);
            } else {
                start_scan(app);
            }
            return;
        }
        UiAction::StartRefine => {
            let interval = app
                .lock()
                .map(|state| state.scan_slot.is_interval())
                .unwrap_or(false);
            if interval {
                start_fuzzy_refine(app);
            } else {
                start_refine(app);
            }
            return;
        }
        UiAction::PrefetchStructWindows(addrs) => {
            prefetch_struct_windows(app, addrs);
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
        UiAction::SetHexAddrInput(v) => {
            state.hex_addr_input = v;
        }
        UiAction::JumpHexAddr => {
            let addr = state.hex_addr_input as u64;
            state.open_hex(addr);
        }
        UiAction::ToggleHexAutorefresh => {
            state.hex_autorefresh = !state.hex_autorefresh;
            state.bump();
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
        UiAction::SetHexRefreshInterval(v) => {
            state.hex_refresh_interval = v;
            state.bump();
        }
        UiAction::ToggleConsole => {
            state.console_open = !state.console_open;
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
        UiAction::SetStructViewKind(kind) => {
            if state.struct_view_kind != kind {
                state.struct_view_kind = kind;
                state.bump();
            }
        }
        UiAction::SetStructAnchor(anchor) => {
            if anchor < 3 && state.struct_anchor != anchor {
                state.struct_anchor = anchor;
                state.reset_struct_view();
                state.bump();
            }
        }
        UiAction::ToggleStructOpen => {
            state.struct_open = !state.struct_open;
            state.bump();
        }
        UiAction::SetRangeMin(text) => {
            state.range_min = text;
            state.bump();
        }
        UiAction::SetRangeMax(text) => {
            state.range_max = text;
            state.bump();
        }
        UiAction::RefreshHex => state.refresh_hex(),
        UiAction::CopyText(text) => {
            state.clip_pending = Some(text);
            state.bump();
        }
        UiAction::RequestClose => state.set_close_confirm_open(true),
        UiAction::CancelClose => state.set_close_confirm_open(false),
        UiAction::ConfirmClose => {
            std::process::exit(0);
        }
        UiAction::StartScan | UiAction::StartRefine | UiAction::PrefetchStructWindows(_) => {}
    }
}

fn selected_maps(state: &AppState) -> Vec<MapRegion> {
    state
        .maps
        .iter()
        .enumerate()
        .filter(|(i, map)| {
            state.map_checked.get(*i).copied().unwrap_or(false)
                && !maps::is_absolute_excluded(map)
        })
        .map(|(_, m)| m.clone())
        .collect()
}

fn start_fuzzy_scan(app: &Arc<Mutex<AppState>>) {
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
        if !state.scan_slot.is_interval() {
            state.status = "區塊模式請先選 Advanced → 256B".to_string();
            state.bump();
            return;
        }
        if state.maps.is_empty() {
            state.status = "沒有可掃描的映射".to_string();
            state.bump();
            return;
        }
        let selected = selected_maps(&state);
        if selected.is_empty() {
            state.status = "未選擇映射".to_string();
            state.bump();
            return;
        }
        let block_bytes = state
            .scan_slot
            .interval_block_bytes()
            .expect("scan_slot is interval");
        state.clear_results();
        state.scanning = true;
        state.status = "建立區塊快照...".to_string();
        state.bump();
        (state.pid, selected, block_bytes)
    };
    let app = Arc::clone(app);
    std::thread::spawn(move || {
        let base = mem::fuzzy_snapshot(job.0, &job.1, job.2);
        if let Ok(mut state) = app.lock() {
            state.scanning = false;
            state.status.clear();
            state.last_scan_bytes = Some(base.candidate_bytes);
            state.scan.replace_blocks(Some(base));
            state.bump();
        }
    });
}

fn start_fuzzy_refine(app: &Arc<Mutex<AppState>>) {
    let job = {
        let mut state = match app.lock() {
            Ok(v) => v,
            Err(_) => return,
        };
        if state.scanning {
            return;
        }
        // 真快照或型別結果對齊出的分塊視圖, 精選縮窄視圖; 常規側原封不動
        if state.scan.blocks.is_none() {
            state.status = "尚無區塊快照，請先按建立區塊快照".to_string();
            state.bump();
            return;
        }
        if !matches!(
            state.change_mode,
            ChangeMode::AnyChange | ChangeMode::Unchanged
        ) {
            state.status = "區塊模式僅支援任意變化／原樣，已切換為任意變化".to_string();
            state.change_mode = ChangeMode::AnyChange;
            state.bump();
        }
        let base = state.scan.blocks.take().expect("blocks checked above");
        state.scanning = true;
        state.status = "篩選中…".to_string();
        state.bump();
        (state.pid, base, state.change_mode)
    };
    let app = Arc::clone(app);
    std::thread::spawn(move || {
        let outcome = mem::fuzzy_refine(job.0, job.1, job.2);
        if let Ok(mut state) = app.lock() {
            state.scanning = false;
            state.status.clear();
            state.last_scan_bytes = Some(outcome.scanned_bytes);
            // 寫入分塊權威源; 常規側快取由 epoch 自然失效
            let empty = outcome.base.is_none();
            state.scan.replace_blocks(outcome.base);
            if empty {
                state.status = "沒有候選".to_string();
            }
            state.bump();
        }
    });
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
        let selected = selected_maps(&state);
        if selected.is_empty() {
            state.status = "未選擇映射".to_string();
            state.bump();
            return;
        }
        let ty = state.scan_slot.ty(state.signed);
        let filter = match state.change_mode {
            ChangeMode::ExactValue => match ty.parse(&state.input) {
                Some(bits) => mem::ScanFilter::Exact(bits),
                None => {
                    state.status = "掃描數值格式錯誤".to_string();
                    state.bump();
                    return;
                }
            },
            ChangeMode::Between => {
                let Some(min) = ty.parse(&state.range_min) else {
                    state.status = "範圍下限格式錯誤".to_string();
                    state.bump();
                    return;
                };
                let Some(max) = ty.parse(&state.range_max) else {
                    state.status = "範圍上限格式錯誤".to_string();
                    state.bump();
                    return;
                };
                if matches!(mem::compare(ty, min, max), std::cmp::Ordering::Greater) {
                    state.status = "範圍下限不可大於上限".to_string();
                    state.bump();
                    return;
                }
                mem::ScanFilter::Between { min, max }
            }
            _ => {
                state.status = "首次掃描僅支援目標值或範圍".to_string();
                state.bump();
                return;
            }
        };
        state.scanning = true;
        state.status = match filter {
            mem::ScanFilter::Exact(bits) => format!("掃描 {} = {} ...", ty.label(), ty.format(bits)),
            mem::ScanFilter::Between { min, max } => {
                format!("掃描 {} 範圍 {} ~ {} ...", ty.label(), ty.format(min), ty.format(max))
            }
        };
        state.bump();
        (state.pid, ty, filter, selected)
    };
    let app = Arc::clone(app);
    std::thread::spawn(move || {
        let outcome = mem::scan_ranges(job.0, job.1, job.2, &job.3);
        if let Ok(mut state) = app.lock() {
            state.scanning = false;
            state.status.clear();
            state.last_scan_bytes = Some(outcome.scanned_bytes);
            state.scan.replace_typed(outcome.results);
            state.results_source = ResultsSource::Typed;
            state.results_epoch += 1;
            state.selected.clear();
            state.live_bits.clear();
            state.reset_struct_view();
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
        if state.scan.typed.is_empty() {
            state.status = "沒有可篩選的結果".to_string();
            state.bump();
            return;
        }
        let ty = state.scan_slot.ty(state.signed);
        let filter = match state.change_mode {
            ChangeMode::ExactValue => match ty.parse(&state.input) {
                Some(v) => mem::RefineFilter::Exact(v),
                None => {
                    state.status = "掃描數值格式錯誤".to_string();
                    state.bump();
                    return;
                }
            },
            ChangeMode::IncreasedBy => match ty.parse(&state.input) {
                Some(v) => mem::RefineFilter::IncreasedBy(v),
                None => {
                    state.status = "掃描數值格式錯誤".to_string();
                    state.bump();
                    return;
                }
            },
            ChangeMode::DecreasedBy => match ty.parse(&state.input) {
                Some(v) => mem::RefineFilter::DecreasedBy(v),
                None => {
                    state.status = "掃描數值格式錯誤".to_string();
                    state.bump();
                    return;
                }
            },
            ChangeMode::Between => {
                let Some(min) = ty.parse(&state.range_min) else {
                    state.status = "範圍下限格式錯誤".to_string();
                    state.bump();
                    return;
                };
                let Some(max) = ty.parse(&state.range_max) else {
                    state.status = "範圍上限格式錯誤".to_string();
                    state.bump();
                    return;
                };
                if matches!(mem::compare(ty, min, max), std::cmp::Ordering::Greater) {
                    state.status = "範圍下限不可大於上限".to_string();
                    state.bump();
                    return;
                }
                mem::RefineFilter::Between { min, max }
            }
            ChangeMode::AnyChange => mem::RefineFilter::AnyChange,
            ChangeMode::Unchanged => mem::RefineFilter::Unchanged,
            ChangeMode::Increased => mem::RefineFilter::Increased,
            ChangeMode::Decreased => mem::RefineFilter::Decreased,
        };
        state.scanning = true;
        state.status = format!("篩選中（{}）...", state.change_mode.label());
        state.bump();
        (state.pid, ty, filter, state.scan.typed.clone())
    };
    let app = Arc::clone(app);
    std::thread::spawn(move || {
        let found = mem::refine_entries(job.0, job.1, job.2, &job.3);
        if let Ok(mut state) = app.lock() {
            state.scanning = false;
            state.status.clear();
            state.scan.replace_typed(found);
            state.results_source = ResultsSource::Typed;
            state.results_epoch += 1;
            state.selected.clear();
            state.live_bits.clear();
            state.reset_struct_view();
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
        // 區塊模式 size=0, 整批刷新會把 results 清空; 直接跳過
        if state.scan_slot.is_interval() {
            return;
        }
        if state.scan.typed.is_empty() {
            return;
        }
        let ty = state.scan_slot.ty(state.signed);
        let targets: Vec<(u64, u8)> = state
            .scan
            .typed
            .iter()
            .map(|e| (e.addr, e.bit))
            .collect();
        (state.pid, ty, targets, state.results_epoch)
    };
    let app = Arc::clone(app);
    std::thread::spawn(move || {
        // 輪詢只更新實時顯示快取; 掃描結果不可變, 嚴禁回寫
        let mut live = HashMap::with_capacity(job.2.len());
        let size = job.1.size();
        for (addr, bit) in &job.2 {
            let bits = match job.1 {
                ValueType::BoolBit => crate::nb9::read_value(job.0, *addr, 1)
                    .map(|v| (v >> bit) & 1),
                _ => crate::nb9::read_value(job.0, *addr, size),
            };
            if let Some(bits) = bits {
                live.insert(crate::mem::entry_key(*addr, *bit), bits);
            }
        }
        if let Ok(mut state) = app.lock() {
            // 副本陳化守護: 快照期間結果被清空/替換過, 這次的刷新直接作廢
            if state.results_epoch != job.3 {
                return;
            }
            state.live_bits = live;
            state.bump();
        }
    });
}

fn refresh_hex_silent(app: &Arc<Mutex<AppState>>) {
    let job = {
        let state = match app.lock() {
            Ok(v) => v,
            Err(_) => return,
        };
        if state.pid <= 0 {
            return;
        }
        (state.pid, state.hex_addr)
    };
    // 讀取失敗時保留舊轉儲, 與掃描輪詢失敗時保留舊值一致
    let dump = crate::mem::hex_dump(job.0, job.1, HEX_LEN);
    if dump.is_empty() {
        return;
    }
    if let Ok(mut state) = app.lock() {
        // 轉儲是鎖外讀取, 期間位址可能已被改動 (例如 MCP 跳轉), 舊轉儲直接作廢
        if state.hex_addr != job.1 {
            return;
        }
        state.hex = dump;
        state.bump();
    }
}

fn prefetch_struct_windows(app: &Arc<Mutex<AppState>>, addrs: Vec<u64>) {
    let job = {
        let mut state = match app.lock() {
            Ok(v) => v,
            Err(_) => return,
        };
        let ty = state.scan_slot.ty(state.signed);
        if !matches!(ty, ValueType::Float32 | ValueType::Double) || state.pid <= 0 {
            for addr in addrs {
                state.struct_pending.remove(&addr);
            }
            return;
        }
        let epoch = state.struct_epoch;
        let anchor = state.struct_anchor;
        let mut want = Vec::with_capacity(addrs.len());
        for addr in addrs {
            // 同一位址同一世代只請求一次, 背景完成前重複滾動不會重複 ioctl
            if state.struct_pending.get(&addr) == Some(&epoch) {
                continue;
            }
            state.struct_pending.insert(addr, epoch);
            want.push(addr);
        }
        if want.is_empty() {
            return;
        }
        (state.pid, ty, anchor, epoch, want)
    };
    let app = Arc::clone(app);
    std::thread::spawn(move || {
        let component = job.1.size();
        let mut out = Vec::with_capacity(job.4.len());
        for addr in &job.4 {
            let offset = (job.2 * component) as u64;
            let values = match addr.checked_sub(offset) {
                Some(base) => mem::read_struct_components(job.0, job.1, base),
                None => [None, None, None],
            };
            out.push((*addr, values));
        }
        if let Ok(mut state) = app.lock() {
            let ok = state.struct_epoch == job.3
                && state.struct_anchor == job.2
                && state.scan_slot.ty(state.signed) == job.1;
            let mut changed_any = false;
            for (addr, values) in out {
                if state.struct_pending.get(&addr) == Some(&job.3) {
                    state.struct_pending.remove(&addr);
                }
                if !ok {
                    continue;
                }
                match state.struct_cache.get_mut(&addr) {
                    Some(existing)
                        if existing.ty == job.1
                            && existing.anchor == job.2
                            && existing.epoch == job.3
                            && existing.values == values =>
                    {
                        existing.at = Instant::now();
                    }
                    _ => {
                        if state.struct_cache.len() >= 512 {
                            state.struct_cache.clear();
                        }
                        state.struct_cache.insert(
                            addr,
                            StructWindow {
                                ty: job.1,
                                anchor: job.2,
                                epoch: job.3,
                                values,
                                at: Instant::now(),
                            },
                        );
                        changed_any = true;
                    }
                }
            }
            if changed_any {
                state.bump();
            }
        }
    });
}

pub struct UiHost {
    ctx: egui::Context,
    renderer: EguiSoftwareRender,
    keyboard: Keyboard,
    clipboard: crate::keyboard::Clipboard,
    start: Instant,
    last_rev: u64,
    last_visible: bool,
    last_size: (usize, usize),
    last_screen: (usize, usize),
    last_auto_refresh: Instant,
    last_hex_refresh: Instant,
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
            clipboard: crate::keyboard::Clipboard::default(),
            start: Instant::now(),
            last_rev: 0,
            last_visible: false,
            last_size: (0, 0),
            last_screen: (0, 0),
            last_auto_refresh: Instant::now(),
            last_hex_refresh: Instant::now(),
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
        let (revision, visible, events, scanning, want_capture, refresh_interval, hex_refresh_interval, results_len, interval_slot, hex_autorefresh, on_hex) = {
            let mut state = match app.lock() {
                Ok(v) => v,
                Err(_) => return false,
            };
            let want_capture = std::mem::take(&mut state.screenshot_pending);
            // 每幀都重填, 這裡先清空; 沒畫出捷徑窗口時保持為空
            state.shortcut_held.clear();
            (
                state.revision,
                state.visible,
                std::mem::take(&mut state.events),
                state.scanning,
                want_capture,
                state.refresh_interval,
                state.hex_refresh_interval,
                state.scan.typed.len(),
                state.scan_slot.is_interval(),
                state.hex_autorefresh,
                matches!(state.tab, Tab::Hex),
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

        let lines = crate::lua_env::take_lines();
        if !visible {
            if self.last_visible {
                for px in buf[..width * height].iter_mut() {
                    *px = [0, 0, 0, 0];
                }
                self.last_visible = false;
                self.last_rev = revision;
                if lines.is_empty() {
                    return true;
                }
            } else if lines.is_empty() {
                return false;
            } else {
                // Lua 每幀都在畫: canvas 必須每幀保持全透明 + 新線, 否則舊線殘留
                for px in buf[..width * height].iter_mut() {
                    *px = [0, 0, 0, 0];
                }
            }
            draw_lua_lines(buf, width, height, &lines);
            return true;
        }

        // 區塊模式沒有逐筆帶型別刷新語意; 留著會用 size=0 重讀把 results 整個清掉
        let auto_refresh_due = results_len > 0
            && !scanning
            && !interval_slot
            && refresh_interval > 0.0
            && self.last_auto_refresh.elapsed().as_secs_f32() >= refresh_interval;

        // hex 頁自動重新整理只在 hex 頁生效; 讀到異常位址時保留舊轉儲不動
        let hex_refresh_due = hex_autorefresh
            && on_hex
            && hex_refresh_interval > 0.0
            && self.last_hex_refresh.elapsed().as_secs_f32() >= hex_refresh_interval;

        self.last_visible = true;
        self.last_size = (width, height);
        self.last_screen = (screen_w, screen_h);
        self.last_rev = revision;

        if auto_refresh_due {
            self.last_auto_refresh = Instant::now();
            refresh_results_silent(app);
        }

        if hex_refresh_due {
            self.last_hex_refresh = Instant::now();
            refresh_hex_silent(app);
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
            let mut state = match app.lock() {
                Ok(v) => v,
                Err(_) => return false,
            };
            crate::lua_env::set_target_info(state.pid, state.module_base);
            self.ctx.run_pass(raw, |ctx| {
                self.keyboard.pump_events(ctx);
                draw_ui(ctx, &mut state, &mut actions, &mut hit_rects, spinner);
                if let Some(rect) = self.keyboard.show(ctx) {
                    hit_rects.push(rect.expand(8.0));
                }
                if let Some(rect) = self.clipboard.show(ctx) {
                    hit_rects.push(rect.expand(8.0));
                }
            })
        };
        if let Ok(mut state) = app.lock() {
            state.hit_rects = hit_rects;
        }

        apply_actions(app, actions);
        if let Ok(mut state) = app.lock() {
            apply_lua_cmds(&mut state);
            // 複製請求經 AppState 過一手交給鍵盤/剪切板組件
            if let Some(text) = std::mem::take(&mut state.clip_pending) {
                self.clipboard.copy(text);
            }
        }

        for px in buf[..width * height].iter_mut() {
            *px = [0, 0, 0, 0];
        }
        draw_lua_lines(buf, width, height, &lines);
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
    // 捷徑按鈕/Lua 自瞄要「按住」而非「按到時限 = 長按」;
    // 禁用 egui 長觸控 (情境選單) 觸發, 不然按鈕約 0.8s 後會「彈起」
    ctx.options_mut(|o| o.input_options.max_click_duration = f64::INFINITY);
    ctx.set_theme(egui::ThemePreference::Dark);
    let mut visuals = egui::Visuals::dark();
    visuals.panel_fill = Color32::BLACK;
    visuals.window_fill = Color32::BLACK;
    visuals.window_shadow = egui::Shadow::NONE;
    visuals.selection.bg_fill = Color32::from_rgb(255, 45, 85);
    visuals.override_text_color = None;
    ctx.set_visuals(visuals);
}

fn selected_map_totals(state: &AppState) -> (u64, usize) {
    let mut virtual_span = 0;
    let mut selected_count = 0;
    for (map, checked) in state.maps.iter().zip(state.map_checked.iter()) {
        if *checked && !maps::is_absolute_excluded(map) {
            virtual_span += map.end - map.start;
            selected_count += 1;
        }
    }
    (virtual_span, selected_count)
}

fn map_text_color(kind: maps::MapKind) -> Color32 {
    match kind {
        maps::MapKind::DefaultSelected => Color32::from_rgb(235, 235, 245),
        maps::MapKind::DefaultExcluded => Color32::from_rgba_unmultiplied(235, 235, 245, 150),
        maps::MapKind::AbsoluteExcluded => Color32::from_rgba_unmultiplied(235, 235, 245, 77),
    }
}

pub(crate) fn apply_actions(app: &Arc<Mutex<AppState>>, actions: Vec<UiAction>) {
    for action in actions {
        apply_action(app, action);
    }
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

/// 把 Lua 這一幀的立即模式畫線疊到 canvas 上 (egui 渲染之前, 墊在所有窗口底下)
fn draw_lua_lines(
    buf: &mut [[u8; 4]],
    width: usize,
    height: usize,
    lines: &[crate::lua_env::DrawLine],
) {
    if lines.is_empty() || width == 0 || height == 0 || buf.len() < width * height {
        return;
    }
    for line in lines {
        draw_line_px(buf, width, height, line.x0, line.y0, line.x1, line.y1, line.color);
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_line_px(
    buf: &mut [[u8; 4]],
    width: usize,
    height: usize,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    color: [u8; 4],
) {
    let dx = x1 - x0;
    let dy = y1 - y0;
    let steps = dx.abs().max(dy.abs()).ceil() as i32;
    if steps <= 0 {
        blend_px(buf, width, height, x0 as i32, y0 as i32, color, 1.0);
        return;
    }
    for i in 0..=steps {
        let t = i as f32 / steps as f32;
        blend_px(
            buf,
            width,
            height,
            (x0 + dx * t) as i32,
            (y0 + dy * t) as i32,
            color,
            1.0,
        );
    }
}

fn blend_px(
    buf: &mut [[u8; 4]],
    width: usize,
    height: usize,
    x: i32,
    y: i32,
    color: [u8; 4],
    coverage: f32,
) {
    if x < 0 || y < 0 || x as usize >= width || y as usize >= height {
        return;
    }
    let idx = y as usize * width + x as usize;
    if idx >= buf.len() {
        return;
    }
    let cov = coverage.clamp(0.0, 1.0);
    let sa = (color[3] as f32 * cov).round() as u32;
    if sa == 0 {
        return;
    }
    let sr = (color[0] as u32 * sa + 127) / 255;
    let sg = (color[1] as u32 * sa + 127) / 255;
    let sb = (color[2] as u32 * sa + 127) / 255;
    let inv = 255 - sa;
    let dst = buf[idx];
    let r = sr + (dst[0] as u32 * inv + 127) / 255;
    let g = sg + (dst[1] as u32 * inv + 127) / 255;
    let b = sb + (dst[2] as u32 * inv + 127) / 255;
    let a = sa + (dst[3] as u32 * inv + 127) / 255;
    buf[idx] = [
        r.min(255) as u8,
        g.min(255) as u8,
        b.min(255) as u8,
        a.min(255) as u8,
    ];
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
    state: &mut AppState,
    actions: &mut Vec<UiAction>,
    hit_rects: &mut Vec<egui::Rect>,
    spinner: usize,
) {
    hit_rects.clear();
    // shortcut_held collects the Lua labels whose buttons are actively held this frame;
    // the Lua Tick in render_frame fires the corresponding Lua functions (never the front-end click path)
    let mut shortcut_held: Vec<String> = Vec::new();
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
                .show(ui, |ui| draw_bottom_bar(ui, state, actions));

            let footer_labels: &[(&str, UiAction)] = match state.tab {
                Tab::Hex => &[("重新整理", UiAction::RefreshHex)],
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
                        .min_size(200.0)
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
                Tab::Hex => draw_hex(ui, state, actions),
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

    // 常駐小窗口: 每個有內容的大組一個獨立窗口, 標題跟設置裡的名字走,
    // 窗口內的 chip 按 N×M 填滿鋪開; 候選名單 (Lua 反射) 不落在任何窗口
    if !state.shortcut_groups.is_empty() {
        let shortcut_fill = Color32::from_black_alpha(128);
        let mut shortcut_style = (*ctx.style_of(egui::Theme::Dark)).clone();
        // 整窗半透明黑, 標題行不單獨上色 → 沿用窗口底色
        shortcut_style.visuals.window_fill = shortcut_fill;
        shortcut_style.visuals.widgets.open.weak_bg_fill = Color32::TRANSPARENT;
        let window_frame = egui::Frame::window(&shortcut_style).shadow(egui::Shadow::NONE);
        let mut idx = 0usize;
        for group_idx in 0..state.shortcut_groups.len() {
            let rows_filled: Vec<Vec<String>> = state.shortcut_groups[group_idx]
                .rows
                .iter()
                .filter(|r| !r.is_empty())
                .cloned()
                .collect();
            if rows_filled.is_empty() {
                continue;
            }
            let group_id = state.shortcut_groups[group_idx].id;
            let name = state.shortcut_groups[group_idx].name.clone();
            let max_chips = rows_filled.iter().map(|r| r.len()).max().unwrap_or(1).max(1);
            // 定值格寬, 窗口寬度只按最長列算; 追蹤內容/spacing 做到不外漲
            const ITEM_W: f32 = 140.0;
            let default_w = max_chips as f32 * ITEM_W
                + max_chips.saturating_sub(1) as f32 * 8.0
                + 16.0;
            let rows_n = rows_filled.len() as f32;
            let default_h = (rows_n * 96.0).max(80.0);
            let default_pos = egui::pos2(
                screen.width() - default_w + 20.0 + (idx as f32 * 24.0),
                screen.height() * 0.5 - (rows_n * 96.0) * 0.5 + (idx as f32 * 40.0),
            );
            idx += 1;
            if let Some(r) = egui::Window::new(&name)
                .id(egui::Id::unique(("theraohe-shortcuts", group_id)))
                .collapsible(true)
                .resizable(true)
                .movable(state.shortcut_groups[group_idx].free_position)
                .default_pos(default_pos)
                .default_size(vec2(default_w, default_h))
                .max_size(vec2(screen.width(), screen.height()))
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
                    let rows_n = rows_filled.len() as f32;
                    let avail_h = ui.available_height();                    // 同 egui reference: 列高扣掉列間距後均分 — 不留剩餘空隙, 也不帶動窗口增長
                    let row_h = ((avail_h - ui.spacing().item_spacing.y * (rows_n - 1.0)) / rows_n).max(0.0);
                    ui.vertical(|ui| {
                        for row in rows_filled.iter() {
                            ui.allocate_ui(egui::vec2(ui.available_width(), row_h), |ui| {
                                ui.columns(row.len(), |cols| {
                                    for (col, chip) in cols.iter_mut().zip(row) {
                                        // 捷徑按鈕要「按住即持續」, 手指滑動不能被 egui 當成取消點擊 —
                                        // 走 click_and_drag sense, 移動超時 egui 仍會將其視為活躍拖曳目標 (key for 自瞄按住)
                                        let resp = col.add_sized(
                                            col.available_size(),
                                            Button::new(chip).sense(egui::Sense::click_and_drag()),
                                        );
                                        if resp.is_pointer_button_down_on() {
                                            shortcut_held.push(chip.clone());
                                        }
                                        if resp.clicked() {
                                            if let Some(a) = shortcut_trigger(chip) {
                                                actions.push(a);
                                            }
                                        }
                                    }
                                });
                            });
                        }
                    });
                })
            {
                hit_rects.push(r.response.rect.expand(8.0));
            }
        }
    }

    if state.console_open {
        let mut console_open = true;
        let console_pos = egui::pos2(screen.width() * 0.05, screen.height() * 0.55);
        if let Some(r) = egui::Window::new("終端台")
            .id(egui::Id::unique("theraohe-console"))
            .collapsible(true)
            .resizable(true)
            .default_pos(console_pos)
            .default_size(vec2(screen.width() * 0.9, screen.height() * 0.35))
            .min_size(vec2(160.0, 80.0))
            .max_size(vec2(screen.width(), screen.height()))
            .constrain(true)
            .order(egui::Order::Foreground)
            .open(&mut console_open)
            .show(ctx, |ui| {
                let text = crate::console::text().unwrap_or_default();
                egui::ScrollArea::vertical()
                    .stick_to_bottom(true)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        ui.label(RichText::new(text).monospace().size(12.0));
                    });
            })
        {
            hit_rects.push(r.response.rect.expand(8.0));
        }
        if !console_open {
            actions.push(UiAction::ToggleConsole);
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
    state.shortcut_held = shortcut_held;
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
    let order = maps::display_order(&state.maps);
    let (checked_bytes, selected_count) = selected_map_totals(state);
    let mut excluded_count = 0usize;
    let mut absolute_count = 0usize;
    for map in &state.maps {
        match maps::classify_map(map) {
            maps::MapKind::DefaultExcluded => excluded_count += 1,
            maps::MapKind::AbsoluteExcluded => absolute_count += 1,
            maps::MapKind::DefaultSelected => {}
        }
    }
    ui.label(format!(
        "映射 {} 個，選取 {} 個，虛擬範圍 {}，預設排除 {} 個，絕對不可掃描 {} 個",
        state.maps.len(),
        selected_count,
        format_size(checked_bytes),
        excluded_count,
        absolute_count
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
                ui.label("虛擬大小");
            });
            header.col(|ui| {
                ui.label("權限");
            });
            header.col(|ui| {
                ui.label("名稱");
            });
        })
        .body(|body| {
            body.rows(row_h, order.len(), |mut row| {
                let idx = order[row.index()];
                let map = &state.maps[idx];
                let kind = maps::classify_map(map);
                let absolute = kind == maps::MapKind::AbsoluteExcluded;
                let text_color = map_text_color(kind);
                row.col(|ui| {
                    let mut checked = state.map_checked.get(idx).copied().unwrap_or(false);
                    if ui
                        .add_enabled(!absolute, egui::Checkbox::new(&mut checked, ""))
                        .changed()
                    {
                        actions.push(UiAction::ToggleMap(idx));
                    }
                });
                row.col(|ui| {
                    let text = format!("{:012x}-{:012x}", map.start, map.end);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(text).monospace().color(text_color),
                        )
                        .truncate(),
                    );
                });
                row.col(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format_size(map.end - map.start))
                                .color(text_color),
                        );
                    });
                });
                row.col(|ui| {
                    ui.horizontal(|ui| {
                        for c in map.perms.chars() {
                            let color = if absolute {
                                text_color
                            } else {
                                match c {
                                    'r' => Color32::from_rgb(0x30, 0xd1, 0x58),
                                    'w' => Color32::from_rgb(0xff, 0xd6, 0x0a),
                                    'x' => Color32::from_rgb(0xff, 0x45, 0x3a),
                                    _ => Color32::from_rgba_unmultiplied(235, 235, 245, 77),
                                }
                            };
                            ui.label(egui::RichText::new(c.to_string()).monospace().color(color));
                        }
                    });
                });
                row.col(|ui| {
                    ui.label(egui::RichText::new(truncate(&map.path, 48)).color(text_color));
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
    let scan_label = if state.scan_slot.is_interval() {
        "建立區塊快照"
    } else if state.scan.typed.is_empty() {
        "首次掃描(依目標值)"
    } else {
        "新一輪掃描(依目標值)"
    };

    egui::Panel::bottom("scan-buttons")
        .resizable(false)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let (span_bytes, _) = selected_map_totals(state);
            let estimate = estimate_scan_bytes(state.scan_slot, span_bytes);
            let available = mem::available_memory_bytes();
            let estimate_text = match available {
                Some(a) => format!("預計 {} / {}", format_size(estimate), format_size(a)),
                None => format!("預計 {} / 未知", format_size(estimate)),
            };
            let insufficient = available.is_some_and(|a| estimate > a);
            let text_color = ui.visuals().text_color();
            ui.horizontal_wrapped(|ui| {
                let color = if insufficient { CHANGED_COLOR } else { text_color };
                ui.label(egui::RichText::new(estimate_text).color(color));
                if let Some(base) = &state.scan.blocks {
                    ui.label(format!(
                        "候選 {} 分塊 / {}",
                        base.candidate_blocks,
                        format_size(base.candidate_bytes)
                    ));
                }
            });
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                let w = (ui.available_width() - 6.0) / 2.0;
                let refine_enabled =
                    !state.scan.typed.is_empty() || state.scan.blocks.is_some();
                ui.add_enabled_ui(refine_enabled, |ui| {
                    if ui
                        .add_sized([w, ui.spacing().interact_size.y], Button::new("篩選結果"))
                        .clicked()
                    {
                        actions.push(UiAction::StartRefine);
                    }
                });
                if ui
                    .add_sized([w, ui.spacing().interact_size.y], Button::new(scan_label))
                    .clicked()
                {
                    actions.push(UiAction::StartScan);
                }
            });
        });

    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());

            let type_section = CollapsingHeader::new("型別")
                .open(Some(state.type_open))
                .show(ui, |ui| {
                    draw_type_grid(ui, state, actions, &TypeSlot::MAIN);
                    ui.add_space(8.0);

                    if !state.scan_slot.is_interval() {
                        ui.horizontal(|ui| {
                            if ui.add(RadioButton::new(state.signed, "有符號")).clicked() {
                                actions.push(UiAction::SetSigned(true));
                            }
                            if ui.add(RadioButton::new(!state.signed, "無符號")).clicked() {
                                actions.push(UiAction::SetSigned(false));
                            }
                        });
                    }
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
                    let interval = fuzzy_active(state);
                    if interval
                        && !matches!(state.change_mode, ChangeMode::AnyChange | ChangeMode::Unchanged)
                    {
                        actions.push(UiAction::ChangeMode(ChangeMode::AnyChange));
                    } else if bool_slot
                        && !matches!(
                            state.change_mode,
                            ChangeMode::ExactValue | ChangeMode::AnyChange | ChangeMode::Unchanged
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
                            let selected = state.change_mode == mode;
                            let enabled = condition_mode_enabled(state, mode);
                            let button = Button::selectable(selected, mode.label());
                            if ui.add_enabled(enabled, button).clicked() {
                                actions.push(UiAction::ChangeMode(mode));
                            }
                        }
                        condition_row(ui, state, actions, ChangeMode::IncreasedBy);
                        condition_row(ui, state, actions, ChangeMode::DecreasedBy);
                        condition_row(ui, state, actions, ChangeMode::Between);
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
    let is_range = mode == ChangeMode::Between;
    let enabled = condition_mode_enabled(state, mode);
    let show_input = matches!(
        mode,
        ChangeMode::ExactValue
            | ChangeMode::IncreasedBy
            | ChangeMode::DecreasedBy
            | ChangeMode::Between
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
    let single_input_w = |ui: &mut egui::Ui, text: &str, hint: &str| {
        let text_w = measure(ui, text);
        let hint_w = measure(ui, hint);
        text_w.max(hint_w).max(40.0) + 20.0
    };
    let range_min_w = if is_range {
        single_input_w(ui, &state.range_min, "Min")
    } else {
        0.0
    };
    let range_max_w = if is_range {
        single_input_w(ui, &state.range_max, "Max")
    } else {
        0.0
    };
    let input_w = if !show_input {
        0.0
    } else if mode == ChangeMode::ExactValue && bool_slot {
        measure(ui, "True") + icon_spacing_x + icon_width
    } else if is_range {
        range_min_w + item_spacing_x + range_max_w
    } else {
        single_input_w(ui, &state.input, "點此輸入")
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
    if enabled {
        if content.add(Button::selectable(selected, label)).clicked() {
            actions.push(UiAction::ChangeMode(mode));
        }
    } else {
        let _ = content.add_enabled(false, Button::selectable(selected, label));
    }
    if show_input && enabled {
        if mode == ChangeMode::ExactValue && bool_slot {
            draw_typed_input(&mut content, state, actions);
        } else if is_range {
            let mut min = state.range_min.clone();
            let resp = content.add(
                egui::TextEdit::singleline(&mut min)
                    .hint_text("Min")
                    .char_limit(32)
                    .desired_width(range_min_w),
            );
            if resp.changed() {
                actions.push(UiAction::SetRangeMin(min));
            }
            let mut max = state.range_max.clone();
            let resp = content.add(
                egui::TextEdit::singleline(&mut max)
                    .hint_text("Max")
                    .char_limit(32)
                    .desired_width(range_max_w),
            );
            if resp.changed() {
                actions.push(UiAction::SetRangeMax(max));
            }
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

fn estimate_scan_bytes(slot: TypeSlot, span_bytes: u64) -> u64 {
    match slot.interval_block_bytes() {
        Some(block) => {
            let blocks = span_bytes.saturating_add(block - 1) / block;
            blocks.saturating_mul(FUZZY_RECORD_BYTES)
                .saturating_add(FUZZY_SCAN_BUFFER)
        }
        None => FUZZY_SCAN_BUFFER,
    }
}

fn fuzzy_active(state: &AppState) -> bool {
    state.scan_slot.is_interval()
}

fn condition_mode_enabled(state: &AppState, mode: ChangeMode) -> bool {
    if fuzzy_active(state) {
        return matches!(mode, ChangeMode::AnyChange | ChangeMode::Unchanged);
    }
    let bool_slot = state.scan_slot == TypeSlot::Bool || state.scan_slot == TypeSlot::Bit;
    // 位元/布林只有 0/1: 增減幅度無意義, 但任意變化/原樣正是切換態旗標的篩選手段
    if bool_slot {
        return matches!(
            mode,
            ChangeMode::ExactValue | ChangeMode::AnyChange | ChangeMode::Unchanged
        );
    }
    // 展開出的是當次全新 bits, 本身就是合法「前值快照」, 相對條件可安全使用
    true
}

fn fuzzy_nth(base: &mem::FuzzyBase, want: usize) -> Option<(u64, u64, mem::FuzzyBlock)> {
    let mut index = 0usize;
    let mut seen = 0usize;
    for map in &base.spans {
        let mut start = map.start;
        while start < map.end {
            let end = (start + base.block_bytes).min(map.end);
            if crate::mem::mask_get(&base.kept_mask, index) {
                let block = base.blocks[index];
                if seen == want {
                    return Some((start, end, block));
                }
                seen += 1;
            }
            index += 1;
            start = end;
        }
    }
    None
}

fn draw_fuzzy_results(ui: &mut egui::Ui, base: &mem::FuzzyBase) {
    ui.label(format!(
        "區塊候選 {} 分塊 / {}",
        base.candidate_blocks,
        format_size(base.candidate_bytes)
    ));
    const PREVIEW_ROWS: usize = 256;
    let shown = base.candidate_blocks.min(PREVIEW_ROWS);
    // TableBuilder 的欄分隔線以父 ui 的 min_rect 為底, 需要與分隔線所在的 Panel 隔離
    ui.scope(|ui| {
        let w = ui.available_width();
        let range_w = (w * 0.56).min(260.0);
        TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .vscroll(true)
            .sense(egui::Sense::click())
            .drag_to_scroll(egui::scroll_area::DragScroll::Always)
            .column(Column::initial(range_w).at_least(120.0).clip(true))
            .column(Column::remainder().at_least(120.0).clip(true))
            .header(20.0, |mut header| {
                header.col(|ui| {
                    ui.label("範圍");
                });
                header.col(|ui| {
                    ui.label("雜湊");
                });
            })
            .body(|body| {
                body.rows(24.0, shown, |mut row| {
                    let Some((start, end, block)) = fuzzy_nth(base, row.index()) else {
                        return;
                    };
                    row.col(|ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(format!("0x{start:012x}-0x{end:012x}"))
                                    .monospace(),
                            )
                            .truncate(),
                        );
                    });
                    row.col(|ui| {
                        let mut text = egui::RichText::new(format!("0x{:016x}", block.hash)).monospace();
                        if block.changed {
                            text = text.color(CHANGED_COLOR);
                        }
                        ui.add(egui::Label::new(text).truncate());
                    });
                });
            });
    });
    if base.candidate_blocks > shown {
        ui.label(format!("… 僅顯示前 {} 行", shown));
    }
}

fn draw_results(ui: &mut egui::Ui, state: &AppState, actions: &mut Vec<UiAction>) {
    egui::Panel::bottom("scan-results-bottom")
        .resizable(false)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            let theme = ui.visuals().selection.bg_fill;
            let struct_section = CollapsingHeader::new("Advanced")
                .open(Some(state.struct_open))
                .show(ui, |ui| {
                    ui.horizontal_wrapped(|ui| {
                        ui.label("結構解析:");
                        let mut kind = state.struct_view_kind;
                        egui::ComboBox::from_id_salt("struct-view-kind")
                            .width(110.0)
                            .selected_text(kind.label())
                            .show_ui(ui, |ui| {
                                for option in StructViewKind::ALL {
                                    ui.selectable_value(&mut kind, option, option.label());
                                }
                            });
                        if kind != state.struct_view_kind {
                            actions.push(UiAction::SetStructViewKind(kind));
                        }
                        if kind != StructViewKind::None {
                            for (axis, label) in kind.axis_labels().into_iter().enumerate() {
                                let selected = state.struct_anchor == axis;
                                if ui.add(Button::selectable(selected, label)).clicked() {
                                    actions.push(UiAction::SetStructAnchor(axis));
                                }
                            }
                        }
                    });
                });
            if struct_section.header_response.clicked() {
                actions.push(UiAction::ToggleStructOpen);
            }
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;
                ui.label(egui::RichText::new("已選中").background_color(theme));
                ui.label(
                    egui::RichText::new("自上次掃描後已變化").color(CHANGED_COLOR),
                );
            });
        });

    if state.scan_slot.is_interval() {
        if let Some(base) = &state.scan.blocks {
            draw_fuzzy_results(ui, base);
        } else {
            ui.label("尚無候選，請先附加行程並按建立區塊快照");
        }
        return;
    }

    if !state.scan.typed.is_empty() {
        ui.label(format!(
            "{} 個結果（權威源：{}）",
            state.scan.typed.len(),
            state.scan.authority.label()
        ));
    }
    let ty = state.scan_slot.ty(state.signed);
    let size = ty.size();
    let w = ui.available_width();
    let addr_w = (w * 0.36).min(200.0);
    let raw_w = (w * 0.30).min(260.0);
    let float_view = matches!(ty, ValueType::Float32 | ValueType::Double)
        && state.struct_view_kind != StructViewKind::None;
    let row_h = if float_view { 60.0 } else { 24.0 };
    let mut struct_requests = Vec::new();
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
                ui.label("值");
            });
            header.col(|ui| {
                ui.label("解析值");
            });
        })
        .body(|body| {
            body.rows(row_h, state.scan.typed.len(), |mut row| {
                let idx = row.index();
                let entry = &state.scan.typed[idx];
                let addr = entry.addr;
                let key = crate::mem::entry_key(addr, entry.bit);
                let shown_bits = state.live_bits.get(&key).copied().unwrap_or(entry.bits);
                let changed = shown_bits != entry.bits;
                let struct_cache = float_view
                    .then(|| {
                        state.struct_cache.get(&addr).filter(|w| {
                            w.ty == ty
                                && w.anchor == state.struct_anchor
                                && w.epoch == state.struct_epoch
                        })
                    })
                    .flatten();
                let need_struct = float_view
                    && match struct_cache {
                        Some(w) => {
                            state.refresh_interval > 0.0
                                && w.at.elapsed().as_secs_f32() >= state.refresh_interval
                        }
                        None => true,
                    };
                if need_struct && state.struct_pending.get(&addr) != Some(&state.struct_epoch) {
                    struct_requests.push(addr);
                }
                row.set_selected(state.selected.contains(&key));
                row.col(|ui| {
                    let text = if matches!(ty, ValueType::BoolBit) {
                        format!("0x{:012x}({})", entry.addr, entry.bit)
                    } else {
                        format!("0x{:012x}", entry.addr)
                    };
                    ui.add(
                        egui::Label::new(egui::RichText::new(text).monospace()).truncate(),
                    );
                });
                row.col(|ui| {
                    // Bit 的值就是該位的 0/1; 原始內容欄沿用該 bit 值
                    let text = if matches!(ty, ValueType::BoolBit) {
                        format!("{}", shown_bits as u8)
                    } else {
                        hex_bytes(shown_bits, size)
                    };
                    ui.add(
                        egui::Label::new(egui::RichText::new(text).monospace()).truncate(),
                    );
                });
                row.col(|ui| {
                    if float_view {
                        let prefixes = state.struct_view_kind.axis_prefixes();
                        ui.vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 0.0;
                            for (axis, prefix) in prefixes.into_iter().enumerate() {
                                let bits = if axis == state.struct_anchor {
                                    Some(shown_bits)
                                } else {
                                    struct_cache.and_then(|w| w.values[axis])
                                };
                                let mut text = RichText::new(format!(
                                    "{}={}",
                                    prefix,
                                    bits.map(|bits| format_float_bits(ty, bits))
                                        .unwrap_or_else(|| "...".to_string())
                                ))
                                .monospace();
                                if axis == state.struct_anchor {
                                    text = text.color(if changed { CHANGED_COLOR } else { STRUCT_ANCHOR_COLOR });
                                }
                                ui.label(text);
                            }
                        });
                    } else {
                        let text = egui::RichText::new(ty.format(shown_bits));
                        let text = if changed {
                            text.color(CHANGED_COLOR)
                        } else {
                            text
                        };
                        ui.add(egui::Label::new(text).truncate());
                    }
                });
                if row.response().clicked() {
                    actions.push(UiAction::ToggleResultSelect(idx));
                }
            });
        });
    });
    if !struct_requests.is_empty() {
        actions.push(UiAction::PrefetchStructWindows(struct_requests));
    }
}

fn format_float_bits(ty: ValueType, bits: u64) -> String {
    match ty {
        ValueType::Float32 => format!("{:.3}", f32::from_bits(bits as u32)),
        ValueType::Double => format!("{:.3}", f64::from_bits(bits)),
        _ => ty.format(bits),
    }
}

const CHANGED_COLOR: Color32 = Color32::from_rgb(255, 45, 85);
const STRUCT_ANCHOR_COLOR: Color32 = Color32::from_rgb(135, 206, 250);

fn draw_hex(ui: &mut egui::Ui, state: &AppState, actions: &mut Vec<UiAction>) {
    ui.horizontal(|ui| {
        if ui.button("跳轉:").clicked() {
            actions.push(UiAction::JumpHexAddr);
        }
        let mut addr = state.hex_addr_input;
        if ui
            .add(egui::DragValue::new(&mut addr).hexadecimal(10, false, false))
            .changed()
        {
            actions.push(UiAction::SetHexAddrInput(addr));
        }
        if ui
            .add(Button::selectable(state.hex_autorefresh, "自動重新整理"))
            .clicked()
        {
            actions.push(UiAction::ToggleHexAutorefresh);
        }
    });
    ui.add_space(4.0);
    let row_count = state.hex.chunks(16).len();
    let w = ui.available_width();
    let addr_w = (w * 0.28).min(140.0);
    let hex_w = (w * 0.52).min(360.0);
    ui.spacing_mut().item_spacing.y = 4.0;
    let row_h = ui.spacing().interact_size.y;
    TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .vscroll(true)
        .drag_to_scroll(egui::scroll_area::DragScroll::Always)
        .column(Column::initial(addr_w).at_least(64.0).clip(true))
        .column(Column::initial(hex_w).at_least(72.0).clip(true))
        .column(Column::remainder().at_least(48.0).clip(true))
        .header(20.0, |mut header| {
            header.col(|ui| {
                ui.label("地址");
            });
            header.col(|ui| {
                ui.label("字節");
            });
            header.col(|ui| {
                ui.label("ASCII");
            });
        })
        .body(|body| {
            body.rows(row_h, row_count, |mut row| {
                let start = row.index() * 16;
                let chunk = &state.hex[start..(start + 16).min(state.hex.len())];
                let addr = state.hex_addr + start as u64;
                row.col(|ui| {
                    ui.add(egui::Label::new(egui::RichText::new(format!("{addr:012x}")).monospace()).truncate());
                });
                row.col(|ui| {
                    let text = chunk
                        .iter()
                        .map(|b| format!("{b:02x}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    ui.add(egui::Label::new(egui::RichText::new(text).monospace()).truncate());
                });
                row.col(|ui| {
                    let ascii: String = chunk
                        .iter()
                        .map(|b| {
                            if (0x20..=0x7e).contains(b) {
                                *b as char
                            } else {
                                '.'
                            }
                        })
                        .collect();
                    ui.add(egui::Label::new(egui::RichText::new(ascii).monospace()).truncate());
                });
            });
        });
}

fn draw_lua(ui: &mut egui::Ui, state: &mut AppState) {
    let Some(dir) = lua_env::dir() else {
        ui.label("無法定位二進制路徑");
        return;
    };
    ui.horizontal(|ui| {
        if ui.button("重新載入").clicked() {
            state.lua_scripts = lua_env::load_all();
            state.bump();
        }
        ui.label(RichText::new(dir.display().to_string()).monospace().size(12.0));
    });
    ui.add_space(6.0);

    if state.lua_scripts.is_empty() {
        ui.label("Lua/ 目錄為空或不存在");
        return;
    }
    ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for idx in 0..state.lua_scripts.len() {
            let name = state.lua_scripts[idx].file_name.clone();
            CollapsingHeader::new(&name)
                .default_open(true)
                .show(ui, |ui| draw_lua_script(ui, state, idx));
            ui.add_space(4.0);
        }
    });
}

fn draw_lua_script(ui: &mut egui::Ui, state: &mut AppState, idx: usize) {
    let script = &mut state.lua_scripts[idx];
    if let Some(err) = &script.error {
        ui.label(RichText::new(err).monospace().size(12.0).color(Color32::LIGHT_RED));
        ui.add_space(4.0);
    }

    // Tick 是每幀回呼, 不是可點擊的動作; 每幀重建列表故 clone 順手過濾
    let functions: Vec<String> = script.functions.iter().filter(|f| *f != "Tick").cloned().collect();
    let variables = script.variables.clone();
    if functions.is_empty() && variables.is_empty() {
        ui.label(RichText::new("(無反射項目)").color(Color32::GRAY));
        return;
    }

    if !functions.is_empty() {
        ui.label("函式");
        ui.horizontal_wrapped(|ui| {
            for name in &functions {
                if ui.button(name).clicked() {
                    lua_env::call(&mut state.lua_scripts[idx], name);
                }
            }
        });
        ui.add_space(4.0);
    }

    if !variables.is_empty() {
        ui.label("變量");
        // 行內流式排版, 與掃描頁「條件」同款行為: 超寬換行
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 6.0;
            for name in &variables {
                draw_lua_variable(ui, idx, name, state);
            }
        });
    }
}

/// measure_mono: {name}: 相同 monospace 字型量測寬度, chip 估算內容寬用
fn measure_mono(ui: &mut egui::Ui, text: &str) -> f32 {
    let font_id = egui::TextStyle::Monospace.resolve(ui.style());
    let color = ui.visuals().text_color();
    ui.fonts_mut(|f| f.layout_no_wrap(text.to_string(), font_id, color).size().x)
}

/// 變量 chip 外殼: 手動 allocate_space + painter 畫框, 與掃描頁「條件」chip 同款;
/// Frame::group 在 horizontal_wrapped 裡不會正確推進游標 (父佈局分行決策過晚), 內容溢出不換行
fn lua_variable_chip(ui: &mut egui::Ui, content_w: f32, add: impl FnOnce(&mut egui::Ui)) {
    const MARGIN: f32 = 6.0;
    let interact_h = ui.spacing().interact_size.y;
    let inner_w = content_w;
    let corner_radius = ui.visuals().widgets.noninteractive.corner_radius;
    let stroke = ui.visuals().widgets.noninteractive.bg_stroke;
    let (_id, rect) = ui.allocate_space(egui::vec2(inner_w + 2.0 * MARGIN, interact_h + 2.0 * MARGIN));
    ui.painter().rect_stroke(rect, corner_radius, stroke, egui::StrokeKind::Inside);
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(rect.shrink(MARGIN))
            .sense(egui::Sense::hover())
            .layout(Layout::left_to_right(Align::Center)),
    );
    content.spacing_mut().item_spacing.x = 6.0;
    add(&mut content);
}

/// egui 沒有內建 toggle switch; 自繪軌道 + 圓點 (手機原生開關樣式)
/// 開關樣式跟主題色同源: off = noninteractive 背景, on = selection 色
fn toggle_switch(ui: &mut egui::Ui, id_source: (usize, &str), checked: &mut bool) -> egui::Response {
    const WIDTH: f32 = 34.0;
    const HEIGHT: f32 = 18.0;
    let desired = egui::vec2(WIDTH, HEIGHT);
    let (rect, response) = ui.allocate_exact_size(desired, egui::Sense::click());
    if response.clicked() {
        *checked = !*checked;
    }
    let t = ui
        .ctx()
        .animate_bool_responsive(ui.make_persistent_id(id_source), *checked);
    let corner = egui::CornerRadius::same(HEIGHT as u8 / 2);
    let off_bg = ui.visuals().widgets.noninteractive.bg_fill;
    let on_bg = ui.visuals().selection.bg_fill;
    let bg = off_bg.lerp_to_gamma(on_bg, t);
    ui.painter().rect_filled(rect, corner, bg);
    let knob_r = (HEIGHT - 4.0) * 0.5;
    let x0 = rect.left() + 2.0 + knob_r;
    let x1 = rect.right() - 2.0 - knob_r;
    let cx = x0 + (x1 - x0) * t;
    let cy = rect.center().y;
    let knob = ui.visuals().text_color();
    ui.painter().circle_filled(egui::pos2(cx, cy), knob_r, knob);
    ui.painter().circle_stroke(
        egui::pos2(cx, cy),
        knob_r,
        egui::Stroke::new(1.0, ui.visuals().widgets.noninteractive.bg_stroke.color),
    );
    response
}

fn draw_lua_variable(ui: &mut egui::Ui, idx: usize, name: &str, state: &mut AppState) {
    use mlua::Value;
    let script = &mut state.lua_scripts[idx];
    let value = script.lua.globals().get::<Value>(name).unwrap_or(Value::Nil);
    match value {
        Value::Integer(mut x) => {
            let label = format!("{name}:");
            let content_w = measure_mono(ui, &label) + 6.0 + 64.0;
            lua_variable_chip(ui, content_w, |ui| {
                ui.label(RichText::new(label).monospace());
                if ui.add(egui::DragValue::new(&mut x).speed(1)).changed() {
                    lua_env::set_global(script, name, Value::Integer(x));
                }
            });
        }
        Value::Number(mut v) => {
            let label = format!("{name}:");
            let content_w = measure_mono(ui, &label) + 6.0 + 80.0;
            lua_variable_chip(ui, content_w, |ui| {
                ui.label(RichText::new(label).monospace());
                if ui
                    .add(egui::DragValue::new(&mut v).speed(0.001).max_decimals(4))
                    .changed()
                {
                    lua_env::set_global(script, name, Value::Number(v));
                }
            });
        }
        Value::Boolean(mut b) => {
            let label = format!("{name}:");
            let content_w = measure_mono(ui, &label) + 6.0 + 34.0;
            lua_variable_chip(ui, content_w, |ui| {
                ui.label(RichText::new(label).monospace());
                if toggle_switch(ui, (idx, name), &mut b).clicked() {
                    lua_env::set_global(script, name, Value::Boolean(b));
                }
            });
        }
        other => {
            ui.horizontal(|ui| {
                match other {
                    Value::String(s) => {
                        let mut v = s.to_string_lossy();
                        ui.label(RichText::new(name).monospace());
                        if ui.add(egui::TextEdit::singleline(&mut v)).changed() {
                            match script.lua.create_string(v) {
                                Ok(ls) => lua_env::set_global(script, name, Value::String(ls)),
                                Err(e) => script.error = Some(format!("{name}: {e}")),
                            }
                        }
                    }
                    kind => {
                        let label = match kind {
                            Value::Nil => "nil",
                            Value::Table(_) => "table",
                            Value::Function(_) => "function",
                            Value::Thread(_) => "thread",
                            Value::UserData(_) => "userdata",
                            Value::LightUserData(_) => "lightuserdata",
                            Value::Error(_) => "error",
                            _ => "other",
                        };
                        ui.label(RichText::new(format!("{name} = <{label}>")).monospace());
                    }
                }
            });
        }
    }
}

/// Insertion indicator, derived from the active theme (no hard-coded colors).
fn insertion_stroke(ui: &egui::Ui, width: f32) -> egui::Stroke {
    egui::Stroke::new(width, ui.visuals().selection.stroke.color)
}

fn draw_shortcut_layout(ui: &mut egui::Ui, state: &mut AppState, _actions: &mut Vec<UiAction>) {
    sync_shortcut_candidates(state);
    let mut row_from: Option<RowLocation> = None;
    let mut row_to: Option<RowLocation> = None;
    let mut chip_from: Option<ChipLocation> = None;
    let mut chip_to: Option<ChipLocation> = None;
    ui.horizontal(|ui| {
        if ui.button("+ 新增窗口").clicked() {
            let id = state.shortcut_id_next;
            state.shortcut_id_next += 1;
            state.shortcut_groups.push(ShortcutGroup::new(id, &format!("捷徑 {id}")));
            state.bump();
        }
    });
    ui.add_space(4.0);
    let mut to_delete: Vec<usize> = Vec::new();
    for group_idx in 0..state.shortcut_groups.len() {
        let group_id = state.shortcut_groups[group_idx].id;
        let name_show = state.shortcut_groups[group_idx].name.clone();
        let state_free_position = state.shortcut_groups[group_idx].free_position;
        let rows: Vec<Vec<String>> = state.shortcut_groups[group_idx].rows.clone();
        let is_renaming = state.shortcut_renaming == Some(group_idx);
        let rename_focus = std::mem::take(&mut state.shortcut_rename_focus);
        let state_id = ui.make_persistent_id(("shortcut_group", group_id));
        let name_buf = Rc::new(RefCell::new(name_show.clone()));
        let flags = Rc::new(RefCell::new(GroupFlags::default()));
        {
            let name_show2 = name_show.clone();
            let name_buf2 = Rc::clone(&name_buf);
            let flags2 = Rc::clone(&flags);
            let flags_row = Rc::clone(&flags);
            egui::collapsing_header::CollapsingState::load_with_default_open(ui.ctx(), state_id, true)
                .show_header(ui, |ui| {
                    if is_renaming {
                        let mut t = name_buf2.borrow_mut();
                        let response = ui.text_edit_singleline(&mut *t);
                        if rename_focus {
                            response.request_focus();
                        }
                        if response.lost_focus() {
                            flags2.borrow_mut().exit_rename = true;
                        }
                    } else {
                        let free_pos = state_free_position;
                        ui.horizontal(|ui| {
                            ui.label(&name_show2);
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                if ui
                                    .add(Button::new("刪除").fill(Color32::from_rgb(200, 40, 40)))
                                    .on_hover_text("刪除此窗口，其中的按鈕回到候選區")
                                    .clicked()
                                {
                                    flags2.borrow_mut().delete = true;
                                }
                                ui.add_space(2.0);
                                if ui
                                    .selectable_label(free_pos, "自由位置")
                                    .on_hover_text("勾選後懸浮窗可被拖動改變位置")
                                    .clicked()
                                {
                                    flags2.borrow_mut().toggle_free_position = true;
                                }
                                ui.add_space(2.0);
                                if ui.button("改名").clicked() {
                                    flags2.borrow_mut().start_rename = true;
                                }
                            });
                        });
                    }
                })
                .body(|ui| {
                    if ui.button("+新增Row").on_hover_text("在大組內加一條空行").clicked() {
                        flags.borrow_mut().add_row = true;
                    }
                    ui.add_space(2.0);
                    let rows_draw: Vec<&Vec<String>> = rows.iter().collect();
                    for (row_idx, row) in rows_draw.into_iter().enumerate() {
                        let row_loc = RowLocation {
                            group: group_idx,
                            row: row_idx,
                        };
                        ui.horizontal(|ui| {
                            bar_ui(
                                ui,
                                row_loc,
                                row,
                                true,
                                &mut row_from,
                                &mut row_to,
                                &mut chip_from,
                                &mut chip_to,
                            );
                            // 刪除此列; 其內項目全部彈回候選區
                            if row_idx < rows.len() {
                                if ui
                                    .button("刪除")
                                    .on_hover_text("刪除此列，內容回到候選區")
                                    .clicked()
                                {
                                    flags_row.borrow_mut().delete_rows.push(row_idx);
                                }
                            }
                        });
                    }
                });
        }
        let f = flags.borrow();
        if f.exit_rename {
            let trimmed = name_buf.borrow().trim().to_string();
            state.shortcut_groups[group_idx].name =
                if trimmed.is_empty() { "未命名".to_string() } else { trimmed };
            state.shortcut_renaming = None;
            state.bump();
        }
        if f.start_rename {
            state.shortcut_renaming = Some(group_idx);
            state.shortcut_rename_focus = true;
            state.bump();
        }
        if f.toggle_free_position {
            state.shortcut_groups[group_idx].free_position =
                !state.shortcut_groups[group_idx].free_position;
            state.bump();
        }
        if f.add_row {
            state.shortcut_groups[group_idx].rows.push(Vec::new());
            state.bump();
        }
        if !f.delete_rows.is_empty() {
            let mut idxs = f.delete_rows.clone();
            idxs.sort_by(|a, b| b.cmp(a));
            idxs.dedup();
            for i in idxs {
                if i < state.shortcut_groups[group_idx].rows.len() {
                    let chips = std::mem::take(&mut state.shortcut_groups[group_idx].rows[i]);
                    state.shortcut_groups[group_idx].rows.remove(i);
                    state.shortcut_trailing.extend(chips);
                }
            }
            state.bump();
        }
        if f.delete {
            to_delete.push(group_idx);
        }
    }
    if !to_delete.is_empty() {
        state.shortcut_renaming = None;
        for i in to_delete.into_iter().rev() {
            let group = state.shortcut_groups.remove(i);
            for row in group.rows {
                for chip in row {
                    state.shortcut_trailing.push(chip);
                }
            }
        }
        state.bump();
    }
    ui.add_space(8.0);
    ui.label("候選區:");
    let trailing = state.shortcut_trailing.clone();
    bar_ui(
        ui,
        RowLocation {
            group: SHORTCUT_END,
            row: 0,
        },
        &trailing,
        false,
        &mut row_from,
        &mut row_to,
        &mut chip_from,
        &mut chip_to,
    );
    if let (Some(from), Some(to)) = (chip_from, chip_to) {
        move_chip(state, from, to);
        state.bump();
    }
    if let (Some(from), Some(to)) = (row_from, row_to) {
        move_row(state, from, to);
        state.bump();
    }
}
#[allow(clippy::too_many_arguments)]
fn bar_ui(
    ui: &mut egui::Ui,
    row_loc: RowLocation,
    chips: &[String],
    allow_row_dnd: bool,
    row_from: &mut Option<RowLocation>,
    row_to: &mut Option<RowLocation>,
    chip_from: &mut Option<ChipLocation>,
    chip_to: &mut Option<ChipLocation>,
) {
    // The whole row (grip + items area) is a drop target as well,
    // so dropping on the grip area snaps just like anywhere else.
    let mut bar_rect = None;
    let mut bar_drop_out = None;
    let (whole, whole_drop) = ui.dnd_drop_zone::<DragPayload, ()>(Frame::new(), |ui| {
        ui.horizontal(|ui| {
            // Left grip: the vertical dnd drag handle (行程不開放拖曳).
            if allow_row_dnd {
                ui.dnd_drag_source(
                    egui::Id::unique(("shortcut_layout_bar", row_loc.group, row_loc.row)),
                    DragPayload::Row(row_loc),
                    |ui| {
                        ui.label("⠿").on_hover_text("拖曳整條上下排序");
                    },
                );
            }
            let (zone, bar_drop) = ui.dnd_drop_zone::<DragPayload, ()>(Frame::new(), |ui| {
                ui.horizontal(|ui| {
                    for (chip_idx, chip) in chips.iter().enumerate() {
                        let chip_loc = ChipLocation {
                            group: row_loc.group,
                            row: row_loc.row,
                            chip: chip_idx,
                        };
                        let chip_id = egui::Id::unique(("shortcut_layout_chip", row_loc.group, row_loc.row, chip_idx));
                        let chip_response = ui
                            .dnd_drag_source(chip_id, DragPayload::Chip(chip_loc), |ui| {
                                ui.label(chip);
                            })
                            .response;
                        let rect = chip_response.rect;
                        let pointer = ui.input(|i| i.pointer.interact_pos());
                        let insert_chip_idx = if pointer.is_some_and(|p| p.x < rect.center().x) {
                            chip_idx
                        } else {
                            chip_idx + 1
                        };
                        // Preview insertion:
                        if let (Some(pointer), Some(hovered_payload)) = (
                            pointer,
                            chip_response.dnd_hover_payload::<DragPayload>(),
                        ) {
                            if let DragPayload::Chip(hovered_loc) = *hovered_payload {
                                let stroke = insertion_stroke(ui, 1.0);
                                if hovered_loc == chip_loc {
                                    ui.painter().vline(rect.center().x, rect.y_range(), stroke);
                                } else if pointer.x < rect.center().x {
                                    ui.painter().vline(rect.left(), rect.y_range(), stroke);
                                } else {
                                    ui.painter().vline(rect.right(), rect.y_range(), stroke);
                                }
                            }
                        }
                        // Detect drops onto this chip:
                        if let (Some(pointer), Some(dragged_payload)) =
                            (pointer, chip_response.dnd_release_payload())
                        {
                            match *dragged_payload {
                                DragPayload::Chip(from_loc) => {
                                    *chip_from = Some(from_loc);
                                    *chip_to = Some(ChipLocation {
                                        group: row_loc.group,
                                        row: row_loc.row,
                                        chip: insert_chip_idx,
                                    });
                                }
                                DragPayload::Row(from_loc) => {
                                    if allow_row_dnd {
                                        *row_from = Some(from_loc);
                                        *row_to = Some(RowLocation {
                                            group: row_loc.group,
                                            row: if pointer.y < rect.center().y {
                                                row_loc.row
                                            } else {
                                                row_loc.row + 1
                                            },
                                        });
                                    }
                                }
                            }
                        }
                    }
                    // 空列: 一個斜體提示兼拖放熱區, 沒項目時也可往裡放回項目
                    if chips.is_empty() {
                        ui.label(
                            RichText::new("無內容")
                                .italics()
                                .color(ui.visuals().weak_text_color()),
                        );
                    }
                });
            });
            bar_rect = Some(zone.response.rect);
            bar_drop_out = bar_drop;
        });
    });
    let whole_rect = whole.response.rect;
    // Row-level preview: a horizontal line above/below the row.
    if allow_row_dnd {
        if let (Some(pointer), Some(hovered_payload)) = (
            ui.input(|i| i.pointer.interact_pos()),
            whole.response.dnd_hover_payload::<DragPayload>(),
        ) {
            if let DragPayload::Row(hovered_row) = *hovered_payload {
                let rect = whole_rect;
                let stroke = insertion_stroke(ui, 2.0);
                if hovered_row == row_loc {
                    ui.painter().hline(rect.x_range(), rect.center().y, stroke);
                } else if pointer.y < rect.center().y {
                    ui.painter().hline(rect.x_range(), rect.top(), stroke);
                } else {
                    ui.painter().hline(rect.x_range(), rect.bottom(), stroke);
                }
            }
        }
    }
    // Dropped on the row, but not on any one chip.
    let from_bar = bar_drop_out.is_some();
    if let Some(dragged_payload) = bar_drop_out.or(whole_drop) {
        match *dragged_payload {
            DragPayload::Chip(from_loc) => {
                *chip_from = Some(from_loc);
                *chip_to = Some(ChipLocation {
                    group: row_loc.group,
                    row: row_loc.row,
                    chip: SHORTCUT_END,
                });
            }
            DragPayload::Row(from_loc) => {
                if allow_row_dnd {
                    let rect = if from_bar { bar_rect.unwrap_or(whole_rect) } else { whole_rect };
                    let y = ui
                        .input(|i| i.pointer.interact_pos())
                        .map_or(rect.center().y, |p| p.y);
                    *row_from = Some(from_loc);
                    *row_to = Some(RowLocation {
                        group: row_loc.group,
                        row: if y < rect.center().y {
                            row_loc.row
                        } else {
                            row_loc.row + 1
                        },
                    });
                }
            }
        }
    }
}
fn draw_settings(ui: &mut egui::Ui, state: &mut AppState, actions: &mut Vec<UiAction>) {
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
            draw_shortcut_layout(ui, state, actions);
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

    egui::CollapsingHeader::new("Hex Dump")
        .default_open(true)
        .show(ui, |ui| {
            settings_row(ui, "重新整理間隙", |ui| {
                let mut secs = state.hex_refresh_interval;
                if ui
                    .add(
                        egui::DragValue::new(&mut secs)
                            .speed(0.1)
                            .range(0.1..=10.0)
                            .suffix("s"),
                    )
                    .changed()
                {
                    actions.push(UiAction::SetHexRefreshInterval(secs));
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

fn draw_bottom_bar(ui: &mut egui::Ui, state: &AppState, actions: &mut Vec<UiAction>) {
    let mut items: Vec<String> = Vec::new();
    if state.pid > 0 {
        items.push(format!("行程: {} | PID: {}", state.proc_name, state.pid));
    } else {
        items.push("行程: - | PID: -".to_string());
    }
    // 點擊基底位址複製到應用內剪切板; 其他 item 保持 hover-only
    let copy_idx = (state.module_base != 0).then(|| {
        items.push(format!("基底位址: 0x{:x}", state.module_base));
        items.len() - 1
    });
    if !state.maps.is_empty() {
        let (virtual_span, selected_count) = selected_map_totals(state);
        items.push(format!(
            "映射{}個 | 共 {}",
            selected_count,
            format_size(virtual_span)
        ));
    }
    if let Some(scanned_bytes) = state.last_scan_bytes {
        items.push(format!("本次掃描 {}", format_size(scanned_bytes)));
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
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                ui.separator();
            }
            if Some(i) == copy_idx {
                if ui.add(egui::Label::new(item).sense(egui::Sense::click())).clicked() {
                    actions.push(UiAction::CopyText(format!("0x{:x}", state.module_base)));
                }
            } else {
                ui.add(egui::Label::new(item));
            }
        }
    });
}