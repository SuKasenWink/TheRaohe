#![allow(dead_code)]

#![allow(non_camel_case_types)]

use std::ffi::{c_void, CString};
use std::mem::size_of;

const OP_CMD_READ: u64 = 601;
const OP_CMD_WRITE: u64 = 602;
const OP_CMD_BASE: u64 = 603;
const OP_CMD_GETPID: u64 = 604;
const OP_CMD_HIDE_PROCESS: u64 = 605;
const OP_CMD_UN_HOOK: u64 = 606;
const OP_CMD_RECOVER_PROCESS: u64 = 607;
const OP_CMD_GYRO: u64 = 608;
const OP_CMD_HWBP_ADD: u64 = 609;
const OP_CMD_HWBP_GET_HITS: u64 = 610;
const OP_CMD_HWBP_ENABLE: u64 = 611;
const OP_CMD_HWBP_CLEAR: u64 = 612;
const OP_CMD_HWBP_DISABLE: u64 = 613;
const OP_CMD_UPDATE_TRACKING: u64 = 614;
const OP_CMD_HOOK_ATTACH: u64 = 615;
const OP_CMD_HOOK_DETACH: u64 = 616;
const OP_CMD_HOOK_SET_ROT: u64 = 617;
const OP_CMD_HOOK_STATUS: u64 = 618;
const OP_CMD_HOOK_READREG: u64 = 619;
const OP_CMD_READ_SAFE: u64 = 620;
const OP_CMD_WRITE_SAFE: u64 = 621;
const OP_CMD_BRK_ADD: u64 = 622;
const OP_CMD_BRK_GET_HITS: u64 = 623;
const OP_CMD_BRK_ENABLE: u64 = 624;
const OP_CMD_BRK_CLEAR: u64 = 625;
const OP_CMD_BRK_DISABLE: u64 = 626;
const OP_CMD_BRK_UPDATE_TRACKING: u64 = 627;
const OP_CMD_TOUCH_INIT: u64 = 628;
const OP_CMD_TOUCH_EVENT: u64 = 629;
const OP_CMD_TOUCH_DESTROY: u64 = 630;

pub const HWBP_TYPE_R: i32 = 1;
pub const HWBP_TYPE_W: i32 = 2;
pub const HWBP_TYPE_RW: i32 = 3;
pub const HWBP_TYPE_X: i32 = 4;
pub const BRK_TYPE_X: i32 = 4;

pub const MAX_MODIFY_REGS: usize = 10;
pub const STACK_ROT_MAX: usize = 12;

const GYRO_MASK_GYRO: u32 = 1 << 0;
const GYRO_MASK_UNCAL: u32 = 1 << 1;
pub const GYRO_MASK_ALL: u32 = GYRO_MASK_GYRO | GYRO_MASK_UNCAL;

pub const TOUCH_DOWN: u32 = 1;
pub const TOUCH_MOVE: u32 = 2;
pub const TOUCH_UP: u32 = 3;
pub const NB9_TOUCH_SLOTS: u16 = 10;
pub const NB9_TOUCH_MASK: u16 = (1 << NB9_TOUCH_SLOTS) - 1;

#[repr(C)]
struct CopyMemory {
    pid: i32,
    _pad: i32,
    addr: usize,
    buffer: *mut c_void,
    size: usize,
}

#[repr(C)]
struct ModuleBase {
    pid: i32,
    _pad: i32,
    name: *const u8,
    base: usize,
    index: i16,
    _pad2: [u8; 6],
}

#[repr(C)]
pub struct TrackingData {
    pub is_active: bool,
    _pad: [u8; 7],
    pub bp_addr: usize,
    pub x: f32,
    pub y: f32,
    pub z: f32,
    _pad2: u32,
}

#[repr(C)]
#[derive(Clone)]
pub struct RegsInfo {
    pub regs: [u64; 31],
    pub sp: u64,
    pub pc: u64,
    pub pstate: u64,
}

#[repr(C)]
#[derive(Clone)]
pub struct BpInfo {
    pub pid: i32,
    _pad: i32,
    pub addr: usize,
    pub bp_type: i32,
    pub len: i32,
    pub is_write_gp_regs: bool,
    _pad2: [u8; 3],
    pub gp_reg_count: i32,
    pub gp_reg_indices: [i32; MAX_MODIFY_REGS],
    pub gp_reg_values: [u64; MAX_MODIFY_REGS],
    pub is_write_fp_regs: bool,
    _pad3: [u8; 3],
    pub fp_reg_count: i32,
    pub fp_reg_indices: [i32; MAX_MODIFY_REGS],
    pub fp_reg_values: [[u64; 2]; MAX_MODIFY_REGS],
}

