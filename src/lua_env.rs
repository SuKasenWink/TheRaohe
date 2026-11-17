//! 內嵌 Lua 執行環境 (PUC-Rio 5.4, vendored)。
//! 腳本來源: 二進制同目錄下的 Lua/ 資料夾, 每個 .lua 一個獨立 Lua 狀態,
//! 腳本之間互不影響; print 寫到 stdout, 由終端台截獲顯示。
//!
//! 反射契約 (載入時 + 每次呼叫/寫回後重算):
//! - globals 裡且非載入前基準(內建或腳本當時狀態)相同項, 且是無參數函式 → 按鈕
//! - 同上, 但不是函式 → 變量控件
//! 基準以同狀態 globals 的快照逐項比較, 故被腳本覆寫的內建名也會被反射出來

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, AtomicU32, AtomicU64, Ordering};
use std::sync::Mutex;

use mlua::{Function, Lua, Value};
use serde::{Deserialize, Serialize};

// 自瞄專用虛擬手指: 固定佔用第 9 槽 (1<<9), 不與使用者常用的低位槽競爭
const AIM_TOUCH_SLOT: u16 = 1 << 9;

pub struct LuaScript {
    // 先 drop 基準(持有 ValueRef), 再 drop Lua, 順序不能反
    baseline: Vec<(String, Value)>,
    /// 腳本自身的默認值: exec 之後、apply_config 覆蓋 toon 之前的全域快照;
    /// 「僅變化才記錄」以它為基準 — 腳本自建的變量自帶的值不算差異, 只有使用者真正改過的才入檔
    defaults: Vec<(String, Value)>,
    pub file_name: String,
    pub lua: Lua,
    pub functions: Vec<String>,
    pub variables: Vec<String>,
    pub error: Option<String>,
}

/// Lua 回呼在 AppState 鎖內(按鈕/ draw_lua)或鎖外(Begin)都可能觸發,
/// 直接操作 AppState 會重入死鎖; 一律入隊, 由 UI 側在安全點排空套用
pub enum LuaCmd {
    SelectName(String),
    SelectPid(i32),
}

/// Lua 立即模式畫線: 線只存活到本幀結束; 腳本每 Tick 重新畫 (ESP 座標每幀都動)
/// color 為直通 sRGB 的 RGBA 位元組
#[derive(Clone, Copy)]
pub struct DrawLine {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
    pub color: [u8; 4],
}

/// Lua 立即模式畫文本: 直接畫到螢幕畫布背景層, 不附加任何窗口/佈局;
/// 該次傳入只活到本幀結束, 腳本每 Tick 重畫。align_x/align_y 0..1 (0=文字左上貼錨點,
/// 1=文字右下貼錨點, 0.5=置中); color 為 sRGB RGBA 位元組 (a 省略為 255);
/// style: 0=Small (egui TextStyle::Small), 1=Body, 2=Heading (全省略 = 0)
#[derive(Clone)]
pub struct DrawText {
    pub x: f32,
    pub y: f32,
    pub align_x: f32,
    pub align_y: f32,
    pub color: [u8; 4],
    pub style: u8,
    pub text: String,
}

static LINES: Mutex<Vec<DrawLine>> = Mutex::new(Vec::new());
static TEXTS: Mutex<Vec<DrawText>> = Mutex::new(Vec::new());

/// 排空由 UI 側呼叫: render() 每幀 + AppState::new 初始化後
static CMDS: Mutex<Vec<LuaCmd>> = Mutex::new(Vec::new());

/// 當前選取的行程 pid; 0 表示未選擇, 所有記憶體讀取直接回 nil
static TARGET_PID: AtomicI32 = AtomicI32::new(0);
/// 當前選取的主模組 base; module_base() 無參數時回傳
static TARGET_BASE: AtomicU64 = AtomicU64::new(0);
/// 行程映射索引 (小寫 path -> { 庫 base (優先 offset==0 的映射) }), attach 時重建
static MODULE_INDEX: Mutex<Vec<(String, u64)>> = Mutex::new(Vec::new());

fn enqueue(cmd: LuaCmd) {
    if let Ok(mut q) = CMDS.lock() {
        q.push(cmd);
    }
}

