use crate::maps::{is_absolute_excluded, MapRegion, Pagemap};
use crate::nb9;
use std::cmp::Ordering;
use std::fmt;
use std::fs;
use std::path::Path;
use xxhash_rust::xxh3;

#[derive(Clone)]
pub struct ProcInfo {
    pub pid: i32,
    pub name: String,
    pub system: bool,
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
    Range,
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
    Between,
}

#[derive(Clone, Copy)]
pub struct ResultEntry {
    pub addr: u64,
    pub bits: u64,
    /// BoolBit 結果專用: 該位在位元組內的位置; 其餘型別恆 0
    pub bit: u8,
}

/// 以 (addr, bit) 聯合作為 selected/live_bits 的鍵; 要求 addr < 2^61
/// (aarch64 Android 的使用者位址遠低於此, 超過即放棄超高位, 不影響等值判斷)
pub(crate) fn entry_key(addr: u64, bit: u8) -> u64 {
    (addr << 3) | bit as u64
}

/// 首次掃描只可表達「絕對值」過濾; 變化/時間條件必須先有快照再篩選
#[derive(Clone, Copy)]
pub enum ScanFilter {
    Exact(u64),
    Between { min: u64, max: u64 },
}

impl ScanFilter {
    fn matches(self, ty: ValueType, val: u64) -> bool {
        match self {
            ScanFilter::Exact(bits) => value_eq(ty, val, bits),
            ScanFilter::Between { min, max } => {
                compare(ty, val, min) != Ordering::Less && compare(ty, val, max) != Ordering::Greater
            }
        }
    }
}

/// 篩選條件; 帶上下文的條件(Between 等)在此封裝, 避免半組狀態
#[derive(Clone, Copy)]
pub enum RefineFilter {
    Exact(u64),
    AnyChange,
    Unchanged,
    Increased,
    Decreased,
    IncreasedBy(u64),
    DecreasedBy(u64),
    Between { min: u64, max: u64 },
}

#[derive(Clone, Copy)]
pub struct FuzzyBlock {
    pub hash: u64,
    pub readable: bool,
    pub changed: bool,
}

#[derive(Clone)]
pub struct FuzzyBase {
    pub block_bytes: u64,
    pub candidate_bytes: u64,
    pub candidate_blocks: usize,
    pub spans: Vec<MapRegion>,
    pub blocks: Vec<FuzzyBlock>,
    /// bit i 對應「第 i 個區塊」, 1 = 仍是候選; 雜湊表永遠依快照時的區塊序排列
    pub kept_mask: Vec<u64>,
}

pub struct FuzzyOutcome {
    pub base: Option<FuzzyBase>,
    pub scanned_bytes: u64,
}

pub struct ScanOutcome {
    pub results: Vec<ResultEntry>,
    pub scanned_bytes: u64,
}

impl ValueType {
    pub fn size(self) -> usize {
        match self {
            ValueType::Int32 | ValueType::UInt32 | ValueType::Float32 => 4,
            ValueType::Int64 | ValueType::UInt64 | ValueType::Double | ValueType::Ptr => 8,
            ValueType::Bool | ValueType::BoolBit | ValueType::UInt8 => 1,
            ValueType::Int16 | ValueType::UInt16 => 2,
            ValueType::Range => 0,
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
            ValueType::Range => "Range",
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
            ValueType::Bool => parse_bool(text),
            ValueType::BoolBit => parse_bool(text),
            ValueType::UInt8 => text.parse::<u8>().ok().map(|v| v as u64),
            ValueType::Ptr => parse_u64(text),
            ValueType::Int16 => text.parse::<i16>().ok().map(|v| v as u16 as u64),
            ValueType::UInt16 => text.parse::<u16>().ok().map(|v| v as u64),
            ValueType::Range => None,
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
            ValueType::Bool | ValueType::BoolBit => format!("{}", bits as u8 != 0),
            ValueType::UInt8 => format!("{}", bits as u8),
            ValueType::Ptr => format!("0x{bits:x}"),
            ValueType::Int16 => format!("{}", bits as u16 as i16),
            ValueType::UInt16 => format!("{}", bits as u16),
            ValueType::Range => "256B".to_string(),
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
            ChangeMode::Between => "範圍：",
        }
    }
}

