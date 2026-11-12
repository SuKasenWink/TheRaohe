//! 螢幕截圖: 底層畫面由 screencap 拿, overlay 由 app 自己合成上去
//! 路徑與命名向 MediaStore 查詢, 不在程式裡硬編碼
//! `content query` 讀 Screenshots 桶, 從最新樣本的 `_data` 拿目錄、`_display_name` 抽
//! 命名規律(如 Screenshot_20261111-204859.png → 前綴 "Screenshot_" + yyyyMMdd-HHmmss),
//! 任何一步失敗就落回 AOSP 默認

use std::io::BufReader;
use std::path::Path;

/// overlay 只在自己的 layer 裡渲染, 螢幕底層由 screencap 取回
/// `canvas` 是 app 記憶中最近一幀已合成的 overlay(RGBA, 寬高 = 可見區域),
/// `offset` 是 overlay 相對底層螢幕的原點
pub fn capture_canvas(
    canvas: &[[u8; 4]],
    width: usize,
    height: usize,
    offset: (i32, i32),
) -> Result<String, String> {
    let (dir, prefix) = discover().unwrap_or_else(|| (default_dir(), "Screenshot_".to_string()));
    let stamp = android_stamp();
    let final_path = Path::new(&dir).join(format!("{prefix}{stamp}.png"));
    let temp_path = format!("/data/local/tmp/theraohe-cap-{stamp}.png");

    let out = std::process::Command::new("screencap")
        .arg("-p")
        .arg(&temp_path)
        .output()
        .map_err(|e| format!("啟動 screencap 失敗: {e}"))?;
    if !out.status.success() {
        let code = out.status.code().unwrap_or(-1);
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(format!("screencap 退出碼 {code}: {}", err.trim()));
    }

    let (sw, sh, mut screen) = decode_png(&temp_path)
        .map_err(|e| format!("讀取 screencap 結果失敗: {e}"))?;
    let _ = std::fs::remove_file(&temp_path);

    composite(&mut screen, sw, sh, canvas, width, height, offset);

    std::fs::create_dir_all(&dir).map_err(|e| format!("建立目錄 {dir} 失敗: {e}"))?;
    encode_png(&final_path, sw, sh, &screen).map_err(|e| format!("寫入截圖失敗: {e}"))?;
    Ok(final_path.to_string_lossy().into_owned())
}

fn decode_png(path: &str) -> Result<(usize, usize, Vec<u8>), String> {
    let file = std::fs::File::open(path).map_err(|e| e.to_string())?;
    let decoder = png::Decoder::new(BufReader::new(file));
    let mut reader = decoder.read_info().map_err(|e| e.to_string())?;
    let mut buffer = vec![0u8; reader.output_buffer_size()];
    let info = reader.next_frame(&mut buffer).map_err(|e| e.to_string())?;
    let bytes = &buffer[..info.buffer_size()];
    match info.color_type {
        png::ColorType::Rgba => Ok((info.width as usize, info.height as usize, bytes.to_vec())),
        png::ColorType::Rgb => {
            let mut rgba = Vec::with_capacity(bytes.len() / 3 * 4);
            for px in bytes.chunks_exact(3) {
                rgba.extend_from_slice(px);
                rgba.push(255);
            }
            Ok((info.width as usize, info.height as usize, rgba))
        }
        other => Err(format!("不支援的 PNG 色彩類型 {other:?}")),
    }
}

fn encode_png(path: &Path, width: usize, height: usize, rgba: &[u8]) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), width as u32, height as u32);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(|e| e.to_string())?;
    writer.write_image_data(rgba).map_err(|e| e.to_string())?;
    Ok(())
}

// 疊加 overlay 到 screencap 上; 只需處理有 alpha 的像素
fn composite(
    screen: &mut [u8],
    sw: usize,
    sh: usize,
    canvas: &[[u8; 4]],
    width: usize,
    height: usize,
    offset: (i32, i32),
) {
    let (ox, oy) = offset;
    for row in 0..height {
        let sy = oy + row as i32;
        if sy < 0 || sy >= sh as i32 {
            continue;
        }
        let srow = sy as usize * sw * 4;
        let crow = row * width;
        for col in 0..width {
            let sx = ox + col as i32;
            if sx < 0 || sx >= sw as i32 {
                continue;
            }
            let soff = srow + sx as usize * 4;
            let spx = &mut screen[soff..soff + 4];
            let opx = canvas[crow + col];
            let a = opx[3] as u32;
            if a == 0 {
                continue;
            }
            let inv = 255 - a;
            for i in 0..3 {
                spx[i] = ((opx[i] as u32 * a + spx[i] as u32 * inv) / 255) as u8;
            }
            spx[3] = 255;
        }
    }
}

fn default_dir() -> String {
    "/sdcard/Pictures/Screenshots".to_string()
}

fn discover() -> Option<(String, String)> {
    let out = std::process::Command::new("content")
        .args([
            "query",
            "--uri",
            "content://media/external/images/media",
            "--projection",
            "_data:_display_name",
            "--where",
            "bucket_display_name='Screenshots'",
            "--sort",
            "date_modified DESC",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let Some(data) = field(line, "_data=") else {
            continue;
        };
        let Some(name) = field(line, "_display_name=") else {
            continue;
        };
        if let Some(target) = pattern(data, name) {
            return Some(target);
        }
    }
    None
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(key)? + key.len();
    let rest = &line[start..];
    let end = rest.find(',').unwrap_or(rest.len());
    Some(rest[..end].trim_end())
}

// 從樣本中抽出 目錄與前綴: _data=/storage/.../Screenshot_20261111-204859.png
// _display_name=Screenshot_20261111-204859.png → (/storage/.../Screenshots, "Screenshot_")
fn pattern(data: &str, name: &str) -> Option<(String, String)> {
    let dir = Path::new(data).parent()?.to_str()?.to_string();
    let body = name.strip_suffix(".png")?;
    let dash = body.rfind('-')?;
    let (head, time) = body.split_at(dash);
    let time = &time[1..];
    if time.len() != 6 || !time.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    if head.len() < 8 {
        return None;
    }
    let date = &head[head.len() - 8..];
    if !date.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((dir, head[..head.len() - 8].to_string()))
}

fn android_stamp() -> String {
    // 設備本地時間, yyyyMMdd-HHmmss, 與 AOSP 截圖檔名一致
    let t = unsafe { libc::time(std::ptr::null_mut()) };
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe {
        libc::localtime_r(&t, &mut tm);
    }
    format!(
        "{:04}{:02}{:02}-{:02}{:02}{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}
