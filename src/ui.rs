use crate::lua_env;
use crate::maps::{self, MapRegion};
use crate::mem::{self, ChangeMode, ProcInfo, ResultEntry, ValueType};
use egui::{
    vec2, Align, Button, CollapsingHeader, Color32, FontData, FontDefinitions, FontFamily, Frame,
    Layout, RadioButton, RichText, ScrollArea, TextWrapMode, Vec2,
};
use egui_extras::{Column, TableBuilder};
use egui_on_screen_ime_temp::Clipboard;
use egui_on_screen_ime_temp::Keyboard;
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
    toggle_allow_out_of_bounds: bool,
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
/// 沒有 id; 順序固定為按名字的 Unicode 碼位排序(A-Z 在前), 同名保序
#[derive(Clone, PartialEq, Eq)]
pub struct ShortcutGroup {
    /// Name shown as the CollapsingHeader label; also used as the window title.
    pub name: String,
    /// rows -> items; rows reorder vertically, items reorder horizontally inside each row.
    pub rows: Vec<Vec<String>>,
    /// 可否自由移動此懸浮窗; false = 鎖位置不可拖動
    pub free_position: bool,
    /// 允許此懸浮窗越出螢幕邊界(配合橫向螢幕佈局); 關閉時關閉動作會把視窗收攏回螢幕內
    pub allow_out_of_bounds: bool,
    /// 懸浮窗的最後可見 rect(使用者拖動的位置/大小); 未曾拖動/沒畫出來時為 None
    pub window_geom: Option<egui::Rect>,
}

/// 渲染後端: GPU (wgpu) 或 CPU (egui 軟體後端); 默認 GPU。
/// GPU 初始化失敗時本進程回落 CPU (UiHost.gpu_unavailable), 不改寫持久化選項。
/// 切換生效方式: 下拉只改 pending, 存檔後下次啟動生效——渲染路徑永遠走啟動時載入的後端,
/// 不在同一個 ANativeWindow 裡讓 WSI 與 CPU 鎖定輪流當 producer (會餓死 BufferQueue, UI 凍結)
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RenderBackend {
    Gpu,
    Cpu,
}

impl RenderBackend {
    pub fn as_str(self) -> &'static str {
        match self {
            RenderBackend::Gpu => "gpu",
            RenderBackend::Cpu => "cpu",
        }
    }

    pub fn from_str_lossy(s: &str) -> Self {
        // 默認 GPU; 僅明確寫 cpu 才走軟體回退
        match s {
            "cpu" => RenderBackend::Cpu,
            _ => RenderBackend::Gpu,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            RenderBackend::Gpu => "GPU(wgpu)",
            RenderBackend::Cpu => "CPU(軟體)",
        }
    }
}