fn parse_bool(text: &str) -> Option<u64> {
    // Bool 顯示 true/false, 輸入契約須能吃回自己顯示的字串
    if text.eq_ignore_ascii_case("true") {
        return Some(1);
    }
    if text.eq_ignore_ascii_case("false") {
        return Some(0);
    }
    text.parse::<u8>().ok().map(|v| v as u64)
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

pub(crate) fn compare(ty: ValueType, a: u64, b: u64) -> Ordering {
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
        ValueType::Bool | ValueType::BoolBit | ValueType::UInt8 => (a as u8).cmp(&(b as u8)),
        ValueType::Ptr => a.cmp(&b),
        ValueType::Int16 => (a as u16 as i16).cmp(&(b as u16 as i16)),
        ValueType::UInt16 => (a as u16).cmp(&(b as u16)),
        ValueType::Range => Ordering::Equal,
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
        ValueType::Bool | ValueType::BoolBit => a.wrapping_sub(b),
        ValueType::UInt8 => (a as u8).wrapping_sub(b as u8) as u64,
        ValueType::Ptr => a.wrapping_sub(b),
        ValueType::Int16 => (a as u16 as i16).wrapping_sub(b as u16 as i16) as u16 as u64,
        ValueType::UInt16 => (a as u16).wrapping_sub(b as u16) as u64,
        ValueType::Range => 0,
    }
}

fn value_eq(ty: ValueType, a: u64, b: u64) -> bool {
    compare(ty, a, b) == Ordering::Equal
}

fn refine_match(
    ty: ValueType,
    filter: RefineFilter,
    current: u64,
    entry: &ResultEntry,
) -> bool {
    match filter {
        RefineFilter::Exact(value) => value_eq(ty, current, value),
        RefineFilter::AnyChange => !value_eq(ty, current, entry.bits),
        RefineFilter::Unchanged => value_eq(ty, current, entry.bits),
        RefineFilter::Increased => compare(ty, current, entry.bits) == Ordering::Greater,
        RefineFilter::Decreased => compare(ty, current, entry.bits) == Ordering::Less,
        RefineFilter::IncreasedBy(delta) => {
            compare(ty, current, entry.bits) == Ordering::Greater
                && wrapping_sub(ty, current, entry.bits) == delta
        }
        RefineFilter::DecreasedBy(delta) => {
            compare(ty, current, entry.bits) == Ordering::Less
                && wrapping_sub(ty, entry.bits, current) == delta
        }
        RefineFilter::Between { min, max } => {
            compare(ty, current, min) != Ordering::Less && compare(ty, current, max) != Ordering::Greater
        }
    }
}

const SCAN_CHUNK: u64 = 1 << 20;
const SCAN_SPLIT: u64 = 64 * 1024;
pub(crate) const SCAN_LIMIT: usize = 1_000_000;

fn load_le(raw: &[u8]) -> Option<u64> {
    Some(match raw.len() {
        1 => raw[0] as u64,
        2 => u16::from_le_bytes([raw[0], raw[1]]) as u64,
        4 => u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]) as u64,
        8 => u64::from_le_bytes([
            raw[0], raw[1], raw[2], raw[3], raw[4], raw[5], raw[6], raw[7],
        ]),
        _ => return None,
    })
}

fn scan_buffer(
    ty: ValueType,
    filter: ScanFilter,
    addr: u64,
    chunk: &[u8],
    out: &mut Vec<ResultEntry>,
) -> bool {
    let size = ty.size();
    if ty == ValueType::BoolBit {
        // Bit 一次掃 0-7 位: 每個位元各自一行結果, bits 存該位 0/1
        for (i, byte) in chunk.iter().enumerate() {
            if out.len() >= SCAN_LIMIT {
                return true;
            }
            for k in 0..8u8 {
                if out.len() >= SCAN_LIMIT {
                    return true;
                }
                let val = ((*byte >> k) & 1) as u64;
                if filter.matches(ty, val) {
                    out.push(ResultEntry {
                        addr: addr + i as u64,
                        bits: val,
                        bit: k,
                    });
                }
            }
        }
        return out.len() >= SCAN_LIMIT;
    }
    for (i, raw) in chunk.chunks_exact(size).enumerate() {
        if out.len() >= SCAN_LIMIT {
            return true;
        }
        let Some(val) = load_le(raw) else { continue };
        if filter.matches(ty, val) {
            out.push(ResultEntry {
                addr: addr + (i * size) as u64,
                bits: val,
                bit: 0,
            });
        }
    }
    out.len() >= SCAN_LIMIT
}

