//! 靜態 Toon 資料表提取管線。
//!
//! Lua 只面對已解析的資料, 從不見檔案系統; 本模組掃描 Lua/ 下的 .toon,
//! 依 scope 分成: 公域 (根目錄, stem 不對應腳本) / 私域 (Lua/<stem>/**.toon,
//! 遞歸, 僅同 stem 腳本可讀) / 排除 (根目錄 <stem>.toon, 視為持久配置, 不提取)。
//!
//! Key 比對一律歸一化字串 (Integer→十進位字串, Number→整數字樣, String 原樣)。
//! 僅支援 keyed tabular (根 key = 表名, 每個值是 keyed object); 其餘形態整表跳過並警告。

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};

use mlua::Value;

#[derive(Clone)]
enum Scalar {
    Int(i64),
    Number(f64),
    Bool(bool),
    Str(String),
    Null,
}

/// row key -> 欄位; 表名 -> 行集合
type Table = HashMap<String, Vec<(String, Scalar)>>;
type Tables = HashMap<String, Table>;

static SHARED: LazyLock<Mutex<Tables>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static PRIVATE: LazyLock<Mutex<HashMap<String, Tables>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn is_toon(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase() == "toon")
        .unwrap_or(false)
}

fn file_stem<'a>(path: &'a Path) -> &'a str {
    path.file_stem()
        .map(|s| s.to_str().unwrap_or(""))
        .unwrap_or("")
}

fn parse_file(path: &Path) -> Result<HashMap<String, Table>, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("讀取失敗: {e}"))?;
    let v: serde_json::Value =
        toon_format::decode_default(&text).map_err(|e| format!("解碼失敗: {e}"))?;
    let obj = v
        .as_object()
        .ok_or_else(|| "根節點不是物件".to_string())?;
    let mut out = HashMap::new();
    for (name, val) in obj {
        match parse_keyed_tabular(val) {
            Some(table) => {
                out.insert(name.clone(), table);
            }
            None => eprintln!("[toon] {} 的表 \"{name}\" 不是 keyed tabular, 已跳過", path.display()),
        }
    }
    Ok(out)
}

fn parse_keyed_tabular(v: &serde_json::Value) -> Option<Table> {
    let obj = v.as_object()?;
    let mut rows = HashMap::with_capacity(obj.len());
    for (row_key, rv) in obj {
        let rec = rv.as_object()?;
        let mut fields = Vec::with_capacity(rec.len());
        for (f, fv) in rec {
            fields.push((f.clone(), scalar_of(fv)));
        }
        rows.insert(row_key.clone(), fields);
    }
    Some(rows)
}

fn scalar_of(v: &serde_json::Value) -> Scalar {
    match v {
        serde_json::Value::Null => Scalar::Null,
        serde_json::Value::Bool(b) => Scalar::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                Scalar::Int(i)
            } else if let Some(u) = n.as_u64() {
                if u <= i64::MAX as u64 {
                    Scalar::Int(u as i64)
                } else {
                    Scalar::Null
                }
            } else {
                Scalar::Number(n.as_f64().unwrap_or(f64::NAN))
            }
        }
        serde_json::Value::String(s) => Scalar::Str(s.clone()),
        // 複合型別不交付給 Lua
        _ => Scalar::Null,
    }
}

/// scope 內登記: 同名表後到者忽略, 僅 console 註記
fn ingest(map: &mut Tables, scope: &str, path: &Path, tables: HashMap<String, Table>) {
    for (name, table) in tables {
        if map.contains_key(&name) {
            eprintln!("[toon] {scope}: 表 \"{name}\" 重複, 忽略 {}", path.display());
            continue;
        }
        map.insert(name, table);
    }
}

fn walk_toon(dir: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                walk_toon(&p, out);
            } else if is_toon(&p) {
                out.push(p);
            }
        }
    }
}

/// 重新掃描 Lua/: 先建出新集合, 再整體交換, Tick 中途只看到全舊或全新
pub fn refresh() {
    let Some(dir) = crate::lua_env::dir() else {
        return;
    };
    refresh_at(&dir);
}

