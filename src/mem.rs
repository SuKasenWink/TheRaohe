use crate::nb9;
use std::cmp::Ordering;
use std::fs;
use std::path::Path;

#[derive(Clone)]
pub struct ProcInfo {
    pub pid: i32,
    pub name: String,
    pub system: bool,
}

#[derive(Clone)]
pub struct MapRegion {
    pub start: u64,
    pub end: u64,
    pub offset: u64,
    pub perms: String,
    pub path: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ValueType {
    Int32,
    UInt32,
    Int64,
    UInt64,
    Float32,
    Double,
    Bool,
    BoolBit,
    UInt8,
    Ptr,
    Int16,
    UInt16,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ChangeMode {
    ExactValue,
    AnyChange,
    Unchanged,
    Increased,
    Decreased,
    IncreasedBy,
    DecreasedBy,
}

#[derive(Clone, Copy)]
pub struct ResultEntry {
    pub addr: u64,
    pub bits: u64,
}

impl ValueType {
    pub fn size(self) -> usize {
        match self {
            ValueType::Int32 | ValueType::UInt32 | ValueType::Float32 => 4,
            ValueType::Int64 | ValueType::UInt64 | ValueType::Double | ValueType::Ptr => 8,
            ValueType::Bool | ValueType::BoolBit | ValueType::UInt8 => 1,
            ValueType::Int16 | ValueType::UInt16 => 2,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ValueType::Int32 => "I32",
            ValueType::UInt32 => "U32",
            ValueType::Int64 => "I64",
            ValueType::UInt64 => "U64",
            ValueType::Float32 => "F32",
            ValueType::Double => "F64",
            ValueType::Bool => "Bool",
            ValueType::BoolBit => "Bit",
            ValueType::UInt8 => "U8",
            ValueType::Ptr => "Ptr",
            ValueType::Int16 => "I16",
            ValueType::UInt16 => "U16",
        }
    }

    pub fn parse(self, text: &str) -> Option<u64> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        match self {
            ValueType::Int32 => text.parse::<i32>().ok().map(|v| v as u32 as u64),
            ValueType::UInt32 => parse_u64(text).map(|v| v as u32 as u64),
            ValueType::Int64 => text.parse::<i64>().ok().map(|v| v as u64),
            ValueType::UInt64 => parse_u64(text),
            ValueType::Float32 => text.parse::<f32>().ok().map(|v| v.to_bits() as u64),
            ValueType::Double => text.parse::<f64>().ok().map(|v| v.to_bits()),
            ValueType::Bool => text.parse::<u8>().ok().map(|v| v as u64),
            ValueType::BoolBit => text.parse::<u8>().ok().map(|v| v as u64),
            ValueType::UInt8 => text.parse::<u8>().ok().map(|v| v as u64),
            ValueType::Ptr => parse_u64(text),
            ValueType::Int16 => text.parse::<i16>().ok().map(|v| v as u16 as u64),
            ValueType::UInt16 => text.parse::<u16>().ok().map(|v| v as u64),
        }
    }

    pub fn format(self, bits: u64) -> String {
        match self {
            ValueType::Int32 => format!("{}", bits as u32 as i32),
            ValueType::UInt32 => format!("{}", bits as u32),
            ValueType::Int64 => format!("{}", bits as i64),
            ValueType::UInt64 => format!("{bits}"),
            ValueType::Float32 => format!("{:.4}", f32::from_bits(bits as u32)),
            ValueType::Double => format!("{:.6}", f64::from_bits(bits)),
            ValueType::Bool => format!("{}", bits as u8 != 0),
            ValueType::BoolBit => format!("{}", bits as u8),
            ValueType::UInt8 => format!("{}", bits as u8),
            ValueType::Ptr => format!("0x{bits:x}"),
            ValueType::Int16 => format!("{}", bits as u16 as i16),
            ValueType::UInt16 => format!("{}", bits as u16),
        }
    }
}

impl ChangeMode {

    pub fn label(self) -> &'static str {
        match self {
            ChangeMode::ExactValue => "目標值：",
            ChangeMode::AnyChange => "任意變化",
            ChangeMode::Unchanged => "原樣",
            ChangeMode::Increased => "任意增大",
            ChangeMode::Decreased => "任意減少",
            ChangeMode::IncreasedBy => "增加了：",
            ChangeMode::DecreasedBy => "減少了：",
        }
    }
}

fn parse_u64(text: &str) -> Option<u64> {
    if let Some(hex) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()
    } else {
        text.parse::<u64>().ok()
    }
}

// 用 `pm list packages -f` 只留 APK 路徑在 /data 分區的套件, 即「用戶安裝的應用」;
// /system、/product、/system_ext、/vendor、/apex 下的是系統映像內建, 視為系統行程。
// 拿不到清單時退回 UID < 10000 的 AID_* 判斷, 至少把 root/system/daemon 濾掉
fn installed_packages() -> std::collections::HashSet<String> {
    std::process::Command::new("cmd")
        .args(["package", "list", "packages", "-f"])
        .output()
        .ok()
        .map(|out| {
            String::from_utf8_lossy(&out.stdout)
                .lines()
                .filter_map(|line| {
                    let rest = line.strip_prefix("package:")?;
                    let (path, pkg) = rest.rsplit_once('=')?;
                    path.starts_with("/data/").then(|| pkg.trim().to_string())
                })
                .collect()
        })
        .unwrap_or_default()
}