fn scan_block(
    pid: i32,
    ty: ValueType,
    filter: ScanFilter,
    out: &mut Vec<ResultEntry>,
    scanned_bytes: &mut u64,
    buf: &mut [u8],
    page: u64,
    addr: u64,
    len: u64,
) -> bool {
    if len == 0 || out.len() >= SCAN_LIMIT {
        return out.len() >= SCAN_LIMIT;
    }
    let chunk_len = len.min(SCAN_CHUNK);
    let read_len = chunk_len as usize;
    if nb9::read(pid, addr, &mut buf[..read_len]) {
        *scanned_bytes += read_len as u64;
        if scan_buffer(ty, filter, addr, &buf[..read_len], out) {
            return true;
        }
        if len > SCAN_CHUNK {
            return scan_block(
                pid,
                ty,
                filter,
                out,
                scanned_bytes,
                buf,
                page,
                addr + SCAN_CHUNK,
                len - SCAN_CHUNK,
            );
        }
        return false;
    }
    if chunk_len > SCAN_SPLIT {
        let mut offset = 0;
        while offset < chunk_len {
            let sub = (chunk_len - offset).min(SCAN_SPLIT);
            if scan_block(pid, ty, filter, out, scanned_bytes, buf, page, addr + offset, sub) {
                return true;
            }
            offset += sub;
        }
        if len > SCAN_CHUNK {
            return scan_block(
                pid,
                ty,
                filter,
                out,
                scanned_bytes,
                buf,
                page,
                addr + SCAN_CHUNK,
                len - SCAN_CHUNK,
            );
        }
        return false;
    }
    if chunk_len > page {
        let mut offset = 0;
        while offset < chunk_len {
            let sub = (chunk_len - offset).min(page);
            if scan_block(pid, ty, filter, out, scanned_bytes, buf, page, addr + offset, sub) {
                return true;
            }
            offset += sub;
        }
        if len > SCAN_CHUNK {
            return scan_block(
                pid,
                ty,
                filter,
                out,
                scanned_bytes,
                buf,
                page,
                addr + SCAN_CHUNK,
                len - SCAN_CHUNK,
            );
        }
        return false;
    }
    false
}

fn scan_span(
    pid: i32,
    ty: ValueType,
    filter: ScanFilter,
    out: &mut Vec<ResultEntry>,
    scanned_bytes: &mut u64,
    buf: &mut [u8],
    page: u64,
    start: u64,
    end: u64,
) -> bool {
    if end <= start {
        return false;
    }
    scan_block(pid, ty, filter, out, scanned_bytes, buf, page, start, end - start)
}

pub fn scan_ranges(pid: i32, ty: ValueType, filter: ScanFilter, maps: &[MapRegion]) -> ScanOutcome {
    let mut pagemap = Pagemap::open(pid);
    let page = pagemap
        .as_ref()
        .map(Pagemap::page)
        .unwrap_or_else(crate::maps::page_size);
    let mut out = Vec::new();
    let mut scanned_bytes = 0;
    let mut buf = vec![0u8; SCAN_CHUNK as usize];
    'regions: for map in maps {
        if is_absolute_excluded(map) {
            continue;
        }
        let hit_limit = match pagemap.as_mut().and_then(|pagemap| pagemap.intervals(map)) {
            Some(intervals) => {
                let mut hit = false;
                for (start, end) in intervals {
                    if scan_span(pid, ty, filter, &mut out, &mut scanned_bytes, &mut buf, page, start, end) {
                        hit = true;
                        break;
                    }
                }
                hit
            }
            None => scan_span(pid, ty, filter, &mut out, &mut scanned_bytes, &mut buf, page, map.start, map.end),
        };
        if hit_limit {
            break 'regions;
        }
    }
    ScanOutcome {
        results: out,
        scanned_bytes,
    }
}