/// 在 render_frame / draw 前刷新目標行程資訊; 讀取類 API 只讀快照, 不觸碰 AppState 鎖
pub fn set_target_info(pid: i32, module_base: u64) {
    TARGET_PID.store(pid, Ordering::Relaxed);
    TARGET_BASE.store(module_base, Ordering::Relaxed);
}

/// 由 attach / AppState::new 呼叫: 把行程映射壓成 庫 base 索引
pub fn set_module_index(maps: &[crate::maps::MapRegion]) {
    let mut by_path: HashMap<String, (Option<u64>, u64)> = HashMap::new();
    for m in maps {
        if m.path.is_empty() {
            continue;
        }
        let key = m.path.to_ascii_lowercase();
        let e = by_path.entry(key).or_insert((None, m.start));
        if m.offset == 0 {
            e.0 = Some(m.start);
        }
        e.1 = e.1.min(m.start);
    }
    let index = by_path
        .into_iter()
        .map(|(k, (o0, mn))| (k, o0.unwrap_or(mn)))
        .collect();
    if let Ok(mut q) = MODULE_INDEX.lock() {
        *q = index;
    }
}

pub fn drain_cmds() -> Vec<LuaCmd> {
    CMDS.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
}

pub fn take_lines() -> Vec<DrawLine> {
    LINES.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
}

pub fn take_texts() -> Vec<DrawText> {
    TEXTS.lock().map(|mut q| std::mem::take(&mut *q)).unwrap_or_default()
}

/// 庫 base 查找: path 含 needle (須已小寫) 的庫 base, 優先 offset==0 的映射
fn module_base_lookup(needle: &str) -> Option<u64> {
    MODULE_INDEX.lock().ok().and_then(|q| {
        q.iter()
            .find(|(path, _)| path.contains(needle))
            .map(|(_, b)| *b)
    })
}

/// 讀取類 API: pid<=0 直接回 nil; 讀取失敗亦回 nil, 不拋錯
/// (腳本可安心 `if not addr then return end`, 遊戲未啟動時靜默重試)
fn read_into(pid: i32, addr: u64, buf: &mut [u8]) -> bool {
    pid > 0 && crate::nb9::read(pid, addr, buf)
}