// 系統行程判定: UID < 10000 的 AID_* 保留區 (root/system/vendor/shell 等, 含 kernel thread);
// 讀不到 status 的多半是剛退出的行程, 一律視為系統行程以免雜訊
fn is_system_process(pid: i32) -> bool {
    let Ok(status) = fs::read_to_string(format!("/proc/{pid}/status")) else {
        return true;
    };
    for line in status.lines() {
        if let Some(rest) = line.strip_prefix("Uid:") {
            return rest
                .split_whitespace()
                .next()
                .and_then(|tok| tok.parse::<u32>().ok())
                .map(|uid| uid < 10000)
                .unwrap_or(true);
        }
    }
    true
}

pub fn list_processes() -> Vec<ProcInfo> {
    let mut out = Vec::new();
    let self_pid = std::process::id() as i32;
    let entries = match fs::read_dir("/proc") {
        Ok(v) => v,
        Err(_) => return out,
    };
    let packages = installed_packages();
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let name = file_name.to_string_lossy();
        let pid = match name.parse::<i32>() {
            Ok(v) if v > 0 && v != self_pid => v,
            _ => continue,
        };
        let cmdline = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default();
        let cmdline = cmdline.split(|b| *b == 0).next().unwrap_or_default();
        let raw = String::from_utf8_lossy(cmdline).trim().to_string();
        let display = if raw.is_empty() {
            fs::read_to_string(format!("/proc/{pid}/comm"))
                .unwrap_or_default()
                .trim()
                .to_string()
        } else {
            Path::new(&raw)
                .file_name()
                .map(|v| v.to_string_lossy().into_owned())
                .unwrap_or_else(|| raw.clone())
        };
        if display.is_empty() {
            continue;
        }
        let system = if packages.is_empty() {
            is_system_process(pid)
        } else {
            let pkg = raw.split(':').next().unwrap_or(raw.as_str());
            !packages.contains(pkg)
        };
        out.push(ProcInfo {
            pid,
            name: display,
            system,
        });
    }
    out.sort_by(|a, b| {
        a.name
            .to_ascii_lowercase()
            .cmp(&b.name.to_ascii_lowercase())
            .then(a.pid.cmp(&b.pid))
    });
    out
}

pub fn read_maps(pid: i32) -> Vec<MapRegion> {
    let mut out = Vec::new();
    let data = match fs::read_to_string(format!("/proc/{pid}/maps")) {
        Ok(v) => v,
        Err(_) => return out,
    };
    for line in data.lines() {
        let mut fields = line.split_whitespace();
        let range = match fields.next() {
            Some(v) => v,
            None => continue,
        };
        let perms = fields.next().unwrap_or("").to_string();
        let offset = fields
            .next()
            .and_then(|v| u64::from_str_radix(v, 16).ok())
            .unwrap_or(0);
        let _dev = fields.next();
        let _inode = fields.next();
        let path = fields.collect::<Vec<_>>().join(" ");
        let mut parts = range.split('-');
        let start = parts
            .next()
            .and_then(|v| u64::from_str_radix(v, 16).ok())
            .unwrap_or(0);
        let end = parts
            .next()
            .and_then(|v| u64::from_str_radix(v, 16).ok())
            .unwrap_or(0);
        if start == 0 || end <= start {
            continue;
        }
        out.push(MapRegion {
            start,
            end,
            offset,
            perms,
            path,
        });
    }
    out
}

pub fn main_module(maps: &[MapRegion]) -> u64 {
    maps.iter()
        .find(|m| m.perms.contains('x') && !m.path.is_empty() && m.offset == 0)
        .map(|m| m.start)
        .or_else(|| {
            maps.iter()
                .find(|m| m.perms.contains('x') && !m.path.is_empty())
                .map(|m| m.start)
        })
        .unwrap_or(0)
}

fn compare(ty: ValueType, a: u64, b: u64) -> Ordering {
    match ty {
        ValueType::Int32 => (a as u32 as i32).cmp(&(b as u32 as i32)),
        ValueType::UInt32 => (a as u32).cmp(&(b as u32)),
        ValueType::Int64 => (a as i64).cmp(&(b as i64)),
        ValueType::UInt64 => a.cmp(&b),
        ValueType::Float32 => f32::from_bits(a as u32)
            .partial_cmp(&f32::from_bits(b as u32))
            .unwrap_or(Ordering::Equal),
        ValueType::Double => f64::from_bits(a)
            .partial_cmp(&f64::from_bits(b))
            .unwrap_or(Ordering::Equal),
        ValueType::Bool => (a as u8).cmp(&(b as u8)),
        ValueType::BoolBit => (a as u8).cmp(&(b as u8)),
        ValueType::UInt8 => (a as u8).cmp(&(b as u8)),
        ValueType::Ptr => a.cmp(&b),
        ValueType::Int16 => (a as u16 as i16).cmp(&(b as u16 as i16)),
        ValueType::UInt16 => (a as u16).cmp(&(b as u16)),
    }
}