/// UiHost::render 的產出: 呼叫方據此決定是否還要碰 ANativeWindow
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RenderOutcome {
    /// 畫面無變化 (無 revision 變更 / 無事件 / 無 Lua 線 / egui 無重繪請求); 上一幀保持原樣
    Skip,
    /// CPU 軟體渲染完成, 呼叫方需把 canvas 拷進 ANativeWindow
    Canvas,
    /// GPU 後端已 present
    Presented,
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
    pub const ALL: [Tab; 6] = [
        Tab::Proc,
        Tab::Maps,
        Tab::Scan,
        Tab::Hex,
        Tab::Lua,
        Tab::Settings,
    ];

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
    pub(crate) fn new(name: &str) -> Self {
        ShortcutGroup {
            name: name.to_owned(),
            rows: Vec::new(),
            free_position: true,
            allow_out_of_bounds: false,
            window_geom: None,
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
    Utf16,
    Utf8,
}

impl TypeSlot {
    pub const MAIN: [TypeSlot; 9] = [
        TypeSlot::S32,
        TypeSlot::S64,
        TypeSlot::F32,
        TypeSlot::F64,
        TypeSlot::Bool,
        TypeSlot::Bit,
        TypeSlot::U8,
        TypeSlot::Ptr,
        TypeSlot::Utf16,
    ];

    pub const ADVANCED: [TypeSlot; 3] = [TypeSlot::S16, TypeSlot::Range, TypeSlot::Utf8];

    pub fn ty(self, signed: bool) -> ValueType {
        match self {
            TypeSlot::Utf16 => ValueType::StringUtf16,
            TypeSlot::Utf8 => ValueType::StringUtf8,
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
            TypeSlot::Utf16 => "Str(Utf16)",
            TypeSlot::Utf8 => "Str(Utf8)",
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

// 結果為單一快照: 常規型別與 256B 區塊快照互斥共存; 前進常規時整批展開並取代, 不做雙側投影
pub(crate) struct ScanResults {
    pub typed: Vec<ResultEntry>,
    pub blocks: Option<mem::FuzzyBase>,
}

impl ScanResults {
    fn new() -> Self {
        Self {
            typed: Vec::new(),
            blocks: None,
        }
    }

    fn replace_typed(&mut self, entries: Vec<ResultEntry>) {
        self.typed = entries;
        self.blocks = None;
    }

    fn replace_blocks(&mut self, base: Option<mem::FuzzyBase>) {
        self.blocks = base;
        self.typed.clear();
    }

    fn clear(&mut self) {
        self.typed.clear();
        self.blocks = None;
    }
}

/// 底欄「結果 → 新目標型別(解讀方式)」的小寫短標籤; Range 顯示 256B
fn compact_type_label(ty: ValueType) -> &'static str {
    match ty {
        ValueType::Int32 => "i32",
        ValueType::UInt32 => "u32",
        ValueType::Int64 => "i64",
        ValueType::UInt64 => "u64",
        ValueType::Float32 => "f32",
        ValueType::Double => "f64",
        ValueType::Bool => "bool",
        ValueType::BoolBit => "Bit",
        ValueType::UInt8 => "u8",
        ValueType::Ptr => "Ptr",
        ValueType::Int16 => "i16",
        ValueType::UInt16 => "u16",
        ValueType::Range => "256B",
        ValueType::StringUtf16 => "Str16",
        ValueType::StringUtf8 => "Str8",
    }
}

/// 凍結結果重解讀為預覽型別的方式; 所有切換皆走窗口重解讀或拆分, 不物化
fn reinterpret_method(frozen: ValueType, preview: ValueType) -> &'static str {
    let interval = |ty: ValueType| ty == ValueType::Range;
    match (interval(frozen), interval(preview)) {
        (true, true) => "原樣",
        (true, false) => "拆分",
        (false, true) => "聚塊",
        (false, false) => {
            let f = mem::type_bits(frozen);
            let p = mem::type_bits(preview);
            if frozen == preview {
                "原樣"
            } else if f == p {
                "重新解讀"
            } else if p < f {
                "拆分"
            } else {
                "擴展"
            }
        }
    }
}

/// 擴展偏移標籤: 前 26 個 A..Z, 其後 0.. (位元級偏移可達 63)
fn expand_label(i: u8) -> String {
    if i < 26 {
        ((b'A' + i) as char).to_string()
    } else {
        (i - 26).to_string()
    }
}

/// 篩選提交用的重建規格: 窄→寬時遮罩覆蓋 off*f 處的 f 個凍結位元窗口,
/// 落地時位址減 off*f/8; 寬→窄 (拆分) 由 expand_frozen 處理, 回傳 None
fn expand_spec(frozen: ValueType, preview: ValueType, off: u8) -> Option<mem::Expand> {
    let f = mem::type_bits(frozen);
    let p = mem::type_bits(preview);
    if f == 0 || p == 0 || p <= f {
        return None;
    }
    let shift = off as u64 * f;
    Some(mem::Expand {
        mask: mem::mask_bits(f) << shift,
        shift: shift as u8,
        base_decay: shift / 8,
    })
}

/// 預覽中某凍結位址對應的基址: 擴展時 N - off*f/8, 選項 0 (A) 即 N 本身; 其餘情形位址不變
fn expand_base(addr: u64, frozen: ValueType, preview: ValueType, off: u8) -> u64 {
    let f = mem::type_bits(frozen);
    let p = mem::type_bits(preview);
    if p <= f || f == 0 {
        return addr;
    }
    addr.wrapping_sub(off as u64 * f / 8)
}

/// 擴展適用條件: 同族型別， 窄→寬 (不含 Range), 回傳可選偏移數 (A=0..)
fn expand_option_count(frozen: ValueType, preview: ValueType) -> Option<u8> {
    let f = mem::type_bits(frozen);
    let p = mem::type_bits(preview);
    if f == 0 || p == 0 || p <= f {
        return None;
    }
    let n = p / f;
    u8::try_from(n).ok()
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
    /// 結果凍結時的型別: 型別切換只影響下次掃描與預覽, 不改寫此欄位; 篩選/掃描提交時對齊當前解析
    pub(crate) frozen_ty: ValueType,
    /// 擴展偏移: 窄→寬時候選位元組在 widened 值中的記憶體位元組偏移 (0 = 起點即候選; 與端序無關, 組值時才受端序影響)
    pub(crate) expand_off: u8,
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
    /// 介面縮放的單一真相來源; 設置頁 DragValue 寫它, ctx 側用 set_zoom_factor 跟上
    pub(crate) zoom: f32,
    /// 啟動時載入的後端; 設置頁下拉不寫它, 改寫 pending_backend, render() 固定走 active_backend
    pub render_backend: RenderBackend,
    /// 設置頁尚未生效的待選後端; None = 與 render_backend 一致。存檔時寫入 settings.toon, 下次啟動生效
    pub pending_backend: Option<RenderBackend>,
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
    /// 本幀主按鈕按住的捷徑 (Lua 標籤, 手指按下時的屏幕物理座標 (左上原點)); render() 重填, render_frame 在 Lua Tick 時消費
    pub shortcut_held: Vec<(String, Option<(f64, f64)>)>,
    /// 點擊後被判定為"開關式"的捷徑標籤 → 開關狀態; 首次呼叫回 Bool 後登記; 僅持行程有效, 重啓默認關
    pub shortcut_toggles: HashMap<String, bool>,
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
    pub fn new(show: bool, settings: crate::persist::LoadedSettings) -> Self {
        let driver_ok = crate::nb9::probe();
        let status_bar_h = crate::display::status_bar_height().unwrap_or(0);
        let procs = mem::list_processes();
        let mut status = if driver_ok {
            format!("核心驅動已連線，行程 {} 個", procs.len())
        } else {
            "核心驅動未連線：ioctl 618 失敗".to_string()
        };
        if let Some(note) = &settings.note {
            status.push_str("; ");
            status.push_str(note);
        }
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
            selected: HashSet::new(),
            live_bits: HashMap::new(),
            struct_view_kind: StructViewKind::None,
            struct_anchor: 0,
            struct_open: true,
            struct_cache: HashMap::new(),
            struct_pending: HashMap::new(),
            struct_epoch: 1,
            frozen_ty: ValueType::Int32,
            expand_off: 0,
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
            refresh_interval: settings.refresh_interval,
            hex_refresh_interval: settings.hex_refresh_interval,
            zoom: settings.zoom,
            render_backend: settings.backend,
            pending_backend: None,
            hex_addr_input: 0.0,
            hex_autorefresh: false,
            clip_pending: None,
            console_open: false,
            lua_scripts: lua_env::load_all(),
            shortcut_groups: settings.shortcut_groups,
            shortcut_trailing: Vec::new(),
            shortcut_renaming: None,
            shortcut_rename_focus: false,
            shortcut_held: Vec::new(),
            shortcut_toggles: HashMap::new(),
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

    fn preview_ty(&self) -> ValueType {
        self.scan_slot.ty(self.signed)
    }

    /// 寬→窄切分: 每個凍結入口展開 f/p 行 (位址順序); 其餘為 1 行
    pub(crate) fn preview_split(&self) -> u64 {
        let f = mem::type_bits(self.frozen_ty);
        let p = mem::type_bits(self.preview_ty());
        if p == 0 {
            return 1;
        }
        if f > p {
            f / p
        } else {
            1
        }
    }

    /// 虛擬行數: 預覽中呈現的總行數 (切分展開後)
    pub(crate) fn preview_row_count(&self) -> usize {
        self.scan.typed.len() * self.preview_split() as usize
    }

    /// 虛擬行映射 → (位址, 位元序, 凍結窗口位元值, needle 長度)
    /// 切分時 addr = N + k·p/8, bits 截取第 k 個窗口 (p<8 時 bit=k·p%8);
    /// 窄→寬時 addr 退 off 位元組, bits 原樣 (整個凍結值落在窗口低處)
    pub(crate) fn preview_row(&self, idx: usize) -> Option<(u64, u8, u64, u16)> {
        let f = mem::type_bits(self.frozen_ty);
        let p = mem::type_bits(self.preview_ty());
        if f == 0 || p == 0 {
            let e = self.scan.typed.get(idx)?;
            return Some((e.addr, e.bit, e.bits, e.len));
        }
        let q = if f > p { f / p } else { 1 };
        let e_idx = idx / q as usize;
        let k = (idx as u64) % q;
        let e = self.scan.typed.get(e_idx)?;
        if f <= p {
            let base = expand_base(e.addr, self.frozen_ty, self.preview_ty(), self.expand_off);
            return Some((base, e.bit, e.bits, e.len));
        }
        let addr = e.addr + (k * p / 8);
        let bits = (e.bits >> (k * p)) & mem::mask_bits(p);
        let bit = if p < 8 { ((k * p) % 8) as u8 } else { 0 };
        Some((addr, bit, bits, 0))
    }

    pub(crate) fn clear_results(&mut self) {
        self.scan.clear();
        self.results_epoch += 1;
        self.selected.clear();
        self.live_bits.clear();
        self.reset_struct_view();
        self.expand_off = 0;
        self.frozen_ty = self.preview_ty();
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
        // 目標為 256B: 若凍結結果不是 256B, 從常規候選重建對齊視圖 (投影, 不動結果本身);
        // 凍結結果本身是 256B 時 blocks 即快照, 原樣保留
        if slot.is_interval() {
            let block_bytes = slot.interval_block_bytes().expect("slot is interval");
            if self.frozen_ty != ValueType::Range {
                self.scan.blocks = (!self.scan.typed.is_empty()).then(|| {
                    mem::blocks_from_entries(self.pid, &self.maps, &self.scan.typed, block_bytes)
                });
            }
            self.scan_slot = slot;
            self.expand_off = 0;
            self.selected.clear();
            self.live_bits.clear();
            self.bump();
            return;
        }
        // 256B → 常規: blocks 是靠粗篩建立的快照, 展開成整批常規候選並取代 (此為唯一前進路徑)
        // 此時 typed 側自始為空, 無資料語意可言, 直接枚舉窗口落地
        if self.frozen_ty == ValueType::Range {
            let ty = slot.ty(self.signed);
            match &self.scan.blocks {
                Some(base) => {
                    let (entries, _) = mem::fuzzy_to_entries(self.pid, base, ty);
                    self.scan.typed = entries;
                }
                None => self.scan.typed.clear(),
            }
            self.scan.blocks = None;
            self.frozen_ty = ty;
            self.expand_off = 0;
            self.results_epoch += 1;
            self.selected.clear();
            self.live_bits.clear();
            self.reset_struct_view();
            self.input.clear();
            self.scan_slot = slot;
            self.bump();
            return;
        }
        // 一切常規型別切換皆為純預覽: 不重讀不物化, 只影響下次掃描與預覽;
        // 凍結快照在此永不改動 (窗口重解讀/切分皆由表格與篩選時投影推導)
        // 同寬切換保留選中 (鍵不變), 異寬行/基址皆可能位移, 選中與 live 鍵全部作廢
        if let Some(opts) = expand_option_count(self.frozen_ty, slot.ty(self.signed)) {
            self.expand_off = self.expand_off.min(opts - 1);
        } else {
            self.expand_off = 0;
        }
        if mem::type_bits(self.frozen_ty) != mem::type_bits(slot.ty(self.signed)) {
            self.selected.clear();
            self.live_bits.clear();
            self.reset_struct_view();
        }
        self.scan_slot = slot;
        self.bump();
    }

    fn set_signed(&mut self, signed: bool) {
        if self.signed == signed {
            return;
        }
        // 純重新解讀(i32↔u32 等同寬切換): 不清空結果, 預覽重繪; 位址與位元語意不變, 選中保留
        self.signed = signed;
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
        // 虛擬行: 以預覽映射為鍵, 與 live_bits / 選中集合一致
        let Some((addr, bit, _, _)) = self.preview_row(index) else {
            return;
        };
        let key = crate::mem::entry_key(addr, bit);
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
    SetExpandOff(u8),
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
    let chip = match chip_vec(
        state,
        RowLocation {
            group: from.group,
            row: from.row,
        },
    )
    .and_then(|v| {
        if from.chip < v.len() {
            Some(v.remove(from.chip))
        } else {
            None
        }
    }) {
        Some(c) => c,
        None => return,
    };
    if let Some(dest) = chip_vec(
        state,
        RowLocation {
            group: to.group,
            row: to.row,
        },
    ) {
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
                        state
                            .procs
                            .iter()
                            .find(|p| p.name.to_ascii_lowercase().contains(&needle))
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
            let (interval, snapshot) = app
                .lock()
                .map(|state| {
                    (
                        state.scan_slot.is_interval(),
                        state.change_mode == ChangeMode::Snapshot,
                    )
                })
                .unwrap_or((false, false));
            if interval {
                // 256B 視圖: 「記錄快照」重建為區塊快照, 其餘為區塊篩選
                if snapshot {
                    start_fuzzy_scan(app);
                } else {
                    start_fuzzy_refine(app);
                }
            } else if snapshot {
                start_typed_snapshot(app);
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
            if state.tab == Tab::Settings && tab != Tab::Settings {
                crate::persist::save(&state);
            }
            if state.tab == Tab::Lua && tab != Tab::Lua {
                crate::lua_env::save_configs(&state.lua_scripts);
            }
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
        UiAction::SetExpandOff(off) => {
            // 位址改基址後 live/選中鍵全部失效
            state.expand_off = off;
            state.live_bits.clear();
            state.selected.clear();
            state.bump();
        }
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
            crate::persist::save(&state);
            crate::lua_env::save_configs(&state.lua_scripts);
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
            state.map_checked.get(*i).copied().unwrap_or(false) && !maps::is_absolute_excluded(map)
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
            // 256B 快照成為新的凍結結果: 整批清除常規側
            state.frozen_ty = ValueType::Range;
            state.expand_off = 0;
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
        // 256B 篩選只在結果本身是 256B 快照時成立; 否則 blocks 是常規候選投影出的視圖
        if state.frozen_ty != ValueType::Range {
            state.status = "請先切換到常規型別或 256B 快照".to_string();
            state.bump();
            return;
        }
        if state.scan.blocks.is_none() {
            state.status = "尚無區塊快照，請先按建立區塊快照".to_string();
            state.bump();
            return;
        }
        if !matches!(
            state.change_mode,
            ChangeMode::AnyChange | ChangeMode::Unchanged
        ) {
            // 「快照」已由 apply_action 路由到建立區塊快照; 到這裡代表呼叫方違約
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
            // 256B 快照縮窄後仍是 256B 結果
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
        state.status = match &filter {
            mem::ScanFilter::Exact(bits) => format!(
                "掃描 {} = {} ...",
                ty.label(),
                if mem::is_string(ty) {
                    format!("0x{:016x}", bits)
                } else {
                    ty.format(*bits)
                }
            ),
            mem::ScanFilter::Between { min, max } => {
                format!(
                    "掃描 {} 範圍 {} ~ {} ...",
                    ty.label(),
                    ty.format(*min),
                    ty.format(*max)
                )
            }
        };
        state.bump();
        (state.pid, ty, filter, selected, state.input.clone())
    };
    let app = Arc::clone(app);
    std::thread::spawn(move || {
        let outcome = if mem::is_string(job.1) {
            // 字串: 按位元組序列 needle 搜尋, 走針
            let needle = mem::encode_needle(job.1, &job.4).expect("string needle prevalidated");
            mem::scan_string(job.0, job.1, &needle, &job.3)
        } else {
            mem::scan_ranges(job.0, job.1, job.2, &job.3)
        };
        if let Ok(mut state) = app.lock() {
            state.scanning = false;
            state.status.clear();
            state.last_scan_bytes = Some(outcome.scanned_bytes);
            state.scan.replace_typed(outcome.results);
            // 若當前停在 256B 視圖, 依新資料重建區塊投影; 否則保持空替換
            if state.scan_slot.is_interval() {
                let block_bytes = state
                    .scan_slot
                    .interval_block_bytes()
                    .expect("scan_slot is interval");
                state.scan.blocks = (!state.scan.typed.is_empty()).then(|| {
                    mem::blocks_from_entries(state.pid, &state.maps, &state.scan.typed, block_bytes)
                });
            }
            state.results_epoch += 1;
            state.selected.clear();
            state.live_bits.clear();
            state.reset_struct_view();
            state.frozen_ty = job.1;
            state.expand_off = 0;
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
        // 快照已在 apply_action 路由到 start_snapshot_typed; 到這裡代表呼叫方違約, 直接放棄
        if state.change_mode == ChangeMode::Snapshot {
            return;
        }
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
            // 已在 apply_action 早期路由到 start_snapshot_typed, 到此代表呼叫方違約
            ChangeMode::Snapshot => return,
        };
        state.scanning = true;
        state.status = format!("篩選中（{}）...", state.change_mode.label());
        state.bump();
        // 切分 (ps<fs): 先把凍結入口切成窗口候選再篩選; 擴展 (ps>fs): 維持重建基準比對
        let frozen = state.frozen_ty;
        let expand = expand_spec(frozen, ty, state.expand_off);
        (
            state.pid,
            ty,
            filter,
            state.scan.typed.clone(),
            frozen,
            expand,
        )
    };
    let app = Arc::clone(app);
    std::thread::spawn(move || {
        let merged = mem::expand_frozen(&job.3, job.4, job.1);
        let found = mem::refine_entries(job.0, job.1, job.2, &merged, job.5);
        if let Ok(mut state) = app.lock() {
            state.scanning = false;
            state.status.clear();
            state.scan.replace_typed(found);
            // 256B 視圖下落地的常規結果要重建區塊投影, 否則表格會空白
            if state.scan_slot.is_interval() {
                let block_bytes = state
                    .scan_slot
                    .interval_block_bytes()
                    .expect("scan_slot is interval");
                state.scan.blocks = (!state.scan.typed.is_empty()).then(|| {
                    mem::blocks_from_entries(state.pid, &state.maps, &state.scan.typed, block_bytes)
                });
            }
            state.results_epoch += 1;
            state.selected.clear();
            state.live_bits.clear();
            state.reset_struct_view();
            // 落地: 結果凍結到本次解析型別, 擴展回到 A
            state.frozen_ty = job.1;
            state.expand_off = 0;
            state.bump();
        }
    });
}

/// 記錄快照: 以當前已有的預覽快照本身重凍結, 不做任何篩選,
/// 型別切換後無「結果快照」可比差異時, 以此重建基線
pub(crate) fn start_typed_snapshot(app: &Arc<Mutex<AppState>>) {
    let job = {
        let mut state = match app.lock() {
            Ok(v) => v,
            Err(_) => return,
        };
        if state.scanning {
            return;
        }
        if state.scan.typed.is_empty() {
            state.status = "沒有可快照的結果".to_string();
            state.bump();
            return;
        }
        let ty = state.scan_slot.ty(state.signed);
        // 以絕對地址枚舉虛擬行 (切分後展開投影), 同目標集同鍵同序
        let targets: Vec<ResultEntry> = (0..state.preview_row_count())
            .filter_map(|i| {
                state.preview_row(i).map(|(addr, bit, _, len)| ResultEntry {
                    addr,
                    bits: 0,
                    bit,
                    len,
                })
            })
            .collect();
        state.scanning = true;
        state.status = "記錄快照…".to_string();
        state.bump();
        (state.pid, ty, targets)
    };
    let app = Arc::clone(app);
    std::thread::spawn(move || {
        let size = job.1.size();
        let mut out = Vec::with_capacity(job.2.len());
        for e in &job.2 {
            let bits = match job.1 {
                ValueType::BoolBit => {
                    crate::nb9::read_value(job.0, e.addr, 1).map(|v| (v >> e.bit) & 1)
                }
                ValueType::StringUtf16 | ValueType::StringUtf8 => {
                    if e.len == 0 {
                        continue;
                    }
                    let buf = crate::mem::hex_dump(job.0, e.addr, e.len as usize);
                    if buf.is_empty() {
                        continue;
                    }
                    Some(crate::mem::string_hash(&buf))
                }
                _ => crate::nb9::read_value(job.0, e.addr, size),
            };
            let Some(bits) = bits else { continue };
            out.push(ResultEntry {
                addr: e.addr,
                bits,
                bit: if matches!(job.1, ValueType::BoolBit) {
                    e.bit
                } else {
                    0
                },
                len: e.len,
            });
        }
        if let Ok(mut state) = app.lock() {
            state.scanning = false;
            state.status.clear();
            state.scan.replace_typed(out);
            state.results_epoch += 1;
            state.selected.clear();
            state.live_bits.clear();
            state.reset_struct_view();
            // 落地: 結果凍結到本次解析型別, 擴展回到 A
            state.frozen_ty = job.1;
            state.expand_off = 0;
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
        let row_count = state.preview_row_count();
        // String 預覽需要 needle 長度; 由行映射一併帶上
        let targets: Vec<(u64, u8, u16)> = (0..row_count)
            .filter_map(|i| {
                state
                    .preview_row(i)
                    .map(|(addr, bit, _, len)| (addr, bit, len))
            })
            .collect();
        (state.pid, ty, targets, state.results_epoch)
    };
    let app = Arc::clone(app);
    std::thread::spawn(move || {
        // 輪詢只更新實時顯示快取; 掃描結果不可變, 嚴禁回寫
        let mut live = HashMap::with_capacity(job.2.len());
        let size = job.1.size();
        // Bit 預覽下 8 行共用一個位元組, 快取住避免重複 ioctl
        let mut byte_cache: HashMap<u64, Option<u8>> = HashMap::new();
        for &(addr, bit, len) in &job.2 {
            let bits = match job.1 {
                ValueType::BoolBit => {
                    let byte = match byte_cache
                        .entry(addr)
                        .or_insert_with(|| crate::nb9::read_value(job.0, addr, 1).map(|v| v as u8))
                    {
                        Some(b) => *b,
                        None => continue,
                    };
                    Some(((byte >> bit) & 1) as u64)
                }
                ValueType::StringUtf16 | ValueType::StringUtf8 => {
                    if len == 0 {
                        continue;
                    }
                    let full = crate::mem::hex_dump(job.0, addr, len as usize);
                    if full.is_empty() {
                        continue;
                    }
                    Some(crate::mem::string_hash(&full))
                }
                _ => crate::nb9::read_value(job.0, addr, size),
            };
            if let Some(bits) = bits {
                live.insert(crate::mem::entry_key(addr, bit), bits);
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
    clipboard: Clipboard,
    start: Instant,
    last_rev: u64,
    last_auto_refresh: Instant,
    last_hex_refresh: Instant,
    layer_offset: (f32, f32),
    frames: u64,
    /// ANativeWindow 指針 (usize 存放以保住 !Send/!Sync 之外無額外約束); 0 = 未設置
    window: usize,
    /// 懶建立的 GPU 後端; 建立失敗時 gpu_unavailable 上鎖, 本進程全部走 CPU
    gpu: Option<crate::gpu_render::GpuRenderer>,
    gpu_unavailable: bool,
    /// 實際在用的後端; 啟動時與設置同步, 下拉只改 AppState.pending_backend, 本值進程內不變
    pub active_backend: RenderBackend,
}

impl UiHost {
    pub fn load(font_path: &str, font_index: Option<u32>, zoom: f32) -> Result<Self, String> {
        let data = std::fs::read(font_path).map_err(|e| format!("read {font_path}: {e}"))?;
        let mut fonts = FontDefinitions::default();
        let index = font_index.unwrap_or(0);
        let noto = FontData::from_blob(Arc::new(data), index);
        fonts
            .font_data
            .insert("noto-cjk".to_owned(), Arc::new(noto));
        for family in [FontFamily::Proportional, FontFamily::Monospace] {
            fonts
                .families
                .entry(family)
                .or_default()
                .splice(0..0, ["noto-cjk".to_owned()]);
        }
        let ctx = egui::Context::default();
        ctx.set_fonts(fonts);
        configure_style(&ctx, zoom);
        let renderer = EguiSoftwareRender::new(ColorFieldOrder::Rgba).with_caching(true);
        Ok(Self {
            ctx,
            renderer,
            keyboard: Keyboard::default(),
            clipboard: Clipboard::default(),
            start: Instant::now(),
            last_rev: 0,
            last_auto_refresh: Instant::now(),
            last_hex_refresh: Instant::now(),
            layer_offset: (0.0, 0.0),
            frames: 0,
            window: 0,
            gpu: None,
            gpu_unavailable: false,
            active_backend: RenderBackend::Gpu,
        })
    }

    /// run_vsync 建立 ANativeWindow 後呼叫; GPU 後端目接用它建 surface
    pub fn set_window(&mut self, window: *mut std::ffi::c_void) {
        self.window = window as usize;
    }

    /// 懶建立 GPU 後端; 失敗回傳 Err (已記錄狀態, 呼叫方應回落 CPU)
    /// swapchain 配成 max(w,h) x max(w,h), 與 ANativeWindow_setBuffersGeometry 的方形 layer 一致
    fn ensure_gpu(
        &mut self,
        width: u32,
        height: u32,
    ) -> Result<&mut crate::gpu_render::GpuRenderer, String> {
        let side = width.max(height);
        if self.gpu.is_none() {
            if self.gpu_unavailable {
                return Err("GPU 後端之前初始化失敗".to_string());
            }
            if self.window == 0 {
                return Err("尚未 set_window".to_string());
            }
            match crate::gpu_render::GpuRenderer::new(
                self.window as *mut std::ffi::c_void,
                side,
                side,
                width,
                height,
            ) {
                Ok(g) => self.gpu = Some(g),
                Err(e) => {
                    self.gpu_unavailable = true;
                    return Err(e);
                }
            }
        } else if let Some(gpu) = &mut self.gpu {
            gpu.resize(side, side, width, height);
        }
        Ok(self.gpu.as_mut().unwrap())
    }

    pub fn render(
        &mut self,
        app: &Arc<Mutex<AppState>>,
        buf: &mut [[u8; 4]],
        width: usize,
        height: usize,
        screen_w: usize,
        screen_h: usize,
    ) -> RenderOutcome {
        if width == 0 || height == 0 || buf.len() < width * height {
            return RenderOutcome::Skip;
        }
        if screen_w == 0 || screen_h == 0 {
            return RenderOutcome::Skip;
        }
        let (
            revision,
            visible,
            events,
            scanning,
            want_capture,
            refresh_interval,
            hex_refresh_interval,
            results_len,
            interval_slot,
            hex_autorefresh,
            on_hex,
        ) = {
            let mut state = match app.lock() {
                Ok(v) => v,
                Err(_) => return RenderOutcome::Skip,
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

        // 後端不可原地切換 (Mali WSI↔CPU 生產者互踩會讓 BufferQueue 餓死), render 永遠走
        // active_backend; 設置頁選的待生效值由 pending_backend 獨立記錄, 存檔後下次啟動生效
        let render_backend = self.active_backend;

        // 區塊模式沒有逐筆帶型別刷新語意; 留著會用 size=0 重讀把 results 整個清掉
        let auto_refresh_due = visible
            && results_len > 0
            && !scanning
            && !interval_slot
            && refresh_interval > 0.0
            && self.last_auto_refresh.elapsed().as_secs_f32() >= refresh_interval;

        // hex 頁自動重新整理只在 hex 頁生效; 讀到異常位址時保留舊轉儲不動
        let hex_refresh_due = visible
            && hex_autorefresh
            && on_hex
            && hex_refresh_interval > 0.0
            && self.last_hex_refresh.elapsed().as_secs_f32() >= hex_refresh_interval;

        if auto_refresh_due {
            self.last_auto_refresh = Instant::now();
            refresh_results_silent(app);
        }

        if hex_refresh_due {
            self.last_hex_refresh = Instant::now();
            refresh_hex_silent(app);
        }

        let lines = crate::lua_env::take_lines();
        let texts = crate::lua_env::take_texts();
        // 無變化跳過: revision 沒動、沒事件、沒 Lua 線、egui 沒有重繪請求,
        // 且不在掃描動畫/自動刷新中 → 整個 pass + present 都省掉, 上一幀保持原樣
        let changed = revision != self.last_rev
            || !events.is_empty()
            || !lines.is_empty()
            || !texts.is_empty()
            || want_capture
            || auto_refresh_due
            || hex_refresh_due
            || (visible && scanning)
            || self.ctx.has_requested_repaint();
        if !changed {
            return RenderOutcome::Skip;
        }
        self.last_rev = revision;
        self.frames += 1;

        let spinner = if scanning && visible {
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
                Err(_) => return RenderOutcome::Skip,
            };
            crate::lua_env::set_target_info(state.pid, state.module_base);
            self.ctx.run_pass(raw, |ctx| {
                paint_lua_draw(ctx, &lines, &texts, ppp);
                if visible {
                    self.keyboard.pump_events(ctx);
                    draw_ui(ctx, &mut state, &mut actions, &mut hit_rects, spinner);
                    if let Some(rect) = self.keyboard.show(ctx) {
                        hit_rects.push(rect.expand(8.0));
                    }
                    if let Some(rect) = self.clipboard.show(ctx) {
                        hit_rects.push(rect.expand(8.0));
                    }
                }
            })
        };
        if visible {
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
        }

        let out = out;
        let primitives = self.ctx.tessellate(out.shapes, out.pixels_per_point);

        if matches!(render_backend, RenderBackend::Gpu) {
            match self.ensure_gpu(width as u32, height as u32) {
                Ok(gpu) => {
                    let outcome = match gpu.present(
                        &primitives,
                        out.textures_delta,
                        out.pixels_per_point,
                        want_capture,
                    ) {
                        Ok(crate::gpu_render::PresentOutcome::Presented) => {
                            RenderOutcome::Presented
                        }
                        Ok(crate::gpu_render::PresentOutcome::Captured(px)) => {
                            spawn_screenshot(px, width, height, self.layer_offset, app);
                            RenderOutcome::Presented
                        }
                        Ok(crate::gpu_render::PresentOutcome::Skipped) => RenderOutcome::Skip,
                        Err(e) => {
                            eprintln!("[gpu] present 失敗, 本進程回落 CPU: {e}");
                            self.gpu_unavailable = true;
                            if let Ok(mut state) = app.lock() {
                                state.status = format!("GPU 呈現失敗, 已回落 CPU: {e}");
                                state.bump();
                            }
                            return RenderOutcome::Skip;
                        }
                    };
                    return outcome;
                }
                Err(e) => {
                    eprintln!("[gpu] 初始化失敗, 本進程回落 CPU: {e}");
                    if let Ok(mut state) = app.lock() {
                        state.status = format!("GPU 初始化失敗, 已回落 CPU: {e}");
                        state.bump();
                    }
                }
            }
        }

        for px in buf[..width * height].iter_mut() {
            *px = [0, 0, 0, 0];
        }
        let mut buffer = BufferMutRef::new(buf, width, height);
        self.renderer.render(
            &mut buffer,
            &primitives,
            &out.textures_delta,
            out.pixels_per_point,
        );
        if want_capture {
            spawn_screenshot(
                buf[..width * height].to_vec(),
                width,
                height,
                self.layer_offset,
                app,
            );
        }
        RenderOutcome::Canvas
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

fn configure_style(ctx: &egui::Context, zoom: f32) {
    // 只改背景為純黑（AMOLED）；按鈕等控制項的預設樣式原封不動；主色改為 255,45,85
    // set_zoom_factor 會延遲到下一個 pass 才生效，且那次應用會用上一幀的 screen_rect
    // 做縮放換算；ctx 建立後第一幀沒有上一幀，會量到 egui 的默認 5000x5000，
    // 把窗口默認尺寸撐爆並永久 persist。直接寫 options，讓第一幀就拿到正確 ppp。
    ctx.options_mut(|o| o.zoom_factor = zoom);
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

/// Lua 即時線畫在 egui Background 層: 位於所有窗口/捷徑之下, CPU/GPU 兩後端共用同一份 primitives。
/// Lua 線座標是物理像素; egui 的世界是 logical point (= 物理 / ppp), 所以位置和線寬都要除以 ppp,
/// 這樣線不會隨 zoom 放大, 截圖/畫面上的落點與 CPU 軟體渲染一致。
/// Lua 畫線/文本共用同一個 Background 圖層: 同層按插入順序繪製, lines 先 lines 後成文
/// (egui 對未註冊 area 的"額外圖層"以雜湊序排, 不保證我們的呼叫順序, 所以合併)
fn paint_lua_draw(
    ctx: &egui::Context,
    lines: &[crate::lua_env::DrawLine],
    texts: &[crate::lua_env::DrawText],
    ppp: f32,
) {
    if lines.is_empty() && texts.is_empty() {
        return;
    }
    let inv = 1.0 / ppp.max(f32::EPSILON);
    let small = egui::FontId::monospace(9.0);
    let body = egui::FontId::monospace(13.0);
    let heading = egui::FontId::monospace(18.0);
    let painter = ctx.layer_painter(egui::LayerId::new(
        egui::Order::Background,
        egui::Id::unique("lua-draw"),
    ));
    for line in lines {
        painter.line_segment(
            [
                egui::pos2(line.x0 * inv, line.y0 * inv),
                egui::pos2(line.x1 * inv, line.y1 * inv),
            ],
            egui::Stroke::new(
                inv,
                egui::Color32::from_rgba_unmultiplied(
                    line.color[0],
                    line.color[1],
                    line.color[2],
                    line.color[3],
                ),
            ),
        );
    }
    for t in texts {
        let anchor = egui::pos2(t.x * inv, t.y * inv);
        let color =
            egui::Color32::from_rgba_unmultiplied(t.color[0], t.color[1], t.color[2], t.color[3]);
        let font = match t.style {
            1 => body.clone(),
            2 => heading.clone(),
            _ => small.clone(),
        };
        let galley = painter.layout_no_wrap(t.text.clone(), font, color);
        let size = galley.size();
        let min = egui::pos2(anchor.x - size.x * t.align_x, anchor.y - size.y * t.align_y);
        painter.galley(min, galley, color);
    }
}

/// 截圖在背景執行緒合成並落盤, 不擋 vsync; 產物回寫 state.screenshot_result + bump
fn spawn_screenshot(
    canvas: Vec<[u8; 4]>,
    width: usize,
    height: usize,
    offset: (f32, f32),
    app: &Arc<Mutex<AppState>>,
) {
    let offset = (offset.0 as i32, offset.1 as i32);
    let app = Arc::clone(app);
    std::thread::spawn(move || {
        let res = crate::screenshot::capture_canvas(&canvas, width, height, offset);
        match &res {
            Ok(path) => {
                let _ = std::process::Command::new("am")
                    .args([
                        "broadcast",
                        "-a",
                        "android.intent.action.MEDIA_SCANNER_SCAN_FILE",
                        "-d",
                        &format!("file://{path}"),
                    ])
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
    // shortcut_held collects the Lua labels whose buttons are actively held this frame,
    // paired with the physical-px pointer position at press time;
    // the Lua Tick in render_frame fires the corresponding Lua functions (never the front-end click path)
    let mut shortcut_held: Vec<(String, Option<(f64, f64)>)> = Vec::new();
    let screen = ctx.content_rect();
    let ppp = ctx.options(|o| o.zoom_factor);
    // content_rect 是邏輯尺寸 (= 物理尺寸 / zoom), 窗口按邏輯比例取值, 物理比例不隨 zoom 變
    // 主 UI 尺寸不分方向都以短邊 90% 為邊長的正方形, 物理大小旋轉後不變
    let side = screen.width().min(screen.height());
    let main_w = (side * 0.90).max(280.0);
    let main_h = (side * 0.90).max(320.0);
    // 狀態欄高度按物理像素記錄, 佈局以邏輯座標, 需除以縮放
    let main_pos = egui::pos2(0.0, state.status_bar_h as f32 / ctx.pixels_per_point());
    let main_id = egui::Id::unique("theraohe-main");
    if state
        .collapse_toggle
        .swap(false, std::sync::atomic::Ordering::Relaxed)
    {
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
                                    .add_sized(
                                        [bw, ui.spacing().interact_size.y],
                                        Button::new(*label),
                                    )
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
            let name = state.shortcut_groups[group_idx].name.clone();
            let max_chips = rows_filled
                .iter()
                .map(|r| r.len())
                .max()
                .unwrap_or(1)
                .max(1);
            // 定值格寬, 窗口寬度只按最長列算; 追蹤內容/spacing 做到不外漲
            const ITEM_W: f32 = 140.0;
            let default_w =
                max_chips as f32 * ITEM_W + max_chips.saturating_sub(1) as f32 * 8.0 + 16.0;
            let rows_n = rows_filled.len() as f32;
            let default_h = (rows_n * 96.0).max(80.0);
            // 捷徑窗口的位置/大小由使用者自己拖, 不與設置頁共享來源: 直接採納持久化 rect
            let persisted = state.shortcut_groups[group_idx].window_geom;
            let (default_pos, default_size) = match persisted {
                Some(r) => (r.min, r.size()),
                None => (
                    egui::pos2(
                        screen.width() - default_w + 20.0 + (idx as f32 * 24.0),
                        screen.height() * 0.5 - (rows_n * 96.0) * 0.5 + (idx as f32 * 40.0),
                    ),
                    vec2(default_w, default_h),
                ),
            };
            idx += 1;
            if let Some(r) = egui::Window::new(&name)
                .collapsible(true)
                .resizable(state.shortcut_groups[group_idx].free_position)
                .movable(state.shortcut_groups[group_idx].free_position)
                .constrain(!state.shortcut_groups[group_idx].allow_out_of_bounds)
                .default_pos(default_pos)
                .default_size(default_size)
                .max_size(vec2(screen.width(), screen.height()))
                .order(egui::Order::Background)
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
                    let avail_h = ui.available_height(); // 同 egui reference: 列高扣掉列間距後均分 — 不留剩餘空隙, 也不帶動窗口增長
                    let row_h = ((avail_h - ui.spacing().item_spacing.y * (rows_n - 1.0)) / rows_n)
                        .max(0.0);
                    ui.vertical(|ui| {
                        for row in rows_filled.iter() {
                            ui.allocate_ui(egui::vec2(ui.available_width(), row_h), |ui| {
                                ui.columns(row.len(), |cols| {
                                    for (col, chip) in cols.iter_mut().zip(row) {
                                        // 開關式捷徑 (點擊後被判定為回 Bool 的按鈕): selectable 渲染且不進按壓積極呼叫
                                        let toggle_on =
                                            state.shortcut_toggles.get(chip.as_str()).copied();
                                        let resp = match toggle_on {
                                            Some(on) => col.add_sized(
                                                col.available_size(),
                                                Button::new(chip).selected(on),
                                            ),
                                            None => col.add_sized(
                                                // 捷徑按鈕要「按住即持續」, 手指滑動不能被 egui 當成取消點擊 —
                                                // 走 click_and_drag sense (key for 自瞄按住)
                                                col.available_size(),
                                                Button::new(chip)
                                                    .sense(egui::Sense::click_and_drag()),
                                            ),
                                        };
                                        if resp.is_pointer_button_down_on() && toggle_on.is_none() {
                                            let pos = resp.interact_pointer_pos().map(|p| {
                                                (f64::from(p.x * ppp), f64::from(p.y * ppp))
                                            });
                                            shortcut_held.push((chip.clone(), pos));
                                        }
                                        if resp.clicked() {
                                            // 開關式探測只對 0 參函式做無參呼叫 (xy 函式是按住類, 點擊不應無參呼叫);
                                            // 0 參且回 Bool → 視為開關: 點擊呼叫一次, 回傳值自動變成開關狀態
                                            let toggled = parse_shortcut_label(chip)
                                                .and_then(|(stem, function)| {
                                                    state
                                                        .lua_scripts
                                                        .iter()
                                                        .find(|s| {
                                                            s.file_name == format!("{stem}.lua")
                                                        })
                                                        .filter(|s| {
                                                            lua_env::is_zero_arg_function(
                                                                s, &function,
                                                            )
                                                        })
                                                        .map(|_| (stem, function))
                                                })
                                                .and_then(|(stem, function)| {
                                                    state
                                                        .lua_scripts
                                                        .iter_mut()
                                                        .find(|s| {
                                                            s.file_name == format!("{stem}.lua")
                                                        })
                                                        .map(|s| (s, function))
                                                })
                                                .and_then(|(s, function)| {
                                                    lua_env::call(s, &function, None)
                                                })
                                                .and_then(|v| match v {
                                                    mlua::Value::Boolean(b) => Some(b),
                                                    _ => None,
                                                });
                                            if let Some(b) = toggled {
                                                state.shortcut_toggles.insert(chip.clone(), b);
                                            } else if !state
                                                .shortcut_toggles
                                                .contains_key(chip.as_str())
                                            {
                                                if let Some(a) = shortcut_trigger(chip) {
                                                    actions.push(a);
                                                }
                                            }
                                        }
                                    }
                                });
                            });
                        }
                    });
                })
            {
                state.shortcut_groups[group_idx].window_geom = Some(r.response.rect);
                hit_rects.push(r.response.rect.expand(8.0));
            }
        }
    }

    if state.console_open {
        let mut console_open = true;
        let console_side = screen.width().min(screen.height());
        let console_pos = egui::pos2(screen.width() * 0.5, screen.height() * 0.5);
        if let Some(r) = egui::Window::new("終端台")
            .id(egui::Id::unique("theraohe-console"))
            .collapsible(true)
            .resizable(true)
            .default_pos(console_pos)
            .default_size(vec2(console_side * 0.75, console_side * 0.6))
            .min_size(vec2(160.0, 80.0))
            .max_size(vec2(screen.width(), screen.height()))
            .constrain(true)
            .order(egui::Order::Background)
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

fn equal_row(ui: &mut egui::Ui, n: usize, add: impl FnOnce(&mut [egui::Ui])) {
    let size = egui::vec2(ui.available_width(), ui.spacing().interact_size.y);
    ui.allocate_ui_with_layout(size, Layout::top_down(Align::Min), |ui| ui.columns(n, add));
}

fn draw_toolbar(ui: &mut egui::Ui, state: &AppState, actions: &mut Vec<UiAction>) {
    equal_row(ui, Tab::ALL.len(), |cols| {
        for (col, tab) in cols.iter_mut().zip(Tab::ALL) {
            let selected = state.tab == tab;
            let button = Button::selectable(selected, RichText::new(tab.label()))
                .wrap_mode(TextWrapMode::Truncate);
            let clicked = col
                .centered_and_justified(|ui| ui.add(button).clicked())
                .inner;
            if clicked {
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
    equal_row(ui, 2, |cols| {
        let show = cols[0]
            .centered_and_justified(|ui| {
                ui.add(Button::selectable(state.show_system, "顯示系統行程"))
                    .clicked()
            })
            .inner;
        if show {
            actions.push(UiAction::SetShowSystem(!state.show_system));
        }
        let refresh = cols[1]
            .centered_and_justified(|ui| ui.button("重新整理").clicked())
            .inner;
        if refresh {
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
                        .add_sized(
                            [w, ui.spacing().interact_size.y],
                            Button::selectable(selected, text),
                        )
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
                        egui::Label::new(egui::RichText::new(text).monospace().color(text_color))
                            .truncate(),
                    );
                });
                row.col(|ui| {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(
                            egui::RichText::new(format_size(map.end - map.start)).color(text_color),
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

fn draw_typed_input(ui: &mut egui::Ui, state: &AppState, actions: &mut Vec<UiAction>) {
    if state.scan_slot == TypeSlot::Bool || state.scan_slot == TypeSlot::Bit {
        let mut checked = state.input == "1";
        let label = if checked { "True" } else { "False" };
        if ui.checkbox(&mut checked, label).changed() {
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
                let color = if insufficient {
                    CHANGED_COLOR
                } else {
                    text_color
                };
                ui.label(egui::RichText::new(estimate_text).color(color));
                if let Some(base) = &state.scan.blocks {
                    ui.label(format!(
                        "候選 {} 分塊 / {}",
                        base.candidate_blocks,
                        format_size(base.candidate_bytes)
                    ));
                }
            });
            let refine_enabled = !state.scan.typed.is_empty() || state.scan.blocks.is_some();
            equal_row(ui, 2, |cols| {
                let refine = cols[0]
                    .centered_and_justified(|ui| {
                        ui.add_enabled_ui(refine_enabled, |ui| ui.button("篩選結果").clicked())
                            .inner
                    })
                    .inner;
                let scan = cols[1]
                    .centered_and_justified(|ui| ui.button(scan_label).clicked())
                    .inner;
                if refine {
                    actions.push(UiAction::StartRefine);
                }
                if scan {
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
                    // 當前條件因模型切換被禁用時, 退回矩陣內第一個可用的條件
                    if !condition_mode_enabled(state, state.change_mode) {
                        actions.push(UiAction::ChangeMode(enabled_fallback(state)));
                    }
                    ui.horizontal_wrapped(|ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        condition_row(ui, state, actions, ChangeMode::ExactValue);
                        for mode in [
                            ChangeMode::Snapshot,
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
    let bool_slot = matches!(
        state.scan_slot.ty(state.signed),
        ValueType::Bool | ValueType::BoolBit
    );
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
    // 两段式 Frame::group: 首帧量測, 次帧按缓存尺寸分配, 超宽由 horizontal_wrapped 判断换行;
    // 与 Lua 页 framed_var 同款, 外框为原生 Frame::group, 不手绘.
    let id = ui.make_persistent_id(("condition-chip", mode as u8));
    framed_var(ui, id, |ui| {
        var_chip(ui, |ui| {
            let mut changed = false;
            if enabled {
                if ui.add(Button::selectable(selected, label)).clicked() {
                    actions.push(UiAction::ChangeMode(mode));
                }
            } else {
                let _ = ui.add_enabled(false, Button::selectable(selected, label));
            }
            if show_input && enabled {
                if mode == ChangeMode::ExactValue && bool_slot {
                    draw_typed_input(ui, state, actions);
                } else if is_range {
                    let (font_id, text_color) = {
                        let s = ui.style();
                        (egui::TextStyle::Body.resolve(s), ui.visuals().text_color())
                    };
                    let measure = |text: &str| {
                        ui.fonts_mut(|f| {
                            f.layout_no_wrap(text.to_string(), font_id.clone(), text_color)
                                .size()
                                .x
                        })
                    };
                    // TextEdit 無 content-autosize, 按文字寬量出 desired_width 來貼齊內容; DragValue 由 egui 自動量
                    let field_w = |text: &str, hint: &str| {
                        (measure(text).max(measure(hint)) + 16.0).max(48.0)
                    };
                    let min_w = field_w(&state.range_min, "Min");
                    let max_w = field_w(&state.range_max, "Max");
                    let mut min = state.range_min.clone();
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut min)
                            .hint_text("Min")
                            .char_limit(32)
                            .desired_width(min_w),
                    );
                    if resp.changed() {
                        actions.push(UiAction::SetRangeMin(min));
                    }
                    changed |= resp.changed();
                    let mut max = state.range_max.clone();
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut max)
                            .hint_text("Max")
                            .char_limit(32)
                            .desired_width(max_w),
                    );
                    if resp.changed() {
                        actions.push(UiAction::SetRangeMax(max));
                    }
                    changed |= resp.changed();
                } else {
                    changed = draw_numeric_drag(ui, state, actions);
                }
            }
            changed
        })
    });
}

/// 數值輸入一律 DragValue; state.input 為單一真相 (拖拉後寫回新的數字字串).
/// 解析失敗退回 0; 整數依型別位寬與符號做飽和夾制.
fn draw_numeric_drag(ui: &mut egui::Ui, state: &AppState, actions: &mut Vec<UiAction>) -> bool {
    let ty = state.scan_slot.ty(state.signed);
    let raw = state.input.trim();
    match ty {
        ValueType::StringUtf16 | ValueType::StringUtf8 => {
            // String 不綁數值, 直接 TextEdit, 與數字型別輸入隔開
            let mut text = state.input.clone();
            let resp = ui.add(
                egui::TextEdit::singleline(&mut text)
                    .hint_text("點此輸入")
                    .desired_width(120.0),
            );
            if resp.changed() {
                actions.push(UiAction::SetInput(text));
            }
            resp.changed()
        }
        ValueType::Float32 | ValueType::Double => {
            let max_dec = if ty == ValueType::Float32 { 4 } else { 6 };
            let mut v: f64 = raw.parse().unwrap_or(0.0);
            let resp = ui.add(
                egui::DragValue::new(&mut v)
                    .speed(0.001)
                    .max_decimals(max_dec),
            );
            if resp.changed() {
                actions.push(UiAction::SetInput(v.to_string()));
            }
            resp.changed()
        }
        ValueType::Ptr => {
            let mut v: u64 = ty.parse(raw).unwrap_or(0);
            let resp = ui.add(
                egui::DragValue::new(&mut v)
                    .speed(1.0)
                    .hexadecimal(10, false, false),
            );
            if resp.changed() {
                actions.push(UiAction::SetInput(format!("0x{v:x}")));
            }
            resp.changed()
        }
        _ => {
            let size = ty.size();
            if size == 8 && !state.signed {
                // u64 全範圍: 拖拉速度 1, 飽和夾制交給輸出格式
                let parsed: u64 = raw.parse().unwrap_or(0);
                let mut v = parsed;
                let resp = ui.add(egui::DragValue::new(&mut v).speed(1.0));
                if resp.changed() {
                    actions.push(UiAction::SetInput(format!("{v}")));
                }
                resp.changed()
            } else {
                let (min, max): (i64, i64) = match (size, state.signed) {
                    (1, true) => (i8::MIN as i64, i8::MAX as i64),
                    (1, false) => (0, u8::MAX as i64),
                    (2, true) => (i16::MIN as i64, i16::MAX as i64),
                    (2, false) => (0, u16::MAX as i64),
                    (4, true) => (i32::MIN as i64, i32::MAX as i64),
                    (4, false) => (0, u32::MAX as i64),
                    (8, true) => (i64::MIN, i64::MAX),
                    _ => (i32::MIN as i64, i32::MAX as i64),
                };
                let mut v: i64 = raw.parse().unwrap_or(0);
                v = v.clamp(min, max);
                let resp = ui.add(egui::DragValue::new(&mut v).range(min..=max));
                if resp.changed() {
                    actions.push(UiAction::SetInput(format!("{v}")));
                }
                resp.changed()
            }
        }
    }
}

fn estimate_scan_bytes(slot: TypeSlot, span_bytes: u64) -> u64 {
    match slot.interval_block_bytes() {
        Some(block) => {
            let blocks = span_bytes.saturating_add(block - 1) / block;
            blocks
                .saturating_mul(FUZZY_RECORD_BYTES)
                .saturating_add(FUZZY_SCAN_BUFFER)
        }
        None => FUZZY_SCAN_BUFFER,
    }
}

// 條件可用性: (條件, 結果型別 → 預覽型別) 為依據; 需要結果快照的差異條件,
// 僅在完全同型時放開 (不同型沒有可比對的基準快照, 256B 快照只存雜湊沒有位元組),
// Bit/Bool 只有「變化 / 原樣」, 沒有定量語意; 256B 視圖只有快照與區塊雜湊比對。
// 「快照」不需快照, 任何時機都可用, 用作沒有可比對基線時的基線重建。
fn condition_mode_enabled(state: &AppState, mode: ChangeMode) -> bool {
    if state.scan_slot.is_interval() {
        return matches!(
            mode,
            ChangeMode::Snapshot | ChangeMode::AnyChange | ChangeMode::Unchanged
        );
    }
    let ty = state.scan_slot.ty(state.signed);
    let same = ty == state.frozen_ty;
    // bool/bit 與 string 只有「變化 / 原樣」語意 (string 比對精確位元組, 無定量增減)
    let bool_like = matches!(
        ty,
        ValueType::Bool | ValueType::BoolBit | ValueType::StringUtf16 | ValueType::StringUtf8
    );
    match mode {
        // 記錄快照: 任何時機可用
        ChangeMode::Snapshot => true,
        // 目標值: 只讀當前值, 無需快照
        ChangeMode::ExactValue => true,
        // 區間: 只讀量, 無需快照; bool/bit 沒有範圍語意
        ChangeMode::Between => !bool_like,
        // 差異 (任意變化/原樣): 需完全同型, 但 Bit/Bool 只有變或原
        ChangeMode::AnyChange | ChangeMode::Unchanged => same,
        // 定量增減 (含按量): 需完全同型且非 bool/bit
        ChangeMode::Increased
        | ChangeMode::Decreased
        | ChangeMode::IncreasedBy
        | ChangeMode::DecreasedBy => same && !bool_like,
    }
}

/// 同型優先順序: 差異條件 > 目標值 > 快照; 找第一個可用的頂替
fn enabled_fallback(state: &AppState) -> ChangeMode {
    const PRIORITY: [ChangeMode; 9] = [
        ChangeMode::AnyChange,
        ChangeMode::Unchanged,
        ChangeMode::ExactValue,
        ChangeMode::Snapshot,
        ChangeMode::Increased,
        ChangeMode::Decreased,
        ChangeMode::IncreasedBy,
        ChangeMode::DecreasedBy,
        ChangeMode::Between,
    ];
    PRIORITY
        .into_iter()
        .find(|&m| condition_mode_enabled(state, m))
        .unwrap_or(ChangeMode::Snapshot)
}

fn draw_fuzzy_results(ui: &mut egui::Ui, base: &mem::FuzzyBase) {
    ui.label(format!(
        "區塊候選 {} 分塊 / {}",
        base.candidate_blocks,
        format_size(base.candidate_bytes)
    ));
    // 每次列舉被保留的區塊: vscroll 尾幀只會接觸可見行, 盤點一次全部 O(n) 足夠
    let mut kept: Vec<(u64, u64, mem::FuzzyBlock)> = Vec::with_capacity(base.candidate_blocks);
    let mut index = 0usize;
    for map in &base.spans {
        let mut start = map.start;
        while start < map.end {
            let end = (start + base.block_bytes).min(map.end);
            if crate::mem::mask_get(&base.kept_mask, index) {
                kept.push((start, end, base.blocks[index]));
            }
            index += 1;
            start = end;
        }
    }
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
                body.rows(24.0, kept.len(), |mut row| {
                    let (start, end, block) = kept[row.index()];
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
                        let mut text =
                            egui::RichText::new(format!("0x{:016x}", block.hash)).monospace();
                        if block.changed {
                            text = text.color(CHANGED_COLOR);
                        }
                        ui.add(egui::Label::new(text).truncate());
                    });
                });
            });
    });
}

fn draw_results(ui: &mut egui::Ui, state: &AppState, actions: &mut Vec<UiAction>) {
    egui::Panel::bottom("scan-results-bottom")
        .resizable(false)
        .show(ui, |ui| {
            ui.set_min_width(ui.available_width());
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
            // 凍結→預覽型別的解讀方式; 擴展附偏移下拉
            let frozen = state.frozen_ty;
            let preview = state.scan_slot.ty(state.signed);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 4.0;
                ui.label(format!("{} → ", compact_type_label(frozen)));
                match expand_option_count(frozen, preview) {
                    Some(opts) => {
                        let off = state.expand_off.min(opts - 1);
                        egui::ComboBox::from_id_salt("expand-offset")
                            .width(80.0)
                            .selected_text(format!(
                                "{}@{}",
                                compact_type_label(preview),
                                expand_label(off)
                            ))
                            .show_ui(ui, |ui| {
                                for i in 0..opts {
                                    if ui
                                        .selectable_label(
                                            i == off,
                                            format!(
                                                "{}@{}",
                                                compact_type_label(preview),
                                                expand_label(i)
                                            ),
                                        )
                                        .clicked()
                                        && i != off
                                    {
                                        actions.push(UiAction::SetExpandOff(i));
                                    }
                                }
                            });
                        ui.label("(擴展)");
                    }
                    None => {
                        ui.label(format!(
                            "{}({})",
                            compact_type_label(preview),
                            reinterpret_method(frozen, preview)
                        ));
                    }
                }
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
        ui.label(format!("{} 個結果", state.scan.typed.len()));
    }
    let ty = state.scan_slot.ty(state.signed);
    let size = ty.size();
    let w = ui.available_width();
    let addr_w = (w * 0.36).min(200.0);
    let raw_w = (w * 0.30).min(260.0);
    // 切分後虛擬行數 (寬→窄逐入口展開投影); 其餘型別與凍結入口一對一
    let row_count = state.preview_row_count();
    // 結構解析僅支援一對一預覽 (同窗型別), 切分展開時不呈現
    let float_view = matches!(ty, ValueType::Float32 | ValueType::Double)
        && state.struct_view_kind != StructViewKind::None
        && state.preview_split() == 1;
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
                body.rows(row_h, row_count, |mut row| {
                    let idx = row.index();
                    let Some((addr, bit, window_bits, len)) = state.preview_row(idx) else {
                        return;
                    };
                    let key = crate::mem::entry_key(addr, bit);
                    let shown_bits = state.live_bits.get(&key).copied().unwrap_or(window_bits);
                    // 切換型別: 凍結解讀不同, 一律視為已變更
                    let changed = ty != state.frozen_ty || shown_bits != window_bits;
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
                            format!("0x{:012x}({})", addr, bit)
                        } else {
                            format!("0x{:012x}", addr)
                        };
                        ui.add(egui::Label::new(egui::RichText::new(text).monospace()).truncate());
                    });
                    row.col(|ui| {
                        // Bit 的值就是該位的 0/1; 原始內容欄沿用該 bit 值
                        let text = if matches!(ty, ValueType::BoolBit) {
                            format!("{}", shown_bits as u8)
                        } else if mem::is_string(ty) {
                            format!("0x{:016x}", shown_bits)
                        } else {
                            hex_bytes(shown_bits, size)
                        };
                        ui.add(egui::Label::new(egui::RichText::new(text).monospace()).truncate());
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
                                        text = text.color(if changed {
                                            CHANGED_COLOR
                                        } else {
                                            STRUCT_ANCHOR_COLOR
                                        });
                                    }
                                    ui.label(text);
                                }
                            });
                        } else if mem::is_string(ty) {
                            // 直接顯示記憶體中的字串 (只讀; 截斷以防過長)
                            let display = if len > 0 {
                                mem::hex_dump(state.pid, addr, len as usize)
                            } else {
                                Vec::new()
                            };
                            let text = if display.is_empty() {
                                format!("0x{shown_bits:016x}")
                            } else if ty == ValueType::StringUtf8 {
                                String::from_utf8_lossy(&display).chars().take(64).collect()
                            } else {
                                let units: Vec<u16> = display
                                    .chunks_exact(2)
                                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                                    .collect();
                                String::from_utf16_lossy(&units).chars().take(64).collect()
                            };
                            let text = RichText::new(text);
                            let text = if changed {
                                text.color(CHANGED_COLOR)
                            } else {
                                text
                            };
                            ui.add(egui::Label::new(text).truncate());
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
                    ui.add(
                        egui::Label::new(egui::RichText::new(format!("{addr:012x}")).monospace())
                            .truncate(),
                    );
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
        ui.label(
            RichText::new(dir.display().to_string())
                .monospace()
                .size(12.0),
        );
    });
    ui.add_space(6.0);

    if state.lua_scripts.is_empty() {
        ui.label("Lua/ 目錄為空或不存在");
        return;
    }
    ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
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
        ui.label(
            RichText::new(err)
                .monospace()
                .size(12.0)
                .color(Color32::LIGHT_RED),
        );
        ui.add_space(4.0);
    }

    // Tick 是每幀回呼, 不是可點擊的動作; 每幀重建列表故 clone 順手過濾
    let functions: Vec<String> = script
        .functions
        .iter()
        .filter(|f| *f != "Tick")
        .cloned()
        .collect();
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
                    let _ = lua_env::call(&mut state.lua_scripts[idx], name, None);
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
            draw_lua_variables_flow(ui, state, idx, &variables);
        });
    }
}

/// 變量流內容 (不管流式或垂直): 有序列表裡同前綴的 X,Y,Z 位置相鄰, 直接兩個或三個消耗
fn draw_lua_variables_flow(
    ui: &mut egui::Ui,
    state: &mut AppState,
    idx: usize,
    variables: &[String],
) {
    let mut i = 0;
    while i < variables.len() {
        if i + 1 < variables.len() {
            match vector_group(&state.lua_scripts[idx], variables, i) {
                Some(VectorGroup::Vec3(prefix, x, y, z)) => {
                    draw_lua_vector3(ui, idx, &prefix, &x, &y, &z, state);
                    i += 3;
                    continue;
                }
                Some(VectorGroup::Vec2(prefix, x, y)) => {
                    draw_lua_vector2(ui, idx, &prefix, &x, &y, state);
                    i += 2;
                    continue;
                }
                None => {}
            }
        }
        draw_lua_variable(ui, idx, &variables[i], state);
        i += 1;
    }
}

/// 變量 chip 兩段式排版: 有尺寸緩存時按緩存寬分配, 超寬換行由 horizontal_wrapped 判斷;
/// 內容有變 (changed) 時失效緩存, 下一幀重量。外框為原生 Frame::group, 不手繪框線。
/// 無緩存(如剛進頁面)時獨佔一行並量測, 配合 request_discard 令本幀重排為緩存尺寸。
/// add 在量測幀與正式幀各執行一次; 量測幀無事件輸入, 其中的 changed() 必為 false,
/// 故 set_global 等寫回只會在正式幀觸發。回傳 true 表示內容有變
fn framed_var(ui: &mut egui::Ui, id: egui::Id, add: impl FnOnce(&mut egui::Ui) -> bool) {
    if let Some(size) = ui.data_mut(|data| data.get_temp::<Vec2>(id)) {
        let changed = ui
            .allocate_ui_with_layout(size, Layout::left_to_right(Align::Center), |ui| add(ui))
            .inner;
        if changed {
            ui.data_mut(|data| data.remove::<Vec2>(id));
        }
        return;
    }
    // 無緩存(如剛進頁面): 隱形量測 — 以獨立 1e6×10k max_rect 子 Ui 排版 (sizing_pass 不消耗事件),
    // 自然寬度由 child 的 min_rect 得到; 量出外框大小後重繪, 完成後由橫向流式按緩存分配
    ui.ctx().request_discard("frame sizing pass");
    let start = ui.available_rect_before_wrap().min;
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(egui::Rect::from_min_size(
                start,
                Vec2::new(1_000_000.0, 10_000.0),
            ))
            .sizing_pass(),
    );
    let _ = add(&mut child);
    let size = child.min_rect().size();
    ui.data_mut(|data| data.insert_temp(id, size));
}

/// iOS-style toggle switch(egui 官方範例 widget,跟隨主題)
fn toggle_switch(ui: &mut egui::Ui, on: &mut bool) -> egui::Response {
    let desired_size = ui.spacing().interact_size.y * Vec2::new(2.0, 1.0);
    let (rect, mut response) = ui.allocate_exact_size(desired_size, egui::Sense::click());
    if response.clicked() {
        *on = !*on;
        response.mark_changed();
    }
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::Role::CheckBox, ui.is_enabled(), *on, ""));

    if ui.is_rect_visible(rect) {
        let how_on = ui.ctx().animate_bool_responsive(response.id, *on);
        let visuals = ui.style().interact_selectable(&response, *on);
        let rect = rect.expand(visuals.expansion);
        let radius = 0.5 * rect.height();
        ui.painter().rect(
            rect,
            radius,
            visuals.bg_fill,
            visuals.bg_stroke,
            egui::StrokeKind::Inside,
        );
        let circle_x = egui::lerp((rect.left() + radius)..=(rect.right() - radius), how_on);
        let center = egui::pos2(circle_x, rect.center().y);
        ui.painter()
            .circle(center, 0.75 * radius, visuals.bg_fill, visuals.fg_stroke);
    }

    response
}

/// Chip 內容: 原生 Frame::group 外框 + 水平排版, 回傳內部是否有變
fn var_chip(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui) -> bool) -> bool {
    Frame::group(ui.style())
        .show(ui, |ui| ui.horizontal(add).inner)
        .inner
}

/// Vector 複合型別識別結果
enum VectorGroup {
    Vec2(String, String, String),
    Vec3(String, String, String, String),
}

/// Vector 複合型別判定: 前綴同名完全一致, 後綴 X/Y/Z 階梯式候選;
/// X/Y 成對必需, X/Y 缺一整組退回普通變數(Lua 沒有複合型別, 損毀≠特權);
/// Z 全純量時優先 Vector3; Z 缺失或非純量時即 Vector2
fn vector_group(script: &lua_env::LuaScript, vars: &[String], i: usize) -> Option<VectorGroup> {
    use mlua::Value;
    fn is_scalar(script: &lua_env::LuaScript, name: &str) -> bool {
        matches!(
            script
                .lua
                .globals()
                .get::<Value>(name)
                .unwrap_or(Value::Nil),
            Value::Integer(_) | Value::Number(_)
        )
    }
    let a = vars.get(i)?;
    let prefix = a.strip_suffix('X')?;
    if prefix.is_empty() {
        return None;
    }
    let y = vars
        .get(i + 1)
        .filter(|b| b.as_str() == format!("{prefix}Y"))?;
    if !is_scalar(script, a) || !is_scalar(script, y) {
        return None;
    }
    if let Some(z) = vars
        .get(i + 2)
        .filter(|b| b.as_str() == format!("{prefix}Z"))
    {
        if is_scalar(script, z) {
            return Some(VectorGroup::Vec3(
                prefix.to_string(),
                a.clone(),
                y.clone(),
                z.clone(),
            ));
        }
    }
    Some(VectorGroup::Vec2(prefix.to_string(), a.clone(), y.clone()))
}

/// Vector 元素: 軸標籤 + DragValue, 回傳是否改變
fn vector_scalar(
    ui: &mut egui::Ui,
    script: &mut lua_env::LuaScript,
    name: &str,
    axis: &str,
) -> bool {
    use mlua::Value;
    ui.label(RichText::new(format!("{axis}:")).monospace());
    match script
        .lua
        .globals()
        .get::<Value>(name)
        .unwrap_or(Value::Nil)
    {
        Value::Integer(mut n) => {
            let response = ui.add(egui::DragValue::new(&mut n).speed(1));
            if response.changed() {
                lua_env::set_global(script, name, Value::Integer(n));
            }
            response.changed()
        }
        Value::Number(mut v) => {
            let response = ui.add(egui::DragValue::new(&mut v).speed(0.001).max_decimals(4));
            if response.changed() {
                lua_env::set_global(script, name, Value::Number(v));
            }
            response.changed()
        }
        _ => unreachable!(),
    }
}

/// Vector2 複合型別控件: 兩個純量 X/Y 同一 chip 内編輯; 仍是兩個獨立全局變數
fn draw_lua_vector2(
    ui: &mut egui::Ui,
    idx: usize,
    prefix: &str,
    x_name: &str,
    y_name: &str,
    state: &mut AppState,
) {
    let script = &mut state.lua_scripts[idx];
    let id = ui.make_persistent_id(("framed_var", script.file_name.as_str(), prefix, "v2"));
    framed_var(ui, id, |ui| {
        var_chip(ui, |ui| {
            ui.label(RichText::new(format!("{prefix}:")).monospace());
            let cx = vector_scalar(ui, script, x_name, "X");
            let cy = vector_scalar(ui, script, y_name, "Y");
            cx | cy
        })
    });
}

/// Vector3 複合型別控件: 三個純量 X/Y/Z 同一 chip 内編輯; 仍是三個獨立全局變數
fn draw_lua_vector3(
    ui: &mut egui::Ui,
    idx: usize,
    prefix: &str,
    x_name: &str,
    y_name: &str,
    z_name: &str,
    state: &mut AppState,
) {
    let script = &mut state.lua_scripts[idx];
    let id = ui.make_persistent_id(("framed_var", script.file_name.as_str(), prefix, "v3"));
    framed_var(ui, id, |ui| {
        var_chip(ui, |ui| {
            ui.label(RichText::new(format!("{prefix}:")).monospace());
            let cx = vector_scalar(ui, script, x_name, "X");
            let cy = vector_scalar(ui, script, y_name, "Y");
            let cz = vector_scalar(ui, script, z_name, "Z");
            cx | cy | cz
        })
    });
}

fn draw_lua_variable(ui: &mut egui::Ui, idx: usize, name: &str, state: &mut AppState) {
    use mlua::Value;
    let script = &mut state.lua_scripts[idx];
    let value = script
        .lua
        .globals()
        .get::<Value>(name)
        .unwrap_or(Value::Nil);
    match value {
        Value::Integer(mut x) => {
            let label = format!("{name}:");
            let id = ui.make_persistent_id(("framed_var", script.file_name.as_str(), name));
            framed_var(ui, id, |ui| {
                var_chip(ui, |ui| {
                    ui.label(RichText::new(&label).monospace());
                    let response = ui.add(egui::DragValue::new(&mut x).speed(1));
                    if response.changed() {
                        lua_env::set_global(script, name, Value::Integer(x));
                    }
                    response.changed()
                })
            });
        }
        Value::Number(mut v) => {
            let label = format!("{name}:");
            let id = ui.make_persistent_id(("framed_var", script.file_name.as_str(), name));
            framed_var(ui, id, |ui| {
                var_chip(ui, |ui| {
                    ui.label(RichText::new(&label).monospace());
                    let response =
                        ui.add(egui::DragValue::new(&mut v).speed(0.001).max_decimals(4));
                    if response.changed() {
                        lua_env::set_global(script, name, Value::Number(v));
                    }
                    response.changed()
                })
            });
        }
        Value::Boolean(mut b) => {
            let label = format!("{name}:");
            let id = ui.make_persistent_id(("framed_var", script.file_name.as_str(), name));
            framed_var(ui, id, |ui| {
                var_chip(ui, |ui| {
                    ui.label(RichText::new(&label).monospace());
                    let response = toggle_switch(ui, &mut b);
                    if response.changed() {
                        lua_env::set_global(script, name, Value::Boolean(b));
                    }
                    response.changed()
                })
            });
        }
        other => match other {
            Value::String(s) => {
                let mut v = s.to_string_lossy();
                let label = format!("{name}:");
                let id = ui.make_persistent_id(("framed_var", script.file_name.as_str(), name));
                framed_var(ui, id, |ui| {
                    var_chip(ui, |ui| {
                        ui.label(RichText::new(&label).monospace());
                        let font_id = egui::TextStyle::Body.resolve(ui.style());
                        let text_w = ui
                            .painter()
                            .layout_no_wrap(v.clone(), font_id, ui.visuals().text_color())
                            .size()
                            .x;
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut v)
                                .desired_width((text_w + 16.0).max(60.0)),
                        );
                        if response.changed() {
                            match script.lua.create_string(&v) {
                                Ok(ls) => lua_env::set_global(script, name, Value::String(ls)),
                                Err(e) => script.error = Some(format!("{name}: {e}")),
                            }
                        }
                        response.changed()
                    })
                });
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
        },
    }
}

/// Insertion indicator, derived from the active theme (no hard-coded colors).
fn insertion_stroke(ui: &egui::Ui, width: f32) -> egui::Stroke {
    egui::Stroke::new(width, ui.visuals().selection.stroke.color)
}

/// 捷徑大組順序固定為名字的 Unicode 碼位排序(A-Z 在前), 同名保序
fn sort_shortcut_groups(groups: &mut Vec<ShortcutGroup>) {
    groups.sort_by(|a, b| a.name.cmp(&b.name));
}

/// 新建大組的預設名字: 名字池 "捷徑 N" 中最小的一個未被使用的
fn fresh_group_name(groups: &[ShortcutGroup]) -> String {
    for n in 1.. {
        let name = format!("捷徑 {n}");
        if !groups.iter().any(|g| g.name == name) {
            return name;
        }
    }
    unreachable!()
}

fn draw_shortcut_layout(ui: &mut egui::Ui, state: &mut AppState, _actions: &mut Vec<UiAction>) {
    sync_shortcut_candidates(state);
    sort_shortcut_groups(&mut state.shortcut_groups);
    let mut row_from: Option<RowLocation> = None;
    let mut row_to: Option<RowLocation> = None;
    let mut chip_from: Option<ChipLocation> = None;
    let mut chip_to: Option<ChipLocation> = None;
    ui.horizontal(|ui| {
        if ui.button("+ 新增視窗").clicked() {
            let name = fresh_group_name(&state.shortcut_groups);
            state.shortcut_groups.push(ShortcutGroup::new(&name));
            state.bump();
        }
    });
    ui.add_space(4.0);
    let mut to_delete: Vec<usize> = Vec::new();
    for group_idx in 0..state.shortcut_groups.len() {
        let name_show = state.shortcut_groups[group_idx].name.clone();
        let state_free_position = state.shortcut_groups[group_idx].free_position;
        let rows: Vec<Vec<String>> = state.shortcut_groups[group_idx].rows.clone();
        let is_renaming = state.shortcut_renaming == Some(group_idx);
        let rename_focus = std::mem::take(&mut state.shortcut_rename_focus);
        let state_id = ui.make_persistent_id(("shortcut_group", &name_show));
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
                        let allow_oob = state.shortcut_groups[group_idx].allow_out_of_bounds;
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
                                    .selectable_label(free_pos, "自由視窗")
                                    .on_hover_text("勾選後懸浮窗可被拖動改變位置與大小")
                                    .clicked()
                                {
                                    flags2.borrow_mut().toggle_free_position = true;
                                }
                                ui.add_space(2.0);
                                if ui
                                    .selectable_label(allow_oob, "允許超過螢幕邊界")
                                    .on_hover_text("開啓後視窗允許停在螢幕外(配合橫向螢幕使用),旋轉時不會被回拉進螢幕")
                                    .clicked()
                                {
                                    flags2.borrow_mut().toggle_allow_out_of_bounds = true;
                                }
                                ui.add_space(2.0);
                                if ui.button("重命名").clicked() {
                                    flags2.borrow_mut().start_rename = true;
                                }
                            });
                        });
                    }
                })
                .body(|ui| {
                    if ui.button("+ 新增Row").on_hover_text("在大組內加一條空行").clicked() {
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
            state.shortcut_groups[group_idx].name = if trimmed.is_empty() {
                "未命名".to_string()
            } else {
                trimmed
            };
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
        if f.toggle_allow_out_of_bounds {
            state.shortcut_groups[group_idx].allow_out_of_bounds =
                !state.shortcut_groups[group_idx].allow_out_of_bounds;
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
                        let chip_id = egui::Id::unique((
                            "shortcut_layout_chip",
                            row_loc.group,
                            row_loc.row,
                            chip_idx,
                        ));
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
                        if let (Some(pointer), Some(hovered_payload)) =
                            (pointer, chip_response.dnd_hover_payload::<DragPayload>())
                        {
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
                    let rect = if from_bar {
                        bar_rect.unwrap_or(whole_rect)
                    } else {
                        whole_rect
                    };
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
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .show(ui, |ui| {
            draw_settings_body(ui, state, actions);
        });
}

fn draw_settings_body(ui: &mut egui::Ui, state: &mut AppState, actions: &mut Vec<UiAction>) {
    egui::CollapsingHeader::new("渲染")
        .default_open(true)
        .show(ui, |ui| {
            settings_row(ui, "後端", |ui| {
                let current = state.pending_backend.unwrap_or(state.render_backend);
                let mut backend = current;
                egui::ComboBox::from_id_salt("render-backend")
                    // width 0 = 緊貼內容, 不限制於 spacing().combo_width
                    .width(0.0)
                    .selected_text(backend.label())
                    .show_ui(ui, |ui| {
                        for option in [RenderBackend::Gpu, RenderBackend::Cpu] {
                            ui.selectable_value(&mut backend, option, option.label());
                        }
                    });
                if backend != current {
                    state.pending_backend = if backend == state.render_backend {
                        None
                    } else {
                        Some(backend)
                    };
                    state.bump();
                }
                // WSI↔CPU 生產者不能原地切換 (Mali BufferQueue 餓死, overlay 消失);
                // 這裡只排隊, 存檔後下次啟動生效
                if state.pending_backend.is_some() {
                    ui.label(RichText::new("已選擇, 重啟程序後生效").small());
                }
            });
        });

    egui::CollapsingHeader::new("介面")
        .default_open(true)
        .show(ui, |ui| {
            settings_row(ui, "介面縮放", |ui| {
                let mut zoom = state.zoom;
                let changed = ui
                    .add(
                        egui::DragValue::new(&mut zoom)
                            .speed(0.02)
                            .range(0.5..=3.0)
                            .suffix("x"),
                    )
                    .changed();
                if changed {
                    state.zoom = zoom;
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
                if ui
                    .add(egui::Label::new(item).sense(egui::Sense::click()))
                    .clicked()
                {
                    actions.push(UiAction::CopyText(format!("0x{:x}", state.module_base)));
                }
            } else {
                ui.add(egui::Label::new(item));
            }
        }
    });
}