pub fn refine_entries(
    pid: i32,
    ty: ValueType,
    filter: RefineFilter,
    entries: &[ResultEntry],
) -> Vec<ResultEntry> {
    let mut out = Vec::new();
    let size = ty.size();
    for entry in entries {
        let current = match ty {
            // Bit: 依位址重讀位元組, 再取 entry 自身的位元
            ValueType::BoolBit => nb9::read_value(pid, entry.addr, 1)
                .map(|v| (v >> entry.bit) & 1),
            _ => nb9::read_value(pid, entry.addr, size),
        };
        if let Some(current) = current {
            if refine_match(ty, filter, current, entry) {
                out.push(ResultEntry {
                    addr: entry.addr,
                    bits: current,
                    bit: entry.bit,
                });
            }
        }
    }
    out
}

pub fn read_struct_components(pid: i32, ty: ValueType, base: u64) -> [Option<u64>; 3] {
    let size = ty.size();
    [0, 1, 2].map(|i| {
        base.checked_add((i * size) as u64)
            .and_then(|addr| nb9::read_value(pid, addr, size))
    })
}

fn hash_bytes(bytes: &[u8]) -> u64 {
    xxh3::xxh3_64(bytes)
}

pub(crate) fn mask_get(mask: &[u64], idx: usize) -> bool {
    (mask[idx / 64] >> (idx % 64)) & 1 == 1
}

fn mask_set(mask: &mut [u64], idx: usize, value: bool) {
    let bit = 1u64 << (idx % 64);
    if value {
        mask[idx / 64] |= bit;
    } else {
        mask[idx / 64] &= !bit;
    }
}

pub fn available_memory_bytes() -> Option<u64> {
    let text = fs::read_to_string("/proc/meminfo").ok()?;
    let pick = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name).map(str::trim))
            .and_then(|rest| rest.split_whitespace().next()?.parse::<u64>().ok())
            .map(|kb| kb.saturating_mul(1024))
    };
    pick("MemAvailable:").or_else(|| pick("MemFree:"))
}

pub fn fuzzy_snapshot(pid: i32, maps: &[MapRegion], block_bytes: u64) -> FuzzyBase {
    let block_count: usize = maps
        .iter()
        .filter(|map| !is_absolute_excluded(map))
        .map(|map| map.end.saturating_sub(map.start).saturating_add(block_bytes - 1) / block_bytes)
        .map(|blocks| blocks as usize)
        .sum();
    let mut blocks = Vec::with_capacity(block_count);
    let mut kept_mask = vec![0u64; block_count.div_ceil(64)];
    let mut spans = Vec::with_capacity(maps.len());
    let mut candidate_bytes = 0;
    let mut candidate_blocks = 0;
    let mut index = 0usize;
    for map in maps {
        if is_absolute_excluded(map) {
            continue;
        }
        spans.push(map.clone());
        let mut start = map.start;
        while start < map.end {
            let end = (start + block_bytes).min(map.end);
            let len = (end - start) as usize;
            let mut buf = vec![0u8; len];
            let readable = nb9::read(pid, start, &mut buf);
            if readable {
                candidate_bytes += end - start;
                candidate_blocks += 1;
                mask_set(&mut kept_mask, index, true);
            }
            blocks.push(FuzzyBlock {
                hash: if readable { hash_bytes(&buf) } else { 0 },
                readable,
                changed: false,
            });
            index += 1;
            start = end;
        }
    }
    FuzzyBase {
        block_bytes,
        candidate_bytes,
        candidate_blocks,
        spans,
        blocks,
        kept_mask,
    }
}