fn register_api(lua: &Lua) -> mlua::Result<()> {
    let t = lua.create_table()?;
    let select = lua.create_function(|_, arg: Value| -> mlua::Result<()> {
        match arg {
            Value::String(s) => enqueue(LuaCmd::SelectName(s.to_string_lossy())),
            Value::Integer(i) => enqueue(LuaCmd::SelectPid(i as i32)),
            Value::Number(n) if n.fract() == 0.0 => enqueue(LuaCmd::SelectPid(n as i32)),
            _ => {
                return Err(mlua::Error::external(
                    "select 需要字串(行程名)或整數(pid)",
                ))
            }
        }
        Ok(())
    })?;
    t.set("select", select)?;

    t.set(
        "pid",
        lua.create_function(|_, ()| Ok(TARGET_PID.load(Ordering::Relaxed)))?,
    )?;

    // 無參數: 主模組 base; 字串參數: path 含該字串(小寫)的庫 base, 優先 offset==0 的映射
    let module_base =
        lua.create_function(|_, pattern: Option<String>| -> mlua::Result<u64> {
            match pattern {
                None => Ok(TARGET_BASE.load(Ordering::Relaxed)),
                Some(p) => Ok(module_base_lookup(&p.to_ascii_lowercase()).unwrap_or(0)),
            }
        })?;
    t.set("module_base", module_base)?;

    // CE 指標鏈語法: '[' expr ']' 解參考一次, 數字一律十六進位 (可選 0x 前綴), 模組名必須加引號
    // 整條鏈在宿主側解析並走訪, 成功回傳最終指標值 (Integer); 模組缺失或任何一層讀取失敗回 nil
    // (語法錯誤是腳本 bug, 直接拋出 mlua 錯誤並帶上位置)
    let resolve_chain = lua.create_function(|_, chain: String| -> mlua::Result<Value> {
        let pid = TARGET_PID.load(Ordering::Relaxed);
        let parsed = crate::mem::parse_chain(&chain)
            .map_err(|e| mlua::Error::external(format!("指標鏈解析失敗: {e}")))?;
        let resolved = crate::mem::resolve(
            &parsed,
            |name| {
                if pid > 0 {
                    module_base_lookup(&name.to_ascii_lowercase())
                } else {
                    None
                }
            },
            |addr| {
                if pid > 0 {
                    crate::nb9::read_value(pid, addr, 8)
                } else {
                    None
                }
            },
        );
        match resolved {
            Ok(addr) => Ok(Value::Integer(addr as mlua::Integer)),
            Err(_) => Ok(Value::Nil),
        }
    })?;
    t.set("resolve_chain", resolve_chain)?;

    // 立即模式畫線: 線只存活到本幀結束, 腳本每 Tick 重畫
    // 座標為物理像素; r/g/b/a 0-255, a 省略時 255; 非有限數值與越界直接鉗位
    let draw_line = lua.create_function(
        |_, (x0, y0, x1, y1, r, g, b, a): (f64, f64, f64, f64, f64, f64, f64, Option<f64>)| {
            let chan = |v: f64| v.clamp(0.0, 255.0).round() as u8;
            if [x0, y0, x1, y1].iter().all(|v| v.is_finite()) {
                if let Ok(mut q) = LINES.lock() {
                    q.push(DrawLine {
                        x0: x0 as f32,
                        y0: y0 as f32,
                        x1: x1 as f32,
                        y1: y1 as f32,
                        color: [chan(r), chan(g), chan(b), chan(a.unwrap_or(255.0))],
                    });
                }
            }
            Ok(())
        },
    )?;
    t.set("draw_line", draw_line)?;

    // 立即模式畫文本: 文本只存活到本幀結束, 腳本每 Tick 重畫; 直接畫在螢幕畫布背景層,
    // 不附加任何窗口/佈局。座標為物理像素; align_* 鉗位到 0..1 (見 DrawText 註釋);
    // r/g/b/a 0-255 向後可選, 僅省略尾部 a 為 255; style 可選: 0=Small 1=Body 2=Heading
    let draw_text = lua.create_function(
        |_, (x, y, align_x, align_y, text, r, g, b, a, style): (f64, f64, f64, f64, String, Option<f64>, Option<f64>, Option<f64>, Option<f64>, Option<u8>)| {
            let chan = |v: f64| v.clamp(0.0, 255.0).round() as u8;
            let ax = align_x.clamp(0.0, 1.0) as f32;
            let ay = align_y.clamp(0.0, 1.0) as f32;
            let color = [
                chan(r.unwrap_or(255.0)),
                chan(g.unwrap_or(255.0)),
                chan(b.unwrap_or(255.0)),
                chan(a.unwrap_or(255.0)),
            ];
            if x.is_finite() && y.is_finite() {
                if let Ok(mut q) = TEXTS.lock() {
                    q.push(DrawText {
                        x: x as f32,
                        y: y as f32,
                        align_x: ax,
                        align_y: ay,
                        color,
                        style: style.unwrap_or(0).clamp(0, 2),
                        text,
                    });
                }
            }
            Ok(())
        },
    )?;
    t.set("draw_text", draw_text)?;

    macro_rules! read_int {
        ($name:literal, $size:expr, $ty:ty) => {{
            let f = lua.create_function(|_, addr: u64| -> mlua::Result<Value> {
                let pid = TARGET_PID.load(Ordering::Relaxed);
                let mut buf = [0u8; $size];
                if !read_into(pid, addr, &mut buf) {
                    return Ok(Value::Nil);
                }
                Ok(Value::Integer(<$ty>::from_le_bytes(buf) as mlua::Integer))
            })?;
            t.set($name, f)?;
        }};
    }
    read_int!("read_u8", 1, u8);
    read_int!("read_u16", 2, u16);
    read_int!("read_u32", 4, u32);
    read_int!("read_u64", 8, u64);
    read_int!("read_i8", 1, i8);
    read_int!("read_i16", 2, i16);
    read_int!("read_i32", 4, i32);
    read_int!("read_i64", 8, i64);

    let read_f32 = lua.create_function(|_, addr: u64| -> mlua::Result<Value> {
        let pid = TARGET_PID.load(Ordering::Relaxed);
        let mut buf = [0u8; 4];
        if !read_into(pid, addr, &mut buf) {
            return Ok(Value::Nil);
        }
        Ok(Value::Number(f32::from_le_bytes(buf) as f64))
    })?;
    t.set("read_f32", read_f32)?;
    let read_f64 = lua.create_function(|_, addr: u64| -> mlua::Result<Value> {
        let pid = TARGET_PID.load(Ordering::Relaxed);
        let mut buf = [0u8; 8];
        if !read_into(pid, addr, &mut buf) {
            return Ok(Value::Nil);
        }
        Ok(Value::Number(f64::from_le_bytes(buf)))
    })?;
    t.set("read_f64", read_f64)?;

    let read_bool = lua.create_function(|_, addr: u64| -> mlua::Result<Value> {
        let pid = TARGET_PID.load(Ordering::Relaxed);
        let mut buf = [0u8; 1];
        if !read_into(pid, addr, &mut buf) {
            return Ok(Value::Nil);
        }
        Ok(Value::Boolean(buf[0] != 0))
    })?;
    t.set("read_bool", read_bool)?;

    // 位元組上限 4096, 防止腳本讀出超大塊記憶體進 Lua
    let read_bytes = lua.create_function(|lua, (addr, len): (u64, usize)| -> mlua::Result<Value> {
        let pid = TARGET_PID.load(Ordering::Relaxed);
        let len = len.min(4096);
        let mut buf = vec![0u8; len];
        if !read_into(pid, addr, &mut buf) {
            return Ok(Value::Nil);
        }
        Ok(Value::String(lua.create_string(&buf)?))
    })?;
    t.set("read_bytes", read_bytes)?;

    // 物理像素寬高; 讀取類契約一致: 失敗回兩個 nil, 不拋錯
    let screen_size = lua.create_function(|_, ()| -> mlua::Result<(Value, Value)> {
        Ok(match crate::display::screen_wh() {
            Some((w, h)) => (
                Value::Integer(w as mlua::Integer),
                Value::Integer(h as mlua::Integer),
            ),
            None => (Value::Nil, Value::Nil),
        })
    })?;
    t.set("screen_size", screen_size)?;

    // 單調時鐘: 啟動至今的秒數 (f64); 腳本內部定時用 (連點間隙), 不做掛鐘時間
    static EPOCH: std::sync::OnceLock<std::time::Instant> = std::sync::OnceLock::new();
    let now = lua.create_function(|_, ()| -> mlua::Result<f64> {
        Ok(EPOCH.get_or_init(std::time::Instant::now).elapsed().as_secs_f64())
    })?;
    t.set("now", now)?;

    // 虛擬手指: 屏幕物理像素座標, 絕對定位; driver 佈局依當前 rotation 決定 (init 會隨之重設)
    // 第三參數 finger = 手指槽索引 0..9, 省略 = 自瞄槽 9; 呼叫成功回傳 true, 目標未就緒/驅動拒絕回傳 false
    // 0 = 未初始化; 非零 = 當前已 init 的 rotation
    static AIM_TOUCH_LAYOUT: AtomicU32 = AtomicU32::new(0);
    fn ensure_touch_init(rot: u32) -> bool {
        if AIM_TOUCH_LAYOUT.load(Ordering::Relaxed) == rot {
            return true;
        }
        let Some((w, h)) = crate::display::screen_wh() else {
            return false;
        };
        // rotation=3 已對齊, rotation=1 只改了 X 軸 (xi=1080-y); 兩者的觸控範圍都是 (1080,2400)
        let ok = crate::nb9::touch_init(h, w);
        if ok {
            AIM_TOUCH_LAYOUT.store(rot, Ordering::Relaxed);
        }
        ok
    }
    // rotation=3 (充電口在左) 已對齊: 軸向互換 + X 軸反彈;
    // rotation=1 (充電口在右) 直送原始值, 待使用者確認偏移方向後再精修
    // 其餘 rotation 的觸控不輸出
    const AIM_ALLOWED_ROTATION: u32 = 3;
    const AIM_RAW_ROTATION: u32 = 1;
    let mk_touch = |cmd: u32| {
        lua.create_function(move |_, (x, y, finger): (f64, f64, Option<u8>)| -> mlua::Result<bool> {
            if !x.is_finite() || !y.is_finite() {
                return Ok(false);
            }
            let rot = crate::display::rotation();
            // 橫向尺寸每次呼叫都現取: 方向一旦改變, 不能被腳本載入時刻的快照拖住
            let (sw, sh) = crate::display::screen_wh().unwrap_or((2400, 1080));
            let pt_w = f64::from(sw);
            let pt_h = f64::from(sh);
            // 手指槽索引 0..9 (省略 = 自瞄槽 9); 傳錯回傳 false 靜默
            let slot_mask = match finger {
                Some(f) if u16::from(f) < crate::nb9::NB9_TOUCH_SLOTS => 1u16 << f,
                Some(_) => return Ok(false),
                None => AIM_TOUCH_SLOT,
            };
            let (xi, yi) = match rot {
                AIM_ALLOWED_ROTATION => (
                    y.clamp(0.0, f64::from(u32::MAX)) as u32,
                    (pt_w - x).clamp(0.0, f64::from(u32::MAX)) as u32,
                ),
                AIM_RAW_ROTATION => (
                    // X = 未旋轉螢幕闊度 (1080) - Lua Y; Y 軸未調校
                    (pt_h - y).clamp(0.0, f64::from(u32::MAX)) as u32,
                    x.clamp(0.0, f64::from(u32::MAX)) as u32,
                ),
                _ => return Ok(false),
            };
            Ok(ensure_touch_init(rot) && crate::nb9::touch_event(cmd, xi, yi, slot_mask))
        })
    };
    t.set("touch_down", mk_touch(crate::nb9::TOUCH_DOWN)?)?;
    t.set("touch_move", mk_touch(crate::nb9::TOUCH_MOVE)?)?;
    t.set("touch_up", mk_touch(crate::nb9::TOUCH_UP)?)?;

    lua.globals().set("theraohe", t)?;
    Ok(())
}

