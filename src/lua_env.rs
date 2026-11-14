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
use std::sync::atomic::{AtomicI32, AtomicU64, Ordering};
use std::sync::Mutex;

use mlua::{Function, Lua, Value};

pub struct LuaScript {
    // 先 drop 基準(持有 ValueRef), 再 drop Lua, 順序不能反
    baseline: Vec<(String, Value)>,
    pub file_name: String,
    pub lua: Lua,
    pub functions: Vec<String>,
    pub variables: Vec<String>,
    pub error: Option<String>,
}

/// Lua 回呼在 AppState 鎖內(按鈕/ draw_lua)或鎖外(init)都可能觸發,
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

static LINES: Mutex<Vec<DrawLine>> = Mutex::new(Vec::new());

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

    lua.globals().set("theraohe", t)?;
    Ok(())
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
    let lua = Lua::new();
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
    let mut script = LuaScript {
        baseline,
        file_name,
        lua,
        functions: Vec::new(),
        variables: Vec::new(),
        error: error.or(api_error),
    };
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
            Value::Function(f) => {
                if f.info().num_params == 0 {
                    functions.push(name);
                }
            }
            _ => variables.push(name),
        }
    }
    functions.sort();
    variables.sort();
    script.functions = functions;
    script.variables = variables;
}

/// 按鈕呼叫: 無參數呼叫全局函式; 錯誤寫入 script.error 供 UI 顯示
pub fn call(script: &mut LuaScript, name: &str) {
    let result = (|| -> mlua::Result<()> {
        let func: Function = script.lua.globals().get(name)?;
        func.call::<()>(())
    })();
    match result {
        Ok(()) => {
            // 呼叫可能增刪全局, 重算反射
            refresh_reflection(script);
            script.error = None;
        }
        Err(e) => script.error = Some(format!("{name}: {e}")),
    }
}

/// Tick() 約定: 每次硬體垂直同步中斷呼叫一次;
/// 非零參/不存在則靜默跳過, 錯誤寫入 script.error 供 UI 顯示
pub fn call_tick(script: &mut LuaScript) {
    let is_zero_arg_fn = matches!(
        script.lua.globals().get::<Value>("Tick"),
        Ok(Value::Function(f)) if f.info().num_params == 0
    );
    if is_zero_arg_fn {
        call(script, "Tick");
    }
}

/// init() 約定: 宿主(egui)就緒且行程表掃描完成後自動呼叫一次;
/// 非零參/不存在則靜默跳過, 錯誤寫入 script.error 供 UI 顯示
pub fn call_init(script: &mut LuaScript) {
    let is_zero_arg_fn = matches!(
        script.lua.globals().get::<Value>("init"),
        Ok(Value::Function(f)) if f.info().num_params == 0
    );
    if is_zero_arg_fn {
        call(script, "init");
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
