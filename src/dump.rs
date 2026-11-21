//! 匯出 RW 映射區到 `/data/local/tmp/theraohe/dump-<unixts>/` —
//! 決賽圈瞬間鑑定 actor 陣列新位置用; 每個 RW-private 非圖形段一個檔,
//! 檔案內偏移 == 虛擬位址偏移 (失敗頁以 0 填充), 加上 maps.txt 與 manifest.txt

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::maps::{classify_map, MapKind, MapRegion};
use crate::nb9;

const CHUNK: usize = 4 * 1024 * 1024;
const FALLBACK_CHUNK: usize = 64 * 1024;

pub struct DumpReport {
    pub dir: PathBuf,
    pub regions: usize,
    pub bytes: u64,
    pub zero_bytes: u64,
}

fn dump_region(pid: i32, map: &MapRegion, file: &Path) -> std::io::Result<(u64, u64, usize)> {
    let mut out = fs::File::create(file)?;
    let buf_sz = CHUNK.max(FALLBACK_CHUNK);
    let mut buf = vec![0u8; buf_sz];
    let mut written: u64 = 0;
    let mut zeroed: u64 = 0;
    let mut failed_chunks: usize = 0;
    let mut addr = map.start;
    while addr < map.end {
        let want = std::cmp::min(CHUNK as u64, map.end - addr) as usize;
        if nb9::read(pid, addr, &mut buf[..want]) {
            out.write_all(&buf[..want])?;
            written += want as u64;
            addr += want as u64;
            continue;
        }
        // 整塊讀失敗 → 退回 64KB 逐頁兜底, 壞頁補 0 保住偏移
        let mut off = 0usize;
        while off < want {
            let n = std::cmp::min(FALLBACK_CHUNK as u64, map.end - addr - off as u64) as usize;
            if nb9::read(pid, addr + off as u64, &mut buf[..n]) {
                out.write_all(&buf[..n])?;
                written += n as u64;
            } else {
                out.write_all(&vec![0u8; n])?;
                zeroed += n as u64;
                failed_chunks += 1;
            }
            off += n;
        }
        addr += want as u64;
    }
    Ok((written, zeroed, failed_chunks))
}

pub fn dump_rw_memory(
    pid: i32,
    maps: &[MapRegion],
    base_dir: &str,
) -> std::io::Result<DumpReport> {
    let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let dir = PathBuf::from(base_dir).join(format!("dump-{}", ts));
    fs::create_dir_all(&dir)?;

    let mut maps_txt = String::new();
    for m in maps {
        maps_txt.push_str(&format!("{:x}-{:x}  {}  {}\n", m.start, m.end, m.perms, m.path));
    }
    fs::write(dir.join("maps.txt"), maps_txt)?;

    let mut manifest = String::new();
    let mut regions = 0usize;
    let mut bytes: u64 = 0;
    let mut zero_bytes: u64 = 0;
    let mut failed_regions: usize = 0;
    for m in maps {
        let kind = classify_map(m);
        if kind != MapKind::DefaultSelected {
            continue;
        }
        regions += 1;
        let name = format!("{:04}_{:x}_{:x}.bin", regions, m.start, m.end);
        match dump_region(pid, m, &dir.join(&name)) {
            Ok((written, zeroed, _)) => {
                bytes += written;
                zero_bytes += zeroed;
                manifest.push_str(&format!(
                    "{}  va={:x}-{:x}  bytes={} zeroed={}\n",
                    name, m.start, m.end, written, zeroed
                ));
            }
            Err(e) => {
                failed_regions += 1;
                manifest.push_str(&format!("{}  va={:x}-{:x}  FAILED: {}\n", name, m.start, m.end, e));
            }
        }
    }
    let summary = format!(
        "regions={} bytes={} zeroed={} failed_regions={}\n",
        regions, bytes, zero_bytes, failed_regions
    );
    fs::write(dir.join("manifest.txt"), format!("{}{}", summary, manifest))?;

    Ok(DumpReport {
        dir,
        regions,
        bytes,
        zero_bytes,
    })
}