/// Lua 設置持久化: 每個腳本一份 `Lua/<腳本名>.toon`, 僅存反射變量(Integer/Number/Boolean/String)
/// 與腳本自身默認不同的值; 由 lua_env 內部讀寫, 不給 Lua 開放檔案能力
#[derive(Serialize, Deserialize)]
struct PersistedLuaConfig {
    variables: Vec<PersistedLuaVar>,
}

#[derive(Serialize, Deserialize)]
struct PersistedLuaVar {
    name: String,
    value: LuaScalar,
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(untagged)]
enum LuaScalar {
    Int(i64),
    Float(f64),
    Bool(bool),
    String(String),
}

fn config_path(dir: &std::path::Path, file_name: &str) -> PathBuf {
    let stem = file_name.strip_suffix(".lua").unwrap_or(file_name);
    dir.join(format!("{stem}.toon"))
}

/// 載入腳本後覆蓋其全域變量: 只對載入後仍然存在的 Integer/Number/Boolean/String globals 生效,
/// 過期條目靜默忽略, 下次存檔時自然消除
fn apply_config(script: &mut LuaScript) {
    let Some(dir) = dir() else {
        return;
    };
    let path = config_path(&dir, &script.file_name);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(_) => return,
    };
    if text.trim().is_empty() {
        return;
    }
    match toon_format::decode_default::<PersistedLuaConfig>(&text) {
        Ok(cfg) => {
            for var in cfg.variables {
                // Integer/Number globals 對 Int/Float scalar 兩向相容 (腳本型別決定不了值型別);
                // Boolean/String 嚴格對型
                let ok = match (script.lua.globals().get::<Value>(var.name.as_str()), &var.value) {
                    (Ok(Value::Integer(_) | Value::Number(_)), LuaScalar::Int(x)) => {
                        script.lua.globals().set(var.name.as_str(), *x)
                    }
                    (Ok(Value::Integer(_) | Value::Number(_)), LuaScalar::Float(x)) => {
                        script.lua.globals().set(var.name.as_str(), *x)
                    }
                    (Ok(Value::Boolean(_)), LuaScalar::Bool(x)) => {
                        script.lua.globals().set(var.name.as_str(), *x)
                    }
                    (Ok(Value::String(_)), LuaScalar::String(x)) => {
                        match script.lua.create_string(x) {
                            Ok(s) => script.lua.globals().set(var.name.as_str(), s),
                            Err(e) => {
                                script.error = Some(format!("{name}: {e}", name = var.name));
                                continue;
                            }
                        }
                    }
                    _ => continue,
                };
                if let Err(e) = ok {
                    eprintln!("[lua] 配置寫回失敗 {}: {e}", path.display());
                }
            }
        }
        Err(e) => eprintln!("[lua] 配置解析失敗 {}: {e}", path.display()),
    }
}

