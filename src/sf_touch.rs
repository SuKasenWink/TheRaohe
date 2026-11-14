use crate::display::ScreenInfo;
use crate::ui::{self, AppState};
use std::fs::{File, OpenOptions};
use std::os::unix::fs::OpenOptionsExt;
use std::os::unix::io::{AsRawFd, RawFd};
use std::ptr;
use std::sync::{Arc, Mutex};

const EV_SYN: u16 = 0x00;
const EV_ABS: u16 = 0x03;
const SYN_REPORT: u16 = 0x00;
const ABS_MT_SLOT: u16 = 0x2f;
const ABS_MT_POSITION_X: u16 = 0x35;
const ABS_MT_POSITION_Y: u16 = 0x36;
const ABS_MT_TRACKING_ID: u16 = 0x39;
const ABS_MAX: usize = 0x3f;
const SLOT_COUNT: usize = 10;

#[repr(C)]
#[derive(Clone, Copy)]
struct InputAbsInfo {
    value: i32,
    minimum: i32,
    maximum: i32,
    fuzz: i32,
    flat: i32,
    resolution: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct InputEvent {
    time_sec: i64,
    time_usec: i64,
    ev_type: u16,
    code: u16,
    value: i32,
}

#[derive(Clone, Copy)]
struct Slot {
    down: bool,
    x: i32,
    y: i32,
    pos_valid: bool,
    pending_down: bool,
    pending_up: bool,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            down: false,
            x: 0,
            y: 0,
            pos_valid: false,
            pending_down: false,
            pending_up: false,
        }
    }
}

struct TouchDevice {
    fd: RawFd,
    x_min: i32,
    x_max: i32,
    y_min: i32,
    y_max: i32,
}

pub fn start(
    app: Arc<Mutex<AppState>>,
    screen: Arc<Mutex<ScreenInfo>>,
    layer_x: f32,
    layer_y: f32,
    preferred: Option<String>,
    ctx: egui::Context,
) {
    std::thread::spawn(move || {
        let (device, initial) = match preferred {
            Some(path) => match TouchDevice::open(&path) {
                Some(d) => (d, None),
                None => {
                    eprintln!("[touch] device open failed: {path}");
                    return;
                }
            },
            None => match blind_detect() {
                Some((d, buf)) => (d, Some(buf)),
                None => {
                    eprintln!("[touch] no touch device found");
                    return;
                }
            },
        };
        eprintln!(
            "[touch] device open: x={}..{} y={}..{}",
            device.x_min, device.x_max, device.y_min, device.y_max
        );
        read_loop(&device, app, screen, layer_x, layer_y, initial, &ctx);
    });
}

impl TouchDevice {
    /// 打開指定的設備節點; 不是 MT 觸控設備則回傳 None
    fn open(path: &str) -> Option<Self> {
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
            .open(path)
            .ok()?;
        let fd = file.as_raw_fd();
        let (x_min, x_max, y_min, y_max) = Self::validate(fd)?;
        // 永久保留 fd, 由行程等級共享, 不再關閉
        std::mem::forget(file);
        Some(Self {
            fd,
            x_min,
            x_max,
            y_min,
            y_max,
        })
    }

    /// 確認 fd 是多指觸控設備, 回傳絕對座標範圍
    fn validate(fd: RawFd) -> Option<(i32, i32, i32, i32)> {
        let mut bits = [0u8; ABS_MAX / 8 + 1];
        let res = unsafe {
            libc::ioctl(
                fd,
                eviocgbit(EV_ABS as u32, bits.len() as u32) as _,
                bits.as_mut_ptr(),
            )
        };
        if res <= 0 {
            return None;
        }
        if !has_bit(&bits, ABS_MT_SLOT)
            || !has_bit(&bits, ABS_MT_POSITION_X)
            || !has_bit(&bits, ABS_MT_POSITION_Y)
        {
            return None;
        }
        let ax = get_abs(fd, ABS_MT_POSITION_X)?;
        let ay = get_abs(fd, ABS_MT_POSITION_Y)?;
        if ax.maximum <= ax.minimum || ay.maximum <= ay.minimum {
            return None;
        }
        Some((ax.minimum, ax.maximum, ay.minimum, ay.maximum))
    }
}