pub fn fuzzy_refine(pid: i32, base: FuzzyBase, mode: ChangeMode) -> FuzzyOutcome {
    let FuzzyBase {
        block_bytes,
        spans,
        blocks,
        mut kept_mask,
        ..
    } = base;
    let mut blocks = blocks;
    let mut candidate_bytes = 0;
    let mut candidate_blocks = 0;
    let mut scanned_bytes = 0;
    let mut index = 0usize;
    for map in &spans {
        let mut start = map.start;
        while start < map.end {
            let end = (start + block_bytes).min(map.end);
            let len = (end - start) as usize;
            let i = index;
            index += 1;
            if !mask_get(&kept_mask, i) {
                start = end;
                continue;
            }
            let mut buf = vec![0u8; len];
            if !blocks[i].readable || !nb9::read(pid, start, &mut buf) {
                mask_set(&mut kept_mask, i, false);
                start = end;
                continue;
            }
            scanned_bytes += len as u64;
            let hash = hash_bytes(&buf);
            let changed = hash != blocks[i].hash;
            blocks[i].hash = hash;
            // 已變化過的標記單調保留到下次建立區塊快照
            if changed {
                blocks[i].changed = true;
            }
            let keep = match mode {
                ChangeMode::AnyChange => changed,
                ChangeMode::Unchanged => !changed,
                _ => changed,
            };
            if !keep {
                mask_set(&mut kept_mask, i, false);
                start = end;
                continue;
            }
            candidate_bytes += end - start;
            candidate_blocks += 1;
            start = end;
        }
    }
    FuzzyOutcome {
        base: if candidate_blocks == 0 {
            None
        } else {
            Some(FuzzyBase {
                block_bytes,
                candidate_bytes,
                candidate_blocks,
                spans,
                blocks,
                kept_mask,
            })
        },
        scanned_bytes,
    }
}

/// 以常規結果對齊出分塊視圖: 每個結果位址 floor 到 block 起點, 去重後重讀算雜湊。
/// 視圖只是呈現層投影, 不記錄候選值語意 (readable/hash 僅供顯示與對視圖做區塊篩選)。
pub fn blocks_from_entries(pid: i32, maps: &[MapRegion], entries: &[ResultEntry], block_bytes: u64) -> FuzzyBase {
    use std::collections::HashSet;
    let kept: HashSet<u64> = entries
        .iter()
        .map(|e| (e.addr / block_bytes) * block_bytes)
        .collect();
    let block_count: usize = maps
        .iter()
        .filter(|map| !is_absolute_excluded(map))
        .map(|map| map.end.saturating_sub(map.start).saturating_add(block_bytes - 1) / block_bytes)
        .map(|blocks| blocks as usize)
        .sum();
    let mut blocks = Vec::with_capacity(block_count);
    let mut kept_mask = vec![0u64; block_count.div_ceil(64)];
    let mut spans = Vec::with_capacity(maps.len());
    let mut candidate_bytes = 0;
    let mut candidate_blocks = 0;
    let mut index = 0usize;
    for map in maps {
        if is_absolute_excluded(map) {
            continue;
        }
        spans.push(map.clone());
        let mut start = map.start;
        while start < map.end {
            let end = (start + block_bytes).min(map.end);
            let len = (end - start) as usize;
            if kept.contains(&start) {
                let mut buf = vec![0u8; len];
                let readable = nb9::read(pid, start, &mut buf);
                if readable {
                    candidate_bytes += end - start;
                    candidate_blocks += 1;
                    mask_set(&mut kept_mask, index, true);
                }
                blocks.push(FuzzyBlock {
                    hash: if readable { hash_bytes(&buf) } else { 0 },
                    readable,
                    changed: false,
                });
            } else {
                blocks.push(FuzzyBlock {
                    hash: 0,
                    readable: false,
                    changed: false,
                });
            }
            index += 1;
            start = end;
        }
    }
    FuzzyBase {
        block_bytes,
        candidate_bytes,
        candidate_blocks,
        spans,
        blocks,
        kept_mask,
    }
}

pub fn fuzzy_to_entries(pid: i32, base: &FuzzyBase, ty: ValueType) -> (Vec<ResultEntry>, u64) {
    let size = ty.size();
    if size == 0 {
        return (Vec::new(), 0);
    }
    let mut out = Vec::new();
    let mut scanned = 0;
    let mut index = 0usize;
    for map in &base.spans {
        let mut start = map.start;
        while start < map.end {
            let end = (start + base.block_bytes).min(map.end);
            let i = index;
            index += 1;
            if mask_get(&base.kept_mask, i) {
                let mut buf = vec![0u8; (end - start) as usize];
                if nb9::read(pid, start, &mut buf) {
                    let mut addr = start;
                    for raw in buf.chunks_exact(size) {
                        if let Some(bits) = load_le(raw) {
                            if ty == ValueType::BoolBit {
                                for k in 0..8u8 {
                                    out.push(ResultEntry {
                                        addr,
                                        bits: (bits >> k) & 1,
                                        bit: k,
                                    });
                                }
                            } else {
                                out.push(ResultEntry {
                                    addr,
                                    bits,
                                    bit: 0,
                                });
                            }
                            scanned += size as u64;
                        }
                        addr += size as u64;
                    }
                }
            }
            start = end;
        }
    }
    (out, scanned)
}