pub(crate) fn save_configs(scripts: &[LuaScript]) {
    let Some(dir) = dir() else {
        return;
    };
    for script in scripts {
        let path = config_path(&dir, &script.file_name);
        let mut vars: Vec<PersistedLuaVar> = Vec::new();
        for pair in script.lua.globals().pairs::<Value, Value>() {
            let Ok((Value::String(k), v)) = pair else {
                continue;
            };
            let scalar = match &v {
                Value::Integer(x) => LuaScalar::Int(*x),
                Value::Number(x) => LuaScalar::Float(*x),
                Value::Boolean(x) => LuaScalar::Bool(*x),
                Value::String(x) => LuaScalar::String(x.to_string_lossy()),
                _ => continue,
            };
            let name = k.to_string_lossy();
            // 與腳本自身默認相同 → 不記錄(使用者沒改過, 也未必曾記錄進 toon)
            let unchanged = script
                .defaults
                .iter()
                .any(|(bk, bv)| *bk == name && *bv == v);
            if unchanged {
                continue;
            }
            vars.push(PersistedLuaVar { name, value: scalar });
        }
        // 沒有自定義 → 不留文件
        if vars.is_empty() {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        vars.sort_by(|a, b| a.name.cmp(&b.name));
        let cfg = PersistedLuaConfig { variables: vars };
        match toon_format::encode_default(&cfg) {
            Ok(text) => {
                let tmp = path.with_extension("toon.tmp");
                match std::fs::write(&tmp, &text).and_then(|()| std::fs::rename(&tmp, &path)) {
                    Ok(()) => (),
                    Err(e) => eprintln!("[lua] 配置寫入失敗 {}: {e}", path.display()),
                }
            }
            Err(e) => eprintln!("[lua] 配置編碼失敗 {}: {e}", path.display()),
        }
    }
}

pub fn dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    Some(exe.parent()?.join("Lua"))
}