/// 非阻塞盲開 /dev/input/event0..15, 用 poll 把 fd 託管給核心休眠監聽;
/// 使用者第一次觸摸螢幕時核心喚醒 poll, 誰先讀出事件誰就是觸控螢幕。
/// 有事件但不是 MT 觸控設備(音量鍵/感測器等)的節點視為輸家, 立即踢掉;
/// 贏家的 fd 以 mem::forget 永久保留, 其餘輸家由 RAII 自動關閉。
fn blind_detect() -> Option<(TouchDevice, Vec<u8>)> {
    let mut candidates: Vec<File> = (0..=15i32)
        .filter_map(|i| {
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC)
                .open(format!("/dev/input/event{i}"))
                .ok()
        })
        .collect();
    eprintln!("[touch] blind open: {} candidates, waiting for touch", candidates.len());
    let mut buf = [0u8; std::mem::size_of::<InputEvent>() * 64];
    loop {
        if candidates.is_empty() {
            return None;
        }
        let mut pfds: Vec<libc::pollfd> = candidates
            .iter()
            .map(|f| libc::pollfd {
                fd: f.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let n = unsafe { libc::poll(pfds.as_mut_ptr(), pfds.len() as libc::nfds_t, -1) };
        if n < 0 {
            let err = std::io::Error::last_os_error();
            if err.raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            eprintln!("[touch] poll failed: {err}");
            return None;
        }
        // 由後往前檢查, 方便原地移除輸家
        for idx in (0..pfds.len()).rev() {
            let revents = pfds[idx].revents;
            if revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
                candidates.remove(idx);
                continue;
            }
            if revents & libc::POLLIN == 0 {
                continue;
            }
            let fd = candidates[idx].as_raw_fd();
            let bytes =
                unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
            if bytes < 0 {
                let raw = std::io::Error::last_os_error().raw_os_error();
                if raw == Some(libc::EAGAIN) || raw == Some(libc::EWOULDBLOCK) {
                    continue;
                }
                candidates.remove(idx);
                continue;
            }
            if bytes == 0 {
                candidates.remove(idx);
                continue;
            }
            match TouchDevice::validate(fd) {
                Some((x_min, x_max, y_min, y_max)) => {
                    let file = candidates.remove(idx);
                    let fd = file.as_raw_fd();
                    std::mem::forget(file);
                    let device = TouchDevice {
                        fd,
                        x_min,
                        x_max,
                        y_min,
                        y_max,
                    };
                    // 觸發喚醒的這一筆事件是真實的觸控, 不能丟棄
                    return Some((device, buf[..bytes as usize].to_vec()));
                }
                None => {
                    candidates.remove(idx);
                }
            }
        }
    }
}

fn has_bit(bits: &[u8], bit: u16) -> bool {
    let bit = bit as usize;
    bit / 8 < bits.len() && (bits[bit / 8] & (1 << (bit % 8))) != 0
}

fn get_abs(fd: RawFd, code: u16) -> Option<InputAbsInfo> {
    let mut info = InputAbsInfo {
        value: 0,
        minimum: 0,
        maximum: 0,
        fuzz: 0,
        flat: 0,
        resolution: 0,
    };
    let res = unsafe {
        libc::ioctl(
            fd,
            eviocgabs(code as u32) as _,
            &mut info as *mut InputAbsInfo,
        )
    };
    if res == 0 {
        Some(info)
    } else {
        None
    }
}

struct SlotMachine {
    slots: [Slot; SLOT_COUNT],
    current_slot: usize,
    active: Option<usize>,
    last_sent: Option<(i32, i32)>,
}

impl Default for SlotMachine {
    fn default() -> Self {
        Self {
            slots: [Slot::default(); SLOT_COUNT],
            current_slot: 0,
            active: None,
            last_sent: None,
        }
    }
}

