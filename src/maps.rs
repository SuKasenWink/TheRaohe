use std::fs;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};

#[derive(Clone)]
pub struct MapRegion {
    pub start: u64,
    pub end: u64,
    pub offset: u64,
    pub perms: String,
    pub path: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MapKind {
    DefaultSelected,
    DefaultExcluded,
    AbsoluteExcluded,
}

fn parse_map_line(line: &str) -> Option<MapRegion> {
    let mut fields = line.split_whitespace();
    let range = fields.next()?;
    let perms = fields.next()?.to_string();
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
        return None;
    }
    Some(MapRegion {
        start,
        end,
        offset,
        perms,
        path,
    })
}

pub(crate) fn page_size() -> u64 {
    let value = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if value > 0 {
        value as u64
    } else {
        4096
    }
}

fn intervals_from_file(
    file: &mut File,
    map: &MapRegion,
    page: u64,
) -> std::io::Result<Vec<(u64, u64)>> {
    let start_page = map.start / page;
    let end_page = (map.end + page - 1) / page;
    file.seek(SeekFrom::Start(start_page * 8))?;
    let mut intervals = Vec::new();
    let mut current = None;
    for index in start_page..end_page {
        let mut raw = [0u8; 8];
        file.read_exact(&mut raw)?;
        let present = u64::from_le_bytes(raw) & (1u64 << 63) != 0;
        let page_addr = index * page;
        if present {
            if current.is_none() {
                current = Some(page_addr);
            }
        } else if let Some(start) = current.take() {
            intervals.push((start, page_addr.min(map.end)));
        }
    }
    if let Some(start) = current {
        intervals.push((start, map.end));
    }
    Ok(intervals)
}

pub(crate) struct Pagemap {
    file: File,
    page: u64,
}

impl Pagemap {
    pub(crate) fn open(pid: i32) -> Option<Self> {
        Some(Self {
            file: File::open(format!("/proc/{pid}/pagemap")).ok()?,
            page: page_size(),
        })
    }

    pub(crate) fn page(&self) -> u64 {
        self.page
    }

    pub(crate) fn intervals(&mut self, map: &MapRegion) -> Option<Vec<(u64, u64)>> {
        intervals_from_file(&mut self.file, map, self.page).ok()
    }
}

pub fn classify_map(map: &MapRegion) -> MapKind {
    let perms = map.perms.to_ascii_lowercase();
    let path = map.path.to_ascii_lowercase();
    let absolute = !perms.contains('r')
        || !perms.contains('w')
        || path.starts_with("/dev/")
        || path.contains("[vvar")
        || path.starts_with("[vdso")
        || path.starts_with("[vsyscall")
        || path.starts_with("[vectors]");
    if absolute {
        return MapKind::AbsoluteExcluded;
    }
    const GRAPHICS_TOKENS: [&str; 10] = [
        "kgsl",
        "gralloc",
        "vulkan",
        "dma_heap",
        "dma-buf",
        "adreno",
        "nvgpu",
        "powervr",
        "mali-",
        "pvr",
    ];
    let shared = perms.chars().nth(3) == Some('s');
    let graphics = GRAPHICS_TOKENS
        .iter()
        .any(|needle| path.contains(needle));
    if shared || graphics {
        MapKind::DefaultExcluded
    } else {
        MapKind::DefaultSelected
    }
}

pub fn is_absolute_excluded(map: &MapRegion) -> bool {
    classify_map(map) == MapKind::AbsoluteExcluded
}

pub fn default_map_checked(map: &MapRegion) -> bool {
    classify_map(map) == MapKind::DefaultSelected
}

pub fn display_order(maps: &[MapRegion]) -> Vec<usize> {
    let mut selected = Vec::with_capacity(maps.len());
    let mut excluded = Vec::new();
    let mut absolute = Vec::new();
    for (index, map) in maps.iter().enumerate() {
        match classify_map(map) {
            MapKind::DefaultSelected => selected.push(index),
            MapKind::DefaultExcluded => excluded.push(index),
            MapKind::AbsoluteExcluded => absolute.push(index),
        }
    }
    selected.extend(excluded);
    selected.extend(absolute);
    selected
}

pub fn read_maps(pid: i32) -> Vec<MapRegion> {
    let mut out = Vec::new();
    let data = match fs::read_to_string(format!("/proc/{pid}/maps")) {
        Ok(v) => v,
        Err(_) => return out,
    };
    for line in data.lines() {
        if let Some(map) = parse_map_line(line) {
            out.push(map);
        }
    }
    out
}

/// 主模組基底: 取「有可執行段的路徑」中總對應尺寸最大者的 offset==0 段位 (PUBGM 的 libUE4.so
/// 被大量匿名/隱藏 VMA 夾雜其前, 只看 argv/maps 順序第一個執行段會撞上 PixUI 這類小外掛)
pub fn main_module(maps: &[MapRegion]) -> u64 {
    use std::collections::HashMap;
    let mut size_by_path: HashMap<&str, u64> = HashMap::new();
    let mut has_rx_by_path: HashMap<&str, bool> = HashMap::new();
    let mut base_by_path: HashMap<&str, u64> = HashMap::new();
    for m in maps {
        if m.path.is_empty() {
            continue;
        }
        *size_by_path.entry(m.path.as_str()).or_default() += m.end - m.start;
        if m.perms.contains('x') {
            has_rx_by_path.insert(m.path.as_str(), true);
        }
        if m.offset == 0 {
            base_by_path.entry(m.path.as_str()).or_insert(m.start);
        }
    }
    size_by_path
        .into_iter()
        // 僅檔案映射的 ELF 類路徑; 虛擬機/堆匿名 VMA (項目最多 rwxp 一兩個 JIT 頁) 會把總和堆出假大數
        .filter(|(p, _)| p.starts_with('/') && p.ends_with(".so"))
        .filter(|(p, _)| has_rx_by_path.get(p).copied().unwrap_or(false) && base_by_path.contains_key(p))
        .max_by_key(|&(_, size)| size)
        .map(|(p, _)| base_by_path[p])
        .unwrap_or(0)
}