fn refresh_at(dir: &Path) {
    let mut script_stems: HashSet<String> = HashSet::new();
    let mut root_toons: Vec<PathBuf> = Vec::new();
    let mut subdirs: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                if let Some(name) = p.file_name().and_then(|s| s.to_str()) {
                    subdirs.push((name.to_string(), p));
                }
                continue;
            }
            if !p.is_file() {
                continue;
            }
            let name = p.file_name().and_then(|s| s.to_str()).unwrap_or("");
            let ext = p
                .extension()
                .map(|e| e.to_string_lossy().to_ascii_lowercase())
                .unwrap_or_default();
            if ext == "lua" || ext == "luau" {
                script_stems.insert(crate::lua_env::stem_of(name).to_string());
            } else if is_toon(&p) {
                root_toons.push(p);
            }
        }
    }
    let mut shared: Tables = HashMap::new();
    for path in root_toons {
        // 與腳本同名的根目錄 .toon 一律排除 (持久配置, 見 save_configs)
        if script_stems.contains(file_stem(&path)) {
            continue;
        }
        match parse_file(&path) {
            Ok(tables) => ingest(&mut shared, "公域", &path, tables),
            Err(e) => eprintln!("[toon] 讀取失敗 {}: {e}", path.display()),
        }
    }
    let mut private: HashMap<String, Tables> = HashMap::new();
    for (name, path) in subdirs {
        // 子目錄 stem 必須對應腳本; 孤兒子目錄整體不可見
        if !script_stems.contains(&name) {
            continue;
        }
        let mut files = Vec::new();
        walk_toon(&path, &mut files);
        files.sort();
        let mut tables = Tables::new();
        for file in files {
            match parse_file(&file) {
                Ok(t) => ingest(&mut tables, &format!("私域[{name}]"), &file, t),
                Err(e) => eprintln!("[toon] 讀取失敗 {}: {e}", file.display()),
            }
        }
        private.insert(name, tables);
    }
    if let Ok(mut q) = SHARED.lock() {
        *q = shared;
    }
    if let Ok(mut q) = PRIVATE.lock() {
        *q = private;
    }
}

fn key_string(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.to_string_lossy()),
        Value::Integer(i) => Some(i.to_string()),
        Value::Number(n) => {
            let f = *n;
            // 僅整數字樣鍵
            if f.is_finite() && f.fract() == 0.0 && f.abs() <= i64::MAX as f64 {
                Some(format!("{f:.0}"))
            } else {
                None
            }
        }
        _ => None,
    }
}

fn query<'q>(map: &'q Tables, table: &str, key: &str, field: &str) -> Option<&'q Scalar> {
    let rows = map.get(table)?;
    let fields = rows.get(key)?;
    for (f, v) in fields {
        if f == field {
            return Some(v);
        }
    }
    None
}

/// 公開查找: 私域(caller)優先, 再公域; 任何語意缺失都回 nil
pub fn lookup_lua(
    lua: &mlua::Lua,
    caller_stem: &str,
    table: &str,
    key: &Value,
    field: &str,
) -> Value {
    let Some(ks) = key_string(key) else {
        return Value::Nil;
    };
    let hit = (|| {
        if let Ok(q) = PRIVATE.lock() {
            if let Some(map) = q.get(caller_stem) {
                if let Some(v) = query(map, table, &ks, field) {
                    return Some(v.clone());
                }
            }
        }
        if let Ok(q) = SHARED.lock() {
            if let Some(v) = query(&q, table, &ks, field) {
                return Some(v.clone());
            }
        }
        None
    })();
    match hit {
        None | Some(Scalar::Null) => Value::Nil,
        Some(Scalar::Int(i)) => Value::Integer(i as mlua::Integer),
        Some(Scalar::Number(n)) => Value::Number(n),
        Some(Scalar::Bool(b)) => Value::Boolean(b),
        Some(Scalar::Str(s)) => lua
            .create_string(&s)
            .map(Value::String)
            .unwrap_or(Value::Nil),
    }
}