impl SlotMachine {
    fn process(
        &mut self,
        buf: &[u8],
        device: &TouchDevice,
        app: &Arc<Mutex<AppState>>,
        screen: &Arc<Mutex<ScreenInfo>>,
        layer_x: f32,
        layer_y: f32,
        ctx: &egui::Context,
    ) {
        let count = buf.len() / std::mem::size_of::<InputEvent>();
        for i in 0..count {
            let ev = unsafe {
                ptr::read_unaligned(
                    buf.as_ptr().add(i * std::mem::size_of::<InputEvent>()) as *const InputEvent
                )
            };
            if ev.ev_type == EV_ABS {
                match ev.code {
                    ABS_MT_SLOT => {
                        if ev.value >= 0 && (ev.value as usize) < SLOT_COUNT {
                            self.current_slot = ev.value as usize;
                        }
                    }
                    ABS_MT_TRACKING_ID => {
                        let slot = &mut self.slots[self.current_slot];
                        if ev.value == -1 {
                            if slot.down {
                                slot.pending_up = true;
                            }
                        } else {
                            if !slot.down {
                                slot.pos_valid = false;
                            }
                            slot.down = true;
                            slot.pending_down = true;
                        }
                    }
                    ABS_MT_POSITION_X => {
                        self.slots[self.current_slot].x = ev.value;
                        self.slots[self.current_slot].pos_valid = true;
                    }
                    ABS_MT_POSITION_Y => {
                        self.slots[self.current_slot].y = ev.value;
                        self.slots[self.current_slot].pos_valid = true;
                    }
                    _ => {}
                }
            } else if ev.ev_type == EV_SYN && ev.code == SYN_REPORT {
                self.flush_report(device, app, screen, layer_x, layer_y, ctx);
            }
        }
    }

    fn flush_report(
        &mut self,
        device: &TouchDevice,
        app: &Arc<Mutex<AppState>>,
        screen: &Arc<Mutex<ScreenInfo>>,
        layer_x: f32,
        layer_y: f32,
        ctx: &egui::Context,
    ) {
        let info = match screen.lock() {
            Ok(v) => *v,
            Err(_) => return,
        };
        let ppp = ctx.pixels_per_point();
        for index in 0..self.slots.len() {
            let (pending_down, pending_up) = {
                let slot = &self.slots[index];
                (slot.pending_down, slot.pending_up)
            };

            if pending_down {
                if self.active.is_none() {
                    let (pos_valid, x, y) = {
                        let slot = &self.slots[index];
                        (slot.pos_valid, slot.x, slot.y)
                    };
                    if pos_valid {
                        self.slots[index].pending_down = false;
                        if let Some((lx, ly)) = map_touch(x, y, device, &info, layer_x, layer_y)
                            .map(|(x, y)| (x / ppp, y / ppp))
                        {
                            // Only a finger that lands on a panel becomes the UI
                            // pointer; fingers that start elsewhere (the app
                            // underneath) are ignored for their whole gesture.
                            if hits_panel(app, lx, ly) {
                                self.active = Some(index);
                                self.last_sent = Some((x, y));
                                ui::push_touch(
                                    app,
                                    egui::TouchId(index as u64),
                                    egui::TouchPhase::Start,
                                    lx,
                                    ly,
                                );
                                eprintln!(
                                    "[touch] start raw=({}, {}) local_pt=({:.1}, {:.1})",
                                    x, y, lx, ly
                                );
                            }
                        }
                    }
                } else {
                    self.slots[index].pending_down = false;
                }
            }

            if self.active == Some(index) {
                let slot = &mut self.slots[index];
                if pending_up {
                    if let Some((lx, ly)) =
                        map_touch(slot.x, slot.y, device, &info, layer_x, layer_y)
                            .map(|(x, y)| (x / ppp, y / ppp))
                    {
                        ui::push_touch(
                            app,
                            egui::TouchId(index as u64),
                            egui::TouchPhase::End,
                            lx,
                            ly,
                        );
                    }
                    slot.down = false;
                    slot.pos_valid = false;
                    slot.pending_up = false;
                    self.active = None;
                    self.last_sent = None;
                } else if slot.down && self.last_sent != Some((slot.x, slot.y)) {
                    if let Some((lx, ly)) =
                        map_touch(slot.x, slot.y, device, &info, layer_x, layer_y)
                            .map(|(x, y)| (x / ppp, y / ppp))
                    {
                        ui::push_touch(
                            app,
                            egui::TouchId(index as u64),
                            egui::TouchPhase::Move,
                            lx,
                            ly,
                        );
                        self.last_sent = Some((slot.x, slot.y));
                    }
                }
            } else if pending_up {
                let slot = &mut self.slots[index];
                slot.down = false;
                slot.pending_up = false;
                slot.pending_down = false;
                slot.pos_valid = false;
            }
        }
    }
}

