//! 終端台: 截獲本行程的 stdout / stderr, 供 egui 窗口原樣顯示。
//! 不另立日誌管道 — println! / eprintln! 寫出的就是終端台的內容;
//! 原 fd 保留副本, 截獲的同時照舊寫回去, logcat 等下游行為不變。

use std::sync::{Arc, Mutex, OnceLock};

/// 環形緩衝上限, 超出時從行首裁剪
const MAX_BYTES: usize = 64 * 1024;

pub struct Console {
    buf: Mutex<Vec<u8>>,
}

impl Console {
    fn push(&self, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let mut buf = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        buf.extend_from_slice(data);
        if buf.len() > MAX_BYTES {
            let excess = buf.len() - MAX_BYTES;
            let cut = buf[excess..]
                .iter()
                .position(|&b| b == b'\n')
                .map(|p| excess + p + 1)
                .unwrap_or(excess);
            buf.drain(..cut);
        }
    }

    pub fn text(&self) -> String {
        let buf = self.buf.lock().unwrap_or_else(|e| e.into_inner());
        String::from_utf8_lossy(&buf).into_owned()
    }
}

static CONSOLE: OnceLock<Arc<Console>> = OnceLock::new();

pub fn text() -> Option<String> {
    CONSOLE.get().map(|c| c.text())
}

#[cfg(unix)]
pub fn init() {
    if CONSOLE.get().is_some() {
        return;
    }
    let console = Arc::new(Console {
        buf: Mutex::new(Vec::with_capacity(16 * 1024)),
    });
    for fd in [1i32, 2i32] {
        unsafe {
            let saved = libc::dup(fd);
            if saved < 0 {
                continue;
            }
            let mut pipe_fds = [0i32; 2];
            if libc::pipe(pipe_fds.as_mut_ptr()) != 0 {
                libc::close(saved);
                continue;
            }
            libc::dup2(pipe_fds[1], fd);
            libc::close(pipe_fds[1]);
            let reader = Arc::clone(&console);
            let read_end = pipe_fds[0];
            std::thread::spawn(move || {
                let mut chunk = [0u8; 8192];
                loop {
                    let n = libc::read(
                        read_end,
                        chunk.as_mut_ptr() as *mut libc::c_void,
                        chunk.len(),
                    );
                    if n <= 0 {
                        break;
                    }
                    let data = &chunk[..n as usize];
                    reader.push(data);
                    libc::write(saved, data.as_ptr() as *const libc::c_void, data.len());
                }
                libc::close(read_end);
                libc::close(saved);
            });
        }
    }
    let _ = CONSOLE.set(console);
}

#[cfg(not(unix))]
pub fn init() {}
