use crate::ui::AppState;
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::AsRawFd;
use std::ptr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const EV_KEY: u16 = 0x01;
const KEY_VOLUMEDOWN: u16 = 114;
const KEY_VOLUMEUP: u16 = 115;
const KEY_MAX: usize = 0x2ff;

#[repr(C)]
#[derive(Clone, Copy)]
struct InputEvent {
    time_sec: i64,
    time_usec: i64,
    ev_type: u16,
    code: u16,
    value: i32,
}

pub fn start(app: Arc<Mutex<AppState>>, preferred: Option<String>) {
    std::thread::spawn(move || {
        let file = match preferred {
            Some(path) => open_volume_device(&path),
            None => find_volume_device(),
        };
        let file = match file {
            Some(v) => v,
            None => {
                eprintln!("[keys] no volume key device found");
                return;
            }
        };
        eprintln!("[keys] volume device ready");
        read_loop(file, app);
    });
}

fn find_volume_device() -> Option<File> {
    let mut paths = Vec::new();
    if let Ok(entries) = std::fs::read_dir("/dev/input") {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with("event") && name[5..].chars().all(|c| c.is_ascii_digit()) {
                paths.push(entry.path());
            }
        }
    }
    paths.sort();
    for path in paths {
        if let Some(file) = open_volume_device(path.to_string_lossy().as_ref()) {
            return Some(file);
        }
    }
    None
}

fn open_volume_device(path: &str) -> Option<File> {
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
        .open(path)
        .ok()?;
    let fd = file.as_raw_fd();
    let mut bits = [0u8; KEY_MAX / 8 + 1];
    let res = unsafe {
        libc::ioctl(
            fd,
            eviocgbit(EV_KEY as u32, bits.len() as u32) as _,
            bits.as_mut_ptr(),
        )
    };
    if res <= 0 {
        return None;
    }
    if has_bit(&bits, KEY_VOLUMEUP) || has_bit(&bits, KEY_VOLUMEDOWN) {
        Some(file)
    } else {
        None
    }
}

fn has_bit(bits: &[u8], bit: u16) -> bool {
    let bit = bit as usize;
    bit / 8 < bits.len() && (bits[bit / 8] & (1 << (bit % 8))) != 0
}

fn read_loop(file: File, app: Arc<Mutex<AppState>>) {
    let fd = file.as_raw_fd();
    let mut buf = [0u8; std::mem::size_of::<InputEvent>() * 32];
    let mut last_toggle = Instant::now() - Duration::from_secs(1);
    loop {
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n > 0 {
            let count = n as usize / std::mem::size_of::<InputEvent>();
            for i in 0..count {
                let ev = unsafe {
                    ptr::read_unaligned(buf.as_ptr().add(i * std::mem::size_of::<InputEvent>())
                        as *const InputEvent)
                };
                if ev.ev_type == EV_KEY
                    && (ev.code == KEY_VOLUMEUP || ev.code == KEY_VOLUMEDOWN)
                    && ev.value == 1
                    && last_toggle.elapsed() > Duration::from_millis(250)
                {
                    if let Ok(mut state) = app.lock() {
                        state
                            .collapse_toggle
                            .store(true, std::sync::atomic::Ordering::Relaxed);
                        state.revision = state.revision.wrapping_add(1);
                        eprintln!("[keys] volume toggle collapse");
                    }
                    last_toggle = Instant::now();
                }
            }
        } else if n == 0 {
            std::thread::sleep(Duration::from_millis(20));
        } else {
            let err = std::io::Error::last_os_error();
            let raw = err.raw_os_error();
            if raw == Some(libc::EAGAIN) || raw == Some(libc::EWOULDBLOCK) {
                std::thread::sleep(Duration::from_millis(8));
            } else if raw == Some(libc::EINTR) {
                continue;
            } else {
                eprintln!("[keys] read failed: {err}");
                return;
            }
        }
    }
}

fn ioc(dir: u32, ty: u32, nr: u32, size: u32) -> u32 {
    (dir << 30) | (size << 16) | (ty << 8) | nr
}

fn eviocgbit(ev: u32, len: u32) -> u32 {
    ioc(2, 'E' as u32, 0x20 + ev, len)
}