fn read_loop(
    device: &TouchDevice,
    app: Arc<Mutex<AppState>>,
    screen: Arc<Mutex<ScreenInfo>>,
    layer_x: f32,
    layer_y: f32,
    initial: Option<Vec<u8>>,
    ctx: &egui::Context,
) {
    let fd = device.fd;
    let mut machine = SlotMachine::default();
    // 檢測階段讀到的第一筆事件包含真實觸控(按下), 直接餵給狀態機, 不能丟掉
    if let Some(buf) = initial.as_deref() {
        machine.process(buf, device, &app, &screen, layer_x, layer_y, ctx);
    }
    let mut buf = [0u8; std::mem::size_of::<InputEvent>() * 64];

    loop {
        // 休眠監聽: fd 託管給核心, 有事件才被喚醒, 平時 0% CPU
        let mut pfd = libc::pollfd {
            fd,
            events: libc::POLLIN,
            revents: 0,
        };
        let n = unsafe { libc::poll(&mut pfd, 1, 250) };
        if n < 0 {
            let raw = std::io::Error::last_os_error().raw_os_error();
            if raw == Some(libc::EINTR) {
                continue;
            }
            eprintln!("[touch] poll failed");
            return;
        }
        if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 {
            eprintln!("[touch] device gone");
            return;
        }
        if pfd.revents & libc::POLLIN == 0 {
            continue;
        }
        let n = unsafe { libc::read(fd, buf.as_mut_ptr() as *mut libc::c_void, buf.len()) };
        if n > 0 {
            machine.process(&buf[..n as usize], device, &app, &screen, layer_x, layer_y, ctx);
        } else if n < 0 {
            let raw = std::io::Error::last_os_error().raw_os_error();
            if raw != Some(libc::EAGAIN) && raw != Some(libc::EWOULDBLOCK) && raw != Some(libc::EINTR) {
                eprintln!("[touch] read failed");
                return;
            }
        } else {
            eprintln!("[touch] device closed");
            return;
        }
    }
}

fn hits_panel(app: &Arc<Mutex<AppState>>, x: f32, y: f32) -> bool {
    match app.lock() {
        Ok(state) => {
            state.visible && state.hit_rects.iter().any(|r| r.contains(egui::pos2(x, y)))
        }
        Err(_) => false,
    }
}

fn map_touch(
    x: i32,
    y: i32,
    device: &TouchDevice,
    info: &ScreenInfo,
    layer_x: f32,
    layer_y: f32,
) -> Option<(f32, f32)> {
    let ux = unit(x, device.x_min, device.x_max);
    let uy = unit(y, device.y_min, device.y_max);
    let (natural_w, natural_h) = if info.rotation % 2 == 1 {
        (info.height as f32, info.width as f32)
    } else {
        (info.width as f32, info.height as f32)
    };
    let (lx, ly) = match info.rotation {
        0 => (ux * natural_w, uy * natural_h),
        1 => (uy * natural_h, (1.0 - ux) * natural_w),
        2 => ((1.0 - ux) * natural_w, (1.0 - uy) * natural_h),
        _ => ((1.0 - uy) * natural_h, ux * natural_w),
    };
    Some((lx - layer_x, ly - layer_y))
}

fn unit(value: i32, min: i32, max: i32) -> f32 {
    if max <= min {
        return 0.0;
    }
    ((value - min) as f32 / (max - min) as f32).clamp(0.0, 1.0)
}

fn ioc(dir: u32, ty: u32, nr: u32, size: u32) -> u32 {
    (dir << 30) | (size << 16) | (ty << 8) | nr
}

fn eviocgbit(ev: u32, len: u32) -> u32 {
    ioc(2, 'E' as u32, 0x20 + ev, len)
}

fn eviocgabs(abs: u32) -> u32 {
    ioc(
        2,
        'E' as u32,
        0x40 + abs,
        std::mem::size_of::<InputAbsInfo>() as u32,
    )
}