impl BpInfo {
    pub fn new(pid: i32, addr: u64, bp_type: i32, len: i32) -> Self {
        BpInfo {
            pid,
            _pad: 0,
            addr: addr as usize,
            bp_type,
            len,
            is_write_gp_regs: false,
            _pad2: [0; 3],
            gp_reg_count: 0,
            gp_reg_indices: [0; MAX_MODIFY_REGS],
            gp_reg_values: [0; MAX_MODIFY_REGS],
            is_write_fp_regs: false,
            _pad3: [0; 3],
            fp_reg_count: 0,
            fp_reg_indices: [0; MAX_MODIFY_REGS],
            fp_reg_values: [[0; 2]; MAX_MODIFY_REGS],
        }
    }
}

impl Default for RegsInfo {
    fn default() -> Self {
        RegsInfo {
            regs: [0; 31],
            sp: 0,
            pc: 0,
            pstate: 0,
        }
    }
}

#[repr(C)]
pub struct HwBpHitItem {
    pub task_id: i32,
    _pad: i32,
    pub hit_addr: usize,
    pub hit_time: u64,
    pub regs_info: RegsInfo,
}

#[repr(C)]
pub struct BrkBpHitItem {
    pub task_id: i32,
    _pad: i32,
    pub hit_addr: usize,
    pub hit_time: u64,
    pub regs_info: RegsInfo,
}

#[repr(C)]
pub struct HwBpHitArgs {
    pub pid: i32,
    _pad: i32,
    pub addr: usize,
    pub out_buf: *mut HwBpHitItem,
    pub out_len: i32,
    pub real_count: i32,
}

#[repr(C)]
pub struct BrkBpHitArgs {
    pub pid: i32,
    _pad: i32,
    pub addr: usize,
    pub out_buf: *mut BrkBpHitItem,
    pub out_len: i32,
    pub real_count: i32,
}

#[repr(C)]
struct HookAttach {
    pid: i32,
    _pad: i32,
    target_addr: usize,
}

#[repr(C)]
pub struct HookSetRot {
    pub rot: [u32; 3],
    pub stack_rot: [u32; STACK_ROT_MAX],
    pub stack_count: u32,
    pub stack_offset: u32,
}

#[repr(C)]
pub struct HookStatus {
    pub active: u8,
    pub armed: u8,
    _pad: [u8; 2],
    pub pid: i32,
    pub vaddr: usize,
    pub target_addr: usize,
    pub rot: [u32; 3],
    pub stack_rot: [u32; 3],
    pub stack_count: u32,
    pub stack_offset: u32,
    pub hit_count: u64,
    pub captured_x0: u64,
}

#[repr(C)]
struct HookReadReg {
    reg_idx: u32,
    _pad: u32,
    value: u64,
    hit_count: u64,
}

#[repr(C)]
struct GyroConfigCmd {
    enable: i32,
    type_mask: u32,
    x: f32,
    y: f32,
    z: f32,
}

#[repr(C)]
pub struct TouchInit {
    pub max_x: i32,
    pub max_y: i32,
}

#[repr(C)]
pub struct TouchEvent {
    pub command: u32,
    pub x: u32,
    pub y: u32,
    pub slot_mask: u16,
    pub reserved: u16,
}

const _: () = assert!(size_of::<CopyMemory>() == 32);
const _: () = assert!(size_of::<ModuleBase>() == 32);
const _: () = assert!(size_of::<TrackingData>() == 32);
const _: () = assert!(size_of::<BpInfo>() == 360);
const _: () = assert!(size_of::<RegsInfo>() == 272);
const _: () = assert!(size_of::<HwBpHitItem>() == 296);
const _: () = assert!(size_of::<BrkBpHitItem>() == 296);
const _: () = assert!(size_of::<HwBpHitArgs>() == 32);
const _: () = assert!(size_of::<BrkBpHitArgs>() == 32);
const _: () = assert!(size_of::<HookAttach>() == 16);
const _: () = assert!(size_of::<HookSetRot>() == 68);
const _: () = assert!(size_of::<HookStatus>() == 72);
const _: () = assert!(size_of::<HookReadReg>() == 24);
const _: () = assert!(size_of::<GyroConfigCmd>() == 20);
const _: () = assert!(size_of::<TouchInit>() == 8);
const _: () = assert!(size_of::<TouchEvent>() == 16);

fn call(cmd: u64, arg: *mut c_void) -> i32 {
    unsafe { libc::ioctl(0, cmd as libc::c_int, arg) }
}

