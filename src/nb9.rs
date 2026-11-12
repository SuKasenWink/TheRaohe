#![allow(non_camel_case_types)]

use std::ffi::c_void;
use std::mem::size_of;

const OP_CMD_READ: u64 = 601;
const OP_CMD_HOOK_STATUS: u64 = 618;

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

const _: () = assert!(size_of::<CopyMemory>() == 32);
const _: () = assert!(size_of::<HookStatus>() == 72);

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