pub fn hex_dump(pid: i32, addr: u64, len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    if !nb9::read(pid, addr, &mut out) {
        out.clear();
    }
    out
}

// CE 指標鏈語法 (x64dbg/CE 風格的指標鏈字串)
// term := '[' expr ']' | '"模組名"' | number ;  number 一律十六進位, 可選 0x 前綴
// expr := term (('+' | '-') number)* ; 模組名必須加引號, 否則與十六進位數字歧義
// 攤平後語意: addr = base + first_offset; 對每個 offset 做 讀指標 → 加偏移; offsets 長度 = 解參考次數
#[derive(Debug, Clone, PartialEq)]
pub enum Base {
    Module(String),
    Absolute(u64),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Chain {
    pub base: Base,
    pub first_offset: i64,
    pub offsets: Vec<i64>,
}

#[derive(Debug)]
pub struct ParseError {
    pub pos: usize,
    pub msg: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "位置 {}: {}", self.pos, self.msg)
    }
}

impl std::error::Error for ParseError {}

enum Op {
    Add(i64),
    Deref,
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn err<T>(&self, msg: &str) -> Result<T, ParseError> {
        Err(ParseError {
            pos: self.pos,
            msg: msg.to_string(),
        })
    }

    fn skip_ws(&mut self) {
        while self.pos < self.s.len() && self.s[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }

    fn peek(&mut self) -> Option<u8> {
        self.skip_ws();
        self.s.get(self.pos).copied()
    }

    fn expect(&mut self, c: u8) -> Result<(), ParseError> {
        if self.peek() == Some(c) {
            self.pos += 1;
            Ok(())
        } else {
            self.err(&format!("預期 '{}'", c as char))
        }
    }

    fn number(&mut self) -> Result<u64, ParseError> {
        self.skip_ws();
        if self.s[self.pos..].starts_with(b"0x") || self.s[self.pos..].starts_with(b"0X") {
            self.pos += 2;
        }
        let start = self.pos;
        while self.pos < self.s.len() && self.s[self.pos].is_ascii_hexdigit() {
            self.pos += 1;
        }
        if start == self.pos {
            return self.err("預期十六進位數字");
        }
        let text = std::str::from_utf8(&self.s[start..self.pos]).unwrap();
        u64::from_str_radix(text, 16).or_else(|_| self.err("數字過大"))
    }

    fn term(&mut self, ops: &mut Vec<Op>) -> Result<Base, ParseError> {
        match self.peek() {
            Some(b'[') => {
                self.pos += 1;
                let base = self.expr(ops)?;
                self.expect(b']')?;
                ops.push(Op::Deref);
                Ok(base)
            }
            Some(b'"') => {
                self.pos += 1;
                let start = self.pos;
                while self.pos < self.s.len() && self.s[self.pos] != b'"' {
                    self.pos += 1;
                }
                if self.pos >= self.s.len() {
                    return self.err("引號未閉合");
                }
                let name = String::from_utf8_lossy(&self.s[start..self.pos]).into_owned();
                self.pos += 1;
                Ok(Base::Module(name))
            }
            Some(_) => Ok(Base::Absolute(self.number()?)),
            None => self.err("表達式意外結束"),
        }
    }

    fn expr(&mut self, ops: &mut Vec<Op>) -> Result<Base, ParseError> {
        let base = self.term(ops)?;
        loop {
            match self.peek() {
                Some(b'+') => {
                    self.pos += 1;
                    ops.push(Op::Add(self.number()? as i64));
                }
                Some(b'-') => {
                    self.pos += 1;
                    ops.push(Op::Add((self.number()? as i64).wrapping_neg()));
                }
                _ => break,
            }
        }
        Ok(base)
    }
}

pub fn parse_chain(input: &str) -> Result<Chain, ParseError> {
    let mut p = Parser {
        s: input.as_bytes(),
        pos: 0,
    };
    let mut ops = Vec::new();
    let base = p.expr(&mut ops)?;
    if p.peek().is_some() {
        return p.err("表達式後有多餘字元");
    }

    // 攤平: 第一個 Deref 之前的加法合成 first_offset;
    // 之後每個 Deref 後面到下一個 Deref 之前的加法合成一個 offset
    let mut first_offset = 0i64;
    let mut offsets: Vec<i64> = Vec::new();
    let mut seen_deref = false;
    for op in ops {
        match op {
            Op::Add(n) if !seen_deref => first_offset = first_offset.wrapping_add(n),
            Op::Add(n) => {
                let last = offsets.last_mut().unwrap();
                *last = last.wrapping_add(n);
            }
            Op::Deref => {
                seen_deref = true;
                offsets.push(0);
            }
        }
    }
    Ok(Chain {
        base,
        first_offset,
        offsets,
    })
}

/// 成功回傳最終指標值; 失敗回傳 Err(斷在第幾層): Err(0) = 模組找不到, Err(n) = 第 n 次解參考失敗 (含空指標)
pub fn resolve(
    chain: &Chain,
    module_base: impl Fn(&str) -> Option<u64>,
    mut read_ptr: impl FnMut(u64) -> Option<u64>,
) -> Result<u64, usize> {
    let base = match &chain.base {
        Base::Absolute(a) => *a,
        Base::Module(m) => module_base(m).ok_or(0usize)?,
    };
    let mut addr = base.wrapping_add_signed(chain.first_offset);

    for (depth, off) in chain.offsets.iter().enumerate() {
        let p = read_ptr(addr).ok_or(depth + 1)?;
        if p == 0 {
            return Err(depth + 1);
        }
        addr = p.wrapping_add_signed(*off);
    }
    Ok(addr)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    #[test]
    fn parses_module_chain() {
        let c = parse_chain(r#"[[["game.exe"+1A8]+30]+10]+5C"#).unwrap();
        assert_eq!(c.base, Base::Module("game.exe".into()));
        assert_eq!(c.first_offset, 0x1A8);
        assert_eq!(c.offsets, vec![0x30, 0x10, 0x5C]);
    }

    #[test]
    fn plain_address() {
        let c = parse_chain("0x1000+8").unwrap();
        assert_eq!(c.offsets, Vec::<i64>::new());
        assert_eq!(c.first_offset, 8);
    }

    #[test]
    fn negative_offset() {
        let c = parse_chain("[1000-8]+4").unwrap();
        assert_eq!(c.first_offset, -8);
        assert_eq!(c.offsets, vec![4]);
    }

    #[test]
    fn trailing_garbage_rejected() {
        assert!(parse_chain("0x1000 xyz").is_err());
        assert!(parse_chain("[0x1000").is_err());
        assert!(parse_chain(r#""abc" "def""#).is_err());
    }

    #[test]
    fn resolve_walks_pointer_chain() {
        let chain = parse_chain(r#"[["game.exe"+1A8]+30]+10"#).unwrap();
        let ptr = HashMap::from([(0x1000u64 + 0x1A8, 0x2000u64), (0x2000 + 0x30, 0x3000)]);
        let addr = resolve(&chain, |_| Some(0x1000), |a| ptr.get(&a).copied()).unwrap();
        assert_eq!(addr, 0x3000 + 0x10);
    }

    #[test]
    fn resolve_reports_failed_depth() {
        let chain = parse_chain(r#"[["game.exe"+1A8]+30]+10"#).unwrap();
        assert_eq!(resolve(&chain, |_| Some(0x1000), |_| None), Err(1));
        assert_eq!(resolve(&chain, |_| None, |_| None), Err(0));
        let ptr = HashMap::from([(0x1000u64 + 0x1A8, 0u64)]);
        assert_eq!(resolve(&chain, |_| Some(0x1000), |a| ptr.get(&a).copied()), Err(1));
    }
}
