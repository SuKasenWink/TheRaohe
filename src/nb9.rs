#![allow(non_camel_case_types)]

use std::ffi::c_void;
use std::mem::size_of;

const OP_CMD_READ: u64 = 601;
const OP_CMD_HOOK_STATUS: u64 = 618;
const OP_CMD_TOUCH_INIT: u64 = 628;
const OP_CMD_TOUCH_EVENT: u64 = 629;
pub const TOUCH_DOWN: u32 = 1;
pub const TOUCH_MOVE: u32 = 2;
pub const TOUCH_UP: u32 = 3;
// 疊加層要注入的虛擬節點固定用第 0 槽; 與實指不衝突
pub const TOUCH_SLOT_MASK: u16 = 0x0001;

#[repr(C)]
struct CopyMemory {
    pid: i32,
    _pad: i32,
    addr: usize,
    buffer: *mut c_void,
    size: usize,
}

#[repr(C)]
struct HookStatus {
    raw: [u8; 72],
}

#[repr(C)]
struct TouchInit {
    max_x: i32,
    max_y: i32,
}

#[repr(C)]
struct TouchEvent {
    command: u32,
    x: u32,
    y: u32,
    slot_mask: u16,
    reserved: u16,
}

const _: () = assert!(size_of::<CopyMemory>() == 32);
const _: () = assert!(size_of::<HookStatus>() == 72);
const _: () = assert!(size_of::<TouchInit>() == 8);
const _: () = assert!(size_of::<TouchEvent>() == 16);

fn call(cmd: u64, arg: *mut c_void) -> i32 {
    unsafe { libc::ioctl(0, cmd as libc::c_int, arg) }
}

pub fn probe() -> bool {
    let mut status = HookStatus { raw: [0; 72] };
    call(
        OP_CMD_HOOK_STATUS,
        &mut status as *mut HookStatus as *mut c_void,
    ) == 0
}

fn copy(pid: i32, cmd: u64, addr: u64, buffer: *mut c_void, size: usize) -> bool {
    if pid <= 0 || addr == 0 || buffer.is_null() || size == 0 {
        return false;
    }
    let mut cm = CopyMemory {
        pid,
        _pad: 0,
        addr: addr as usize,
        buffer,
        size,
    };
    call(cmd, &mut cm as *mut CopyMemory as *mut c_void) == 0
}

pub fn read(pid: i32, addr: u64, buffer: &mut [u8]) -> bool {
    copy(
        pid,
        OP_CMD_READ,
        addr,
        buffer.as_mut_ptr() as *mut c_void,
        buffer.len(),
    )
}

pub fn read_value(pid: i32, addr: u64, size: usize) -> Option<u64> {
    if size == 0 || size > 8 {
        return None;
    }
    let mut buf = [0u8; 8];
    if !read(pid, addr, &mut buf[..size]) {
        return None;
    }
    let mut raw = [0u8; 8];
    raw[..size].copy_from_slice(&buf[..size]);
    Some(u64::from_le_bytes(raw))
}

/// 註冊虛擬觸控節點的絕對座標範圍 (原始裝置範圍, 例如 1079x2399)
pub fn touch_init(max_x: u32, max_y: u32) -> bool {
    let mut init = TouchInit {
        max_x: max_x as i32,
        max_y: max_y as i32,
    };
    call(
        OP_CMD_TOUCH_INIT,
        &mut init as *mut TouchInit as *mut c_void,
    ) == 0
}

pub fn touch_event(command: u32, x: u32, y: u32, slot_mask: u16) -> bool {
    if !(command == TOUCH_DOWN || command == TOUCH_MOVE || command == TOUCH_UP) {
        return false;
    }
    if slot_mask == 0 || slot_mask > 0x03ff {
        return false;
    }
    let mut ev = TouchEvent {
        command,
        x,
        y,
        slot_mask,
        reserved: 0,
    };
    call(
        OP_CMD_TOUCH_EVENT,
        &mut ev as *mut TouchEvent as *mut c_void,
    ) == 0
}
