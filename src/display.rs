use std::io::{BufRead, BufReader};
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScreenInfo {
    pub width: i32,
    pub height: i32,
    pub rotation: i32,
}

/// 狀態欄（頂部系統欄）像素高度, 從 `dumpsys window` 的 statusBars InsetsSourceControl 讀。
/// 查不到時回傳 None；調用方決定降級值。
pub fn status_bar_height() -> Option<i32> {
    let out = std::process::Command::new("/system/bin/dumpsys")
        .arg("window")
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        if !line.contains("mType=statusBars") {
            continue;
        }
        let Some(insets) = line.split("mInsetsHint=Insets{").nth(1) else {
            continue;
        };
        for part in insets.split(',') {
            let part = part.trim();
            if let Some(v) = part.strip_prefix("top=") {
                return v.parse().ok();
            }
        }
    }
    None
}

pub fn query() -> Option<ScreenInfo> {
    let out = std::process::Command::new("/system/bin/dumpsys")
        .arg("display")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    parse(&text)
}

pub fn parse(text: &str) -> Option<ScreenInfo> {
    let marker = "mOverrideDisplayInfo=DisplayInfo{";
    let seg = match text.split(marker).nth(1) {
        Some(v) => v,
        None => text.split("mBaseDisplayInfo=DisplayInfo{").nth(1)?,
    };
    let seg = &seg[..seg.len().min(8000)];
    let real = seg.split(" real ").nth(1)?;
    let mut words = real.split_whitespace();
    let width: i32 = words.next()?.parse().ok()?;
    if words.next()? != "x" {
        return None;
    }
    let height: i32 = words.next()?.trim_end_matches(',').parse().ok()?;
    let rotation: i32 = seg
        .split("rotation ")
        .nth(1)?
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    if width <= 0 || height <= 0 || !(0..=3).contains(&rotation) {
        return None;
    }
    Some(ScreenInfo {
        width,
        height,
        rotation,
    })
}

fn apply(screen: &Arc<Mutex<ScreenInfo>>, info: ScreenInfo) -> bool {
    if let Ok(mut current) = screen.lock() {
        if *current != info {
            eprintln!(
                "[display] {}x{} rotation={}",
                info.width, info.height, info.rotation
            );
            *current = info;
            return true;
        }
    }
    false
}

/// An event can arrive slightly before `dumpsys display` reflects it, so retry a
/// few times (bounded, only after an event) until the info actually changes.
fn refresh_after_event(screen: &Arc<Mutex<ScreenInfo>>) {
    for delay_ms in [0u64, 80, 250, 600] {
        if delay_ms > 0 {
            std::thread::sleep(Duration::from_millis(delay_ms));
        }
        if let Some(info) = query() {
            if apply(screen, info) {
                return;
            }
        }
    }
}

/// Block on ActivityManager's `configuration_changed` event (fired on rotation
/// and other configuration changes) instead of polling. Returns false if the
/// event stream is unavailable or ends.
fn watch_events(screen: &Arc<Mutex<ScreenInfo>>) -> bool {
    let mut cmd = Command::new("/system/bin/logcat");
    cmd.args([
        "-b",
        "events",
        "-T",
        "1",
        "-v",
        "raw",
        "configuration_changed:I",
        "*:S",
    ])
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::null());
    // Do not leave logcat behind if we get killed.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL as libc::c_ulong);
            Ok(())
        });
    }
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("[display] cannot start logcat: {e}");
            return false;
        }
    };
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => return false,
    };
    eprintln!("[display] waiting for configuration_changed events (no polling)");
    for line in BufReader::new(stdout).lines() {
        if line.is_err() {
            break;
        }
        refresh_after_event(screen);
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

pub fn start_watch(screen: Arc<Mutex<ScreenInfo>>) {
    std::thread::spawn(move || {
        if watch_events(&screen) {
            return;
        }
        eprintln!("[display] event stream unavailable; falling back to 1s polling");
        loop {
            if let Some(info) = query() {
                apply(&screen, info);
            }
            std::thread::sleep(Duration::from_secs(1));
        }
    });
}