pub fn probe() -> bool {
    let mut status = HookStatus {
        active: 0,
        armed: 0,
        _pad: [0; 2],
        pid: 0,
        vaddr: 0,
        target_addr: 0,
        rot: [0; 3],
        stack_rot: [0; 3],
        stack_count: 0,
        stack_offset: 0,
        hit_count: 0,
        captured_x0: 0,
    };
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

pub fn write(pid: i32, addr: u64, buffer: &[u8]) -> bool {
    copy(
        pid,
        OP_CMD_WRITE,
        addr,
        buffer.as_ptr() as *mut c_void,
        buffer.len(),
    )
}

pub fn read_safe(pid: i32, addr: u64, buffer: &mut [u8]) -> bool {
    copy(
        pid,
        OP_CMD_READ_SAFE,
        addr,
        buffer.as_mut_ptr() as *mut c_void,
        buffer.len(),
    )
}

pub fn write_safe(pid: i32, addr: u64, buffer: &[u8]) -> bool {
    copy(
        pid,
        OP_CMD_WRITE_SAFE,
        addr,
        buffer.as_ptr() as *mut c_void,
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

/// 模組基底（按 driver 的模組表，非 maps 解析）; index 取同名次序
pub fn module_base(pid: i32, name: &str, index: i16) -> Option<u64> {
    if pid <= 0 || name.is_empty() {
        return None;
    }
    let c_name = CString::new(name).ok()?;
    let mut mb = ModuleBase {
        pid,
        _pad: 0,
        name: c_name.as_ptr() as *const u8,
        base: 0,
        index,
        _pad2: [0; 6],
    };
    if call(OP_CMD_BASE, &mut mb as *mut ModuleBase as *mut c_void) != 0 || mb.base == 0 {
        return None;
    }
    Some(mb.base as u64)
}

/// 與 cmd 無關僅查詢驅動狀態（無參數）
pub fn get_pid() -> i32 {
    let mut out: i32 = -1;
    call(OP_CMD_GETPID, &mut out as *mut i32 as *mut c_void);
    out
}

pub fn hide_process() -> bool {
    call(OP_CMD_HIDE_PROCESS, std::ptr::null_mut()) == 0
}

pub fn un_hook() -> bool {
    call(OP_CMD_UN_HOOK, std::ptr::null_mut()) == 0
}

pub fn recover_process() -> bool {
    call(OP_CMD_RECOVER_PROCESS, std::ptr::null_mut()) == 0
}

pub fn gyro_xyz(enable: bool, type_mask: u32, x: f32, y: f32, z: f32) -> bool {
    let mut cmd = GyroConfigCmd {
        enable: if enable { 1 } else { 0 },
        type_mask,
        x,
        y,
        z,
    };
    call(OP_CMD_GYRO, &mut cmd as *mut GyroConfigCmd as *mut c_void) == 0
}

pub fn gyro_xy(enable: bool, type_mask: u32, x: f32, y: f32) -> bool {
    gyro_xyz(enable, type_mask, x, y, 0.0)
}

pub fn tracking_update(active: bool, bp_addr: u64, x: f32, y: f32, z: f32) -> bool {
    let mut data = TrackingData {
        is_active: active,
        _pad: [0; 7],
        bp_addr: bp_addr as usize,
        x,
        y,
        z,
        _pad2: 0,
    };
    call(OP_CMD_UPDATE_TRACKING, &mut data as *mut TrackingData as *mut c_void) == 0
}

pub fn brk_tracking_update(active: bool, bp_addr: u64, x: f32, y: f32, z: f32) -> bool {
    let mut data = TrackingData {
        is_active: active,
        _pad: [0; 7],
        bp_addr: bp_addr as usize,
        x,
        y,
        z,
        _pad2: 0,
    };
    call(
        OP_CMD_BRK_UPDATE_TRACKING,
        &mut data as *mut TrackingData as *mut c_void,
    ) == 0
}

pub fn hook_attach(pid: i32, target_addr: u64) -> bool {
    if pid <= 0 || target_addr == 0 {
        return false;
    }
    let mut cmd = HookAttach {
        pid,
        _pad: 0,
        target_addr: target_addr as usize,
    };
    call(OP_CMD_HOOK_ATTACH, &mut cmd as *mut HookAttach as *mut c_void) == 0
}

pub fn hook_detach() -> bool {
    call(OP_CMD_HOOK_DETACH, std::ptr::null_mut()) == 0
}

pub fn hook_set_rot(config: &HookSetRot) -> bool {
    if config.stack_count as usize > STACK_ROT_MAX {
        return false;
    }
    call(
        OP_CMD_HOOK_SET_ROT,
        config as *const HookSetRot as *mut HookSetRot as *mut c_void,
    ) == 0
}

pub fn hook_status() -> Option<HookStatus> {
    let mut status = HookStatus {
        active: 0,
        armed: 0,
        _pad: [0; 2],
        pid: 0,
        vaddr: 0,
        target_addr: 0,
        rot: [0; 3],
        stack_rot: [0; 3],
        stack_count: 0,
        stack_offset: 0,
        hit_count: 0,
        captured_x0: 0,
    };
    if call(
        OP_CMD_HOOK_STATUS,
        &mut status as *mut HookStatus as *mut c_void,
    ) != 0
    {
        return None;
    }
    Some(status)
}

pub fn hook_read_reg(reg_idx: u32) -> Option<(u64, u64)> {
    let mut cmd = HookReadReg {
        reg_idx,
        _pad: 0,
        value: 0,
        hit_count: 0,
    };
    if call(OP_CMD_HOOK_READREG, &mut cmd as *mut HookReadReg as *mut c_void) != 0 {
        return None;
    }
    Some((cmd.value, cmd.hit_count))
}

/// hwbp_add 携帶完整軌跡/寫寄存器參數; 簡單唯讀硬斷可用 BpInfo::new
pub fn hwbp_add(info: &BpInfo) -> bool {
    call(OP_CMD_HWBP_ADD, info as *const BpInfo as *mut BpInfo as *mut c_void) == 0
}

pub fn hwbp_enable(info: &BpInfo) -> bool {
    call(
        OP_CMD_HWBP_ENABLE,
        info as *const BpInfo as *mut BpInfo as *mut c_void,
    ) == 0
}

pub fn hwbp_disable(pid: i32, addr: u64) -> bool {
    let info = BpInfo::new(pid, addr, 0, 0);
    call(
        OP_CMD_HWBP_DISABLE,
        &info as *const BpInfo as *mut BpInfo as *mut c_void,
    ) == 0
}

pub fn hwbp_clear() -> bool {
    call(OP_CMD_HWBP_CLEAR, std::ptr::null_mut()) == 0
}

pub fn brk_add(info: &BpInfo) -> bool {
    call(
        OP_CMD_BRK_ADD,
        info as *const BpInfo as *mut BpInfo as *mut c_void,
    ) == 0
}

pub fn brk_enable(info: &BpInfo) -> bool {
    call(
        OP_CMD_BRK_ENABLE,
        info as *const BpInfo as *mut BpInfo as *mut c_void,
    ) == 0
}

pub fn brk_disable(pid: i32, addr: u64) -> bool {
    let info = BpInfo::new(pid, addr, 0, 0);
    call(
        OP_CMD_BRK_DISABLE,
        &info as *const BpInfo as *mut BpInfo as *mut c_void,
    ) == 0
}

pub fn brk_clear() -> bool {
    call(OP_CMD_BRK_CLEAR, std::ptr::null_mut()) == 0
}

pub fn hwbp_get_hits(pid: i32, addr: u64, out: &mut [HwBpHitItem]) -> Option<u32> {
    if out.is_empty() || out.len() > i32::MAX as usize {
        return None;
    }
    let mut args = HwBpHitArgs {
        pid,
        _pad: 0,
        addr: addr as usize,
        out_buf: out.as_mut_ptr(),
        out_len: out.len() as i32,
        real_count: 0,
    };
    if call(
        OP_CMD_HWBP_GET_HITS,
        &mut args as *mut HwBpHitArgs as *mut c_void,
    ) != 0
    {
        return None;
    }
    Some(args.real_count as u32)
}

pub fn brk_get_hits(pid: i32, addr: u64, out: &mut [BrkBpHitItem]) -> Option<u32> {
    if out.is_empty() || out.len() > i32::MAX as usize {
        return None;
    }
    let mut args = BrkBpHitArgs {
        pid,
        _pad: 0,
        addr: addr as usize,
        out_buf: out.as_mut_ptr(),
        out_len: out.len() as i32,
        real_count: 0,
    };
    if call(
        OP_CMD_BRK_GET_HITS,
        &mut args as *mut BrkBpHitArgs as *mut c_void,
    ) != 0
    {
        return None;
    }
    Some(args.real_count as u32)
}

pub fn touch_init(max_x: i32, max_y: i32) -> bool {
    let mut cmd = TouchInit { max_x, max_y };
    call(OP_CMD_TOUCH_INIT, &mut cmd as *mut TouchInit as *mut c_void) == 0
}

pub fn touch_event(command: u32, x: u32, y: u32, slot_mask: u16) -> bool {
    if command != TOUCH_DOWN && command != TOUCH_MOVE && command != TOUCH_UP {
        return false;
    }
    if slot_mask == 0 || slot_mask & !NB9_TOUCH_MASK != 0 {
        return false;
    }
    let mut event = TouchEvent {
        command,
        x,
        y,
        slot_mask,
        reserved: 0,
    };
    call(
        OP_CMD_TOUCH_EVENT,
        &mut event as *mut TouchEvent as *mut c_void,
    ) == 0
}

pub fn touch_destroy() -> bool {
    call(OP_CMD_TOUCH_DESTROY, std::ptr::null_mut()) == 0
}