pub fn load_all() -> Vec<LuaScript> {
    let Some(dir) = dir() else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = match std::fs::read_dir(&dir) {
        Ok(rd) => rd
            .flatten()
            .map(|e| e.path())
            .filter(|p| {
                p.is_file()
                    && p.extension()
                        .map(|e| e.eq_ignore_ascii_case("lua"))
                        .unwrap_or(false)
            })
            .collect(),
        Err(_) => return Vec::new(),
    };
    paths.sort();
    paths.into_iter().map(|p| load_one(&p)).collect()
}

fn load_one(path: &std::path::Path) -> LuaScript {
    let file_name = path
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "?.lua".to_string());
    // 腳本跑在系統最高權限, 且能寫回本行程記憶體, 「沙盒」不是安全邊界;
    // 需要 debug.getlocal 讀形參名 (xy 函式反射), 因此連同 unsafe DEBUG 一起開
    let lua = unsafe {
        Lua::unsafe_new_with(
            mlua::StdLib::ALL_SAFE | mlua::StdLib::DEBUG,
            mlua::LuaOptions::default(),
        )
    };
    let api_error = register_api(&lua).err().map(|e| format!("內建 API 註冊失敗: {e}"));
    let baseline = snapshot_globals(&lua);
    // lua.load 不經過 luaL_loadfilex, 需自行剝 UTF-8 BOM, 否則記事本等編輯器存的檔直接語法錯誤
    let error = std::fs::read_to_string(path)
        .map_err(|e| format!("讀取失敗: {e}"))
        .and_then(|src| {
            lua.load(src.strip_prefix('\u{feff}').unwrap_or(&src))
                .set_name(&file_name)
                .exec()
                .map_err(|e| e.to_string())
        })
        .err();
    if let Some(e) = error.as_deref() {
        eprintln!("[lua] {file_name} 載入失敗: {e}");
    }
    if let Some(e) = api_error.as_deref() {
        eprintln!("[lua] {file_name} {e}");
    }
    // 腳本自身的默認值在 exec 之後、apply_config 之前拍快照:
    // 它既包含腳本自建變量的字面默認, 也包含完整的 stdlib; 與 baseline 的作用不同
    let defaults = snapshot_globals(&lua);
    let mut script = LuaScript {
        baseline,
        defaults,
        file_name,
        lua,
        functions: Vec::new(),
        variables: Vec::new(),
        error: error.or(api_error),
    };
    apply_config(&mut script);
    refresh_reflection(&mut script);
    script
}