fn wrapping_sub(ty: ValueType, a: u64, b: u64) -> u64 {
    match ty {
        ValueType::Int32 => (a as u32 as i32).wrapping_sub(b as u32 as i32) as u32 as u64,
        ValueType::UInt32 => (a as u32).wrapping_sub(b as u32) as u64,
        ValueType::Int64 => (a as i64).wrapping_sub(b as i64) as u64,
        ValueType::UInt64 => a.wrapping_sub(b),
        ValueType::Float32 => (f32::from_bits(a as u32) - f32::from_bits(b as u32)).to_bits() as u64,
        ValueType::Double => (f64::from_bits(a) - f64::from_bits(b)).to_bits(),
        ValueType::Bool => a.wrapping_sub(b),
        ValueType::BoolBit => a.wrapping_sub(b),
        ValueType::UInt8 => (a as u8).wrapping_sub(b as u8) as u64,
        ValueType::Ptr => a.wrapping_sub(b),
        ValueType::Int16 => (a as u16 as i16).wrapping_sub(b as u16 as i16) as u16 as u64,
        ValueType::UInt16 => (a as u16).wrapping_sub(b as u16) as u64,
    }
}

fn value_eq(ty: ValueType, a: u64, b: u64) -> bool {
    compare(ty, a, b) == Ordering::Equal
}

fn change_match(ty: ValueType, mode: ChangeMode, delta: u64, current: u64, previous: u64) -> bool {
    match mode {
        ChangeMode::ExactValue => value_eq(ty, current, delta),
        ChangeMode::AnyChange => !value_eq(ty, current, previous),
        ChangeMode::Unchanged => value_eq(ty, current, previous),
        ChangeMode::Increased => compare(ty, current, previous) == Ordering::Greater,
        ChangeMode::Decreased => compare(ty, current, previous) == Ordering::Less,
        ChangeMode::IncreasedBy => {
            compare(ty, current, previous) == Ordering::Greater
                && wrapping_sub(ty, current, previous) == delta
        }
        ChangeMode::DecreasedBy => {
            compare(ty, current, previous) == Ordering::Less
                && wrapping_sub(ty, previous, current) == delta
        }
    }
}

pub fn scan_ranges(pid: i32, ty: ValueType, bits: u64, maps: &[MapRegion]) -> Vec<ResultEntry> {
    const CHUNK: usize = 1 << 20;
    // 上限取得足夠大 (百萬級), 結果由 UI 層 body.rows 懶加載, 不會一次性塞進介面
    const LIMIT: usize = 1_000_000;
    let mut out = Vec::new();
    let mut buf = vec![0u8; CHUNK];
    let size = ty.size();
    'regions: for map in maps {
        if !map.perms.contains('r') || !map.perms.contains('w') {
            continue;
        }
        if map.path.starts_with("/dev/") || map.path.starts_with("[vvar") {
            continue;
        }
        let mut addr = map.start;
        while addr < map.end {
            if out.len() >= LIMIT {
                break 'regions;
            }
            let len = ((map.end - addr) as usize).min(CHUNK);
            let chunk = &mut buf[..len];
            if nb9::read(pid, addr, chunk) {
                for (i, raw) in chunk.chunks_exact(size).enumerate() {
                    if out.len() >= LIMIT {
                        break;
                    }
                    let val = match size {
                        1 => raw[0] as u64,
                        2 => u16::from_le_bytes([raw[0], raw[1]]) as u64,
                        4 => u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as u64,
                        8 => u64::from_le_bytes([
                            raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
                        ]),
                        _ => continue,
                    };
                    if value_eq(ty, val, bits) {
                        out.push(ResultEntry {
                            addr: addr + (i * size) as u64,
                            bits: val,
                        });
                    }
                }
            }
            addr += len as u64;
        }
    }
    out
}

pub fn refine_entries(
    pid: i32,
    ty: ValueType,
    mode: ChangeMode,
    delta: u64,
    entries: &[ResultEntry],
) -> Vec<ResultEntry> {
    let mut out = Vec::new();
    let size = ty.size();
    for entry in entries {
        if let Some(current) = nb9::read_value(pid, entry.addr, size) {
            if change_match(ty, mode, delta, current, entry.bits) {
                out.push(ResultEntry {
                    addr: entry.addr,
                    bits: current,
                });
            }
        }
    }
    out
}

pub fn refresh_entries(pid: i32, ty: ValueType, entries: &[ResultEntry]) -> Vec<ResultEntry> {
    let mut out = Vec::with_capacity(entries.len());
    let size = ty.size();
    for entry in entries {
        if let Some(bits) = nb9::read_value(pid, entry.addr, size) {
            out.push(ResultEntry {
                addr: entry.addr,
                bits,
            });
        }
    }
    out
}

pub fn hex_dump(pid: i32, addr: u64, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    if !nb9::read(pid, addr, &mut out) {
        out.clear();
    }
    out
}