fn snapshot_globals(lua: &Lua) -> Vec<(String, Value)> {
    lua.globals()
        .pairs::<Value, Value>()
        .filter_map(|pair| pair.ok())
        .filter_map(|(k, v)| match k {
            Value::String(s) => Some((s.to_string_lossy(), v)),
            _ => None,
        })
        .collect()
}

// xy 函式判定 (Luau): Luau 沒有參數名 introspection (debug.getlocal 不存在), 只能按參數數量 — 兩參數即接落點候選
fn is_xy_signature(lua: &Lua, f: &Function) -> bool {
    const CHUNK: &str = r#"
        local f = ...
        return debug.info(f, "a") == 2
    "#;
    lua.load(CHUNK).call::<bool>((f,)).unwrap_or(false)
}

fn refresh_reflection(script: &mut LuaScript) {
    let current = snapshot_globals(&script.lua);
    let mut functions = Vec::new();
    let mut variables = Vec::new();
    for (name, value) in current {
        let unchanged = script
            .baseline
            .iter()
            .any(|(bk, bv)| *bk == name && *bv == value);
        if unchanged {
            continue;
        }
        match &value {
            Value::Function(f) => match f.info().num_params {
                0 => functions.push(name),
                2 if is_xy_signature(&script.lua, f) => functions.push(name),
                _ => {}
            },
            _ => variables.push(name),
        }
    }
    functions.sort();
    variables.sort();
    script.functions = functions;
    script.variables = variables;
}

/// 按鈕呼叫: 依 num_params 決定傳參 (0 → 無參; 2 → 若給了 (x, y) 帶 xy, 否則無參);
/// 成功回函式 reply; 失敗回 None 且 script.error 設值 (Lua 頁可見)
pub fn call(script: &mut LuaScript, name: &str, args: Option<(f64, f64)>) -> Option<Value> {
    let result = (|| -> mlua::Result<Value> {
        let func: Function = script.lua.globals().get(name)?;
        match func.info().num_params {
            0 => func.call::<Value>(()),
            2 => match args {
                Some((x, y)) => func.call::<Value>((x, y)),
                None => func.call::<Value>(()),
            },
            _ => Err(mlua::Error::RuntimeError(format!(
                "{name} 參數個數不受支援 (需 0 或 x,y)"
            ))),
        }
    })();
    match result {
        Ok(v) => {
            // 呼叫可能增刪全局, 重算反射
            refresh_reflection(script);
            script.error = None;
            Some(v)
        }
        Err(e) => {
            script.error = Some(format!("{name}: {e}"));
            None
        }
    }
}

/// 0 參函式判定 (開關式捷徑探測只對 0 參函式做無參呼叫)
pub fn is_zero_arg_function(script: &LuaScript, name: &str) -> bool {
    matches!(
        script.lua.globals().get::<Value>(name),
        Ok(Value::Function(f)) if f.info().num_params == 0
    )
}

/// Tick() 約定: 每次硬體垂直同步中斷呼叫一次;
/// 非零參/不存在則靜默跳過, 錯誤寫入 script.error 供 UI 顯示
pub fn call_tick(script: &mut LuaScript) {
    if call_zero_arg(script, "Tick") {
        // 反射已刷新
    }
}

/// Begin() 約定: 宿主(egui)就緒且行程表掃描完成後自動呼叫一次;
/// 非零參/不存在則靜默跳過, 錯誤寫入 script.error 供 UI 顯示
pub fn call_begin(script: &mut LuaScript) {
    call_zero_arg(script, "Begin");
}

fn call_zero_arg(script: &mut LuaScript, name: &str) -> bool {
    let is_zero_arg_fn = matches!(
        script.lua.globals().get::<Value>(name),
        Ok(Value::Function(f)) if f.info().num_params == 0
    );
    if is_zero_arg_fn {
        let _ = call(script, name, None);
        true
    } else {
        false
    }
}

pub fn set_global(script: &mut LuaScript, name: &str, value: Value) {
    match script.lua.globals().set(name, value) {
        Ok(()) => {
            refresh_reflection(script);
            script.error = None;
        }
        Err(e) => script.error = Some(format!("{name}: {e}")),
    }
}

