#![allow(non_camel_case_types, dead_code)]

use std::collections::HashMap;
use std::ffi::CStr;
use std::fs;
use std::io;
use std::mem::size_of;
use std::os::raw::{c_char, c_int, c_long, c_ulong, c_void};
use std::os::unix::io::RawFd;
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const SYS_PIDFD_OPEN: c_long = 434;
const SYS_PIDFD_GETFD: c_long = 438;

const DRM_CLIENT_CAP_UNIVERSAL_PLANES: u64 = 2;
const DRM_CLIENT_CAP_ATOMIC: u64 = 3;

const DRM_MODE_OBJECT_CRTC: u32 = 0xcccccccc;
const DRM_MODE_OBJECT_PLANE: u32 = 0xeeeeeeee;

const DRM_MODE_PROP_RANGE: u32 = 1 << 1;
const DRM_MODE_PROP_IMMUTABLE: u32 = 1 << 2;
const DRM_MODE_PROP_ENUM: u32 = 1 << 3;
const DRM_MODE_PROP_BLOB: u32 = 1 << 4;
const DRM_MODE_PROP_BITMASK: u32 = 1 << 5;

const DRM_MODE_ATOMIC_TEST_ONLY: u32 = 0x0100;
const DRM_FORMAT_MOD_LINEAR: u64 = 0;

const DRM_CLOEXEC: u32 = 0x8000_0000;

const DMA_BUF_SYNC_WRITE: u64 = 2;
const DMA_BUF_SYNC_END: u64 = 4;

const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}

const F_ARGB8888: u32 = fourcc(b'A', b'R', b'2', b'4');
const F_XRGB8888: u32 = fourcc(b'X', b'R', b'2', b'4');
const F_ABGR8888: u32 = fourcc(b'A', b'B', b'2', b'4');
const F_XBGR8888: u32 = fourcc(b'X', b'B', b'2', b'4');
const F_RGBA8888: u32 = fourcc(b'R', b'A', b'2', b'4');
const F_BGRA8888: u32 = fourcc(b'B', b'A', b'2', b'4');
const F_RGBX8888: u32 = fourcc(b'R', b'X', b'2', b'4');
const F_BGRX8888: u32 = fourcc(b'B', b'X', b'2', b'4');

const FORMAT_PREF: [u32; 8] = [
    F_ABGR8888, F_ARGB8888, F_XBGR8888, F_XRGB8888, F_RGBA8888, F_BGRA8888, F_RGBX8888, F_BGRX8888,
];

#[repr(C)]
#[derive(Clone, Copy)]
struct drm_mode_modeinfo {
    clock: u32,
    hdisplay: u16,
    hsync_start: u16,
    hsync_end: u16,
    htotal: u16,
    hskew: u16,
    vdisplay: u16,
    vsync_start: u16,
    vsync_end: u16,
    vtotal: u16,
    vscan: u16,
    vrefresh: u32,
    flags: u32,
    type_: u32,
    name: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct drm_mode_crtc {
    set_connectors_ptr: u64,
    count_connectors: u32,
    crtc_id: u32,
    fb_id: u32,
    x: u32,
    y: u32,
    gamma_size: u32,
    mode_valid: u32,
    mode: drm_mode_modeinfo,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct drm_mode_card_res {
    fb_id_ptr: u64,
    crtc_id_ptr: u64,
    connector_id_ptr: u64,
    encoder_id_ptr: u64,
    count_fbs: u32,
    count_crtcs: u32,
    count_connectors: u32,
    count_encoders: u32,
    min_width: u32,
    max_width: u32,
    min_height: u32,
    max_height: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct drm_mode_get_plane_res {
    plane_id_ptr: u64,
    count_planes: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct drm_mode_get_plane {
    plane_id: u32,
    crtc_id: u32,
    fb_id: u32,
    possible_crtcs: u32,
    format_type: u32,
    count_format_types: u32,
    format_type_ptr: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct drm_mode_obj_get_properties {
    props_ptr: u64,
    prop_values_ptr: u64,
    count_props: u32,
    obj_id: u32,
    obj_type: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct drm_mode_property_enum {
    value: u64,
    name: [u8; 32],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct drm_mode_get_property {
    values_ptr: u64,
    enum_blob_ptr: u64,
    prop_id: u32,
    flags: u32,
    name: [u8; 32],
    count_values: u32,
    count_enum_blobs: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct drm_mode_get_blob {
    blob_id: u32,
    length: u32,
    data: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct drm_set_client_cap {
    capability: u64,
    value: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct drm_mode_create_dumb {
    height: u32,
    width: u32,
    bpp: u32,
    flags: u32,
    handle: u32,
    pitch: u32,
    size: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct drm_mode_map_dumb {
    handle: u32,
    pad: u32,
    offset: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct drm_mode_destroy_dumb {
    handle: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct drm_mode_fb_cmd2 {
    fb_id: u32,
    width: u32,
    height: u32,
    pixel_format: u32,
    flags: u32,
    handles: [u32; 4],
    pitches: [u32; 4],
    offsets: [u32; 4],
    modifier: [u64; 4],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct drm_mode_atomic {
    flags: u32,
    count_objs: u32,
    objs_ptr: u64,
    count_props_ptr: u64,
    props_ptr: u64,
    prop_values_ptr: u64,
    reserved: u64,
    user_data: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct drm_prime_handle {
    handle: u32,
    flags: u32,
    fd: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct dma_buf_sync {
    flags: u64,
}

const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;

const fn ioc(dir: u32, ty: u32, nr: u32, size: usize) -> c_ulong {
    ((dir as c_ulong) << 30) | ((size as c_ulong) << 16) | ((ty as c_ulong) << 8) | (nr as c_ulong)
}

const fn iowr<T>(nr: u32) -> c_ulong {
    ioc(IOC_READ | IOC_WRITE, b'd' as u32, nr, size_of::<T>())
}

const fn iow<T>(nr: u32) -> c_ulong {
    ioc(IOC_WRITE, b'd' as u32, nr, size_of::<T>())
}

const DMA_BUF_IOCTL_SYNC: c_ulong = ioc(IOC_WRITE, b'b' as u32, 0, size_of::<dma_buf_sync>());

fn zeroed<T>() -> T {
    unsafe { std::mem::zeroed() }
}

unsafe fn ioctl_mut<T>(fd: RawFd, req: c_ulong, arg: &mut T) -> io::Result<()> {
    let r = libc::ioctl(fd, req as _, arg as *mut T as *mut c_void);
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn e2s(e: &io::Error) -> String {
    format!("{} (errno={})", e, e.raw_os_error().unwrap_or(-1))
}

#[derive(Clone)]
struct Prop {
    id: u32,
    flags: u32,
    value: u64,
    values: Vec<u64>,
}

type PropMap = HashMap<String, Prop>;

struct PropMeta {
    name: String,
    flags: u32,
    values: Vec<u64>,
}

struct Resources {
    crtcs: Vec<u32>,
    max_w: u32,
    max_h: u32,
}

struct PlaneInfo {
    id: u32,
    crtc: u32,
    fb: u32,
    possible: u32,
    formats: Vec<u32>,
}

struct ActiveCrtc {
    id: u32,
    index: usize,
    width: u32,
    height: u32,
}

#[derive(Default)]
struct FmtMod {
    modifier: u64,
    offset: u32,
    formats_mask: u64,
}

#[derive(Default)]
struct InFormats {
    formats: Vec<u32>,
    mods: Vec<FmtMod>,
}

impl InFormats {
    fn linear_ok(&self, f: u32) -> bool {
        if self.formats.is_empty() {
            return true;
        }
        let idx = match self.formats.iter().position(|&x| x == f) {
            Some(i) => i,
            None => return false,
        };
        let mut applicable = false;
        for m in &self.mods {
            let off = m.offset as usize;
            if idx >= off && idx < off + 64 {
                let bit = idx - off;
                if (m.formats_mask >> bit) & 1 == 1 {
                    applicable = true;
                    if m.modifier == DRM_FORMAT_MOD_LINEAR {
                        return true;
                    }
                }
            }
        }
        !applicable
    }
}

fn rd_u32(d: &[u8], off: usize) -> Option<u32> {
    d.get(off..off + 4)
        .map(|s| u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}

fn rd_u64(d: &[u8], off: usize) -> Option<u64> {
    d.get(off..off + 8)
        .map(|s| u64::from_le_bytes([s[0], s[1], s[2], s[3], s[4], s[5], s[6], s[7]]))
}

fn parse_in_formats(d: &[u8]) -> InFormats {
    let mut out = InFormats::default();
    let count_formats = rd_u32(d, 8).unwrap_or(0) as usize;
    let format_offset = rd_u32(d, 12).unwrap_or(0) as usize;
    let count_mods = rd_u32(d, 16).unwrap_or(0) as usize;
    let mod_offset = rd_u32(d, 20).unwrap_or(0) as usize;
    for i in 0..count_formats {
        if let Some(f) = rd_u32(d, format_offset + i * 4) {
            out.formats.push(f);
        }
    }
    for i in 0..count_mods {
        let base = mod_offset + i * 24;
        let formats_mask = match rd_u64(d, base) {
            Some(v) => v,
            None => break,
        };
        let offset = rd_u32(d, base + 8).unwrap_or(0);
        let modifier = match rd_u64(d, base + 16) {
            Some(v) => v,
            None => break,
        };
        out.mods.push(FmtMod {
            modifier,
            offset,
            formats_mask,
        });
    }
    out
}

fn fourcc_name(f: u32) -> String {
    let name = match f {
        F_ARGB8888 => "ARGB8888".to_string(),
        F_XRGB8888 => "XRGB8888".to_string(),
        F_ABGR8888 => "ABGR8888".to_string(),
        F_XBGR8888 => "XBGR8888".to_string(),
        F_RGBA8888 => "RGBA8888".to_string(),
        F_BGRA8888 => "BGRA8888".to_string(),
        F_RGBX8888 => "RGBX8888".to_string(),
        F_BGRX8888 => "BGRX8888".to_string(),
        _ => {
            let b = f.to_le_bytes();
            let s: String = b
                .iter()
                .map(|&c| {
                    if (0x20..0x7f).contains(&c) {
                        c as char
                    } else {
                        '?'
                    }
                })
                .collect();
            format!("fourcc({})", s)
        }
    };
    format!("{} [0x{:08x}]", name, f)
}

fn mem_layout(f: u32) -> Option<[usize; 4]> {
    Some(match f {
        F_ABGR8888 => [3, 2, 1, 0],
        F_ARGB8888 => [1, 2, 3, 0],
        F_RGBA8888 => [0, 1, 2, 3],
        F_BGRA8888 => [2, 1, 0, 3],
        F_XBGR8888 => [3, 2, 1, 0],
        F_XRGB8888 => [1, 2, 3, 0],
        F_RGBX8888 => [0, 1, 2, 3],
        F_BGRX8888 => [2, 1, 0, 3],
        _ => return None,
    })
}

fn fmt_has_alpha(f: u32) -> bool {
    matches!(f, F_ABGR8888 | F_ARGB8888 | F_RGBA8888 | F_BGRA8888)
}

fn fill(buf: &mut [u8], pitch: usize, w: usize, h: usize, f: u32, color: [u8; 4]) {
    let m = match mem_layout(f) {
        Some(m) => m,
        None => return,
    };
    let mut px = [0u8; 4];
    px[m[0]] = color[0];
    px[m[1]] = color[1];
    px[m[2]] = color[2];
    px[m[3]] = if fmt_has_alpha(f) { color[3] } else { 0xff };
    for y in 0..h {
        let start = y * pitch;
        if start + w * 4 > buf.len() {
            break;
        }
        let row = &mut buf[start..start + w * 4];
        for c in row.chunks_exact_mut(4) {
            c.copy_from_slice(&px);
        }
    }
}

fn get_property(fd: RawFd, id: u32) -> io::Result<PropMeta> {
    let mut p = zeroed::<drm_mode_get_property>();
    p.prop_id = id;
    unsafe { ioctl_mut(fd, iowr::<drm_mode_get_property>(0xaa), &mut p)? };
    let nv = p.count_values as usize;
    let nb = p.count_enum_blobs as usize;
    let mut vals = vec![0u64; nv];
    let mut enums = vec![
        drm_mode_property_enum {
            value: 0,
            name: [0u8; 32],
        };
        nb
    ];
    if nv > 0 || nb > 0 {
        p.values_ptr = if nv > 0 { vals.as_mut_ptr() as u64 } else { 0 };
        p.enum_blob_ptr = if nb > 0 { enums.as_mut_ptr() as u64 } else { 0 };
        unsafe { ioctl_mut(fd, iowr::<drm_mode_get_property>(0xaa), &mut p)? };
        vals.truncate(p.count_values as usize);
    }
    let name = unsafe { CStr::from_ptr(p.name.as_ptr() as *const c_char) }
        .to_string_lossy()
        .into_owned();
    Ok(PropMeta {
        name,
        flags: p.flags,
        values: vals,
    })
}

fn get_properties(fd: RawFd, obj_id: u32, obj_type: u32) -> io::Result<PropMap> {
    let mut a = zeroed::<drm_mode_obj_get_properties>();
    a.obj_id = obj_id;
    a.obj_type = obj_type;
    unsafe { ioctl_mut(fd, iowr::<drm_mode_obj_get_properties>(0xb9), &mut a)? };
    let n = a.count_props as usize;
    let mut ids = vec![0u32; n];
    let mut vals = vec![0u64; n];
    if n > 0 {
        a.props_ptr = ids.as_mut_ptr() as u64;
        a.prop_values_ptr = vals.as_mut_ptr() as u64;
        a.count_props = n as u32;
        unsafe { ioctl_mut(fd, iowr::<drm_mode_obj_get_properties>(0xb9), &mut a)? };
        ids.truncate(a.count_props as usize);
        vals.truncate(a.count_props as usize);
    }
    let mut map = PropMap::new();
    for (id, val) in ids.into_iter().zip(vals) {
        let meta = get_property(fd, id)?;
        map.insert(
            meta.name,
            Prop {
                id,
                flags: meta.flags,
                value: val,
                values: meta.values,
            },
        );
    }
    Ok(map)
}

fn get_blob(fd: RawFd, blob_id: u32) -> io::Result<Vec<u8>> {
    let mut b = zeroed::<drm_mode_get_blob>();
    b.blob_id = blob_id;
    unsafe { ioctl_mut(fd, iowr::<drm_mode_get_blob>(0xac), &mut b)? };
    let mut data = vec![0u8; b.length as usize];
    if !data.is_empty() {
        b.data = data.as_mut_ptr() as u64;
        unsafe { ioctl_mut(fd, iowr::<drm_mode_get_blob>(0xac), &mut b)? };
        data.truncate(b.length as usize);
    }
    Ok(data)
}

fn set_client_cap(fd: RawFd, cap: u64, value: u64) -> io::Result<()> {
    let mut c = drm_set_client_cap {
        capability: cap,
        value,
    };
    unsafe { ioctl_mut(fd, iow::<drm_set_client_cap>(0x0d), &mut c) }
}

fn read_resources(fd: RawFd) -> io::Result<Resources> {
    let mut r = zeroed::<drm_mode_card_res>();
    unsafe { ioctl_mut(fd, iowr::<drm_mode_card_res>(0xa0), &mut r)? };
    let mut crtcs = vec![0u32; r.count_crtcs as usize];
    if !crtcs.is_empty() {
        r.crtc_id_ptr = crtcs.as_mut_ptr() as u64;
        r.count_crtcs = crtcs.len() as u32;
        r.count_fbs = 0;
        r.fb_id_ptr = 0;
        r.count_connectors = 0;
        r.connector_id_ptr = 0;
        r.count_encoders = 0;
        r.encoder_id_ptr = 0;
        unsafe { ioctl_mut(fd, iowr::<drm_mode_card_res>(0xa0), &mut r)? };
        crtcs.truncate(r.count_crtcs as usize);
    }
    Ok(Resources {
        crtcs,
        max_w: r.max_width,
        max_h: r.max_height,
    })
}

fn get_plane_resources(fd: RawFd) -> io::Result<Vec<u32>> {
    let mut r = zeroed::<drm_mode_get_plane_res>();
    unsafe { ioctl_mut(fd, iowr::<drm_mode_get_plane_res>(0xb5), &mut r)? };
    let mut ids = vec![0u32; r.count_planes as usize];
    if ids.is_empty() {
        return Ok(ids);
    }
    r.plane_id_ptr = ids.as_mut_ptr() as u64;
    r.count_planes = ids.len() as u32;
    unsafe { ioctl_mut(fd, iowr::<drm_mode_get_plane_res>(0xb5), &mut r)? };
    ids.truncate(r.count_planes as usize);
    Ok(ids)
}

fn get_plane(fd: RawFd, id: u32) -> io::Result<PlaneInfo> {
    let mut p = zeroed::<drm_mode_get_plane>();
    p.plane_id = id;
    unsafe { ioctl_mut(fd, iowr::<drm_mode_get_plane>(0xb6), &mut p)? };
    let mut formats = vec![0u32; p.count_format_types as usize];
    if !formats.is_empty() {
        p.format_type_ptr = formats.as_mut_ptr() as u64;
        p.count_format_types = formats.len() as u32;
        unsafe { ioctl_mut(fd, iowr::<drm_mode_get_plane>(0xb6), &mut p)? };
        formats.truncate(p.count_format_types as usize);
    }
    Ok(PlaneInfo {
        id,
        crtc: p.crtc_id,
        fb: p.fb_id,
        possible: p.possible_crtcs,
        formats,
    })
}

fn get_crtc_dims(fd: RawFd, id: u32) -> io::Result<(u32, u32, u32)> {
    let mut c = zeroed::<drm_mode_crtc>();
    c.crtc_id = id;
    unsafe { ioctl_mut(fd, iowr::<drm_mode_crtc>(0xa1), &mut c)? };
    Ok((c.mode.hdisplay as u32, c.mode.vdisplay as u32, c.mode_valid))
}

fn find_active_crtc(fd: RawFd, crtcs: &[u32]) -> io::Result<Option<ActiveCrtc>> {
    for (idx, &id) in crtcs.iter().enumerate() {
        let props = match get_properties(fd, id, DRM_MODE_OBJECT_CRTC) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let active = props.get("ACTIVE").map(|p| p.value).unwrap_or(0);
        if active == 0 {
            continue;
        }
        if let Ok((w, h, valid)) = get_crtc_dims(fd, id) {
            if valid != 0 && w > 0 && h > 0 {
                return Ok(Some(ActiveCrtc {
                    id,
                    index: idx,
                    width: w,
                    height: h,
                }));
            }
        }
    }
    Ok(None)
}

fn open_card(card: &str) -> io::Result<RawFd> {
    let c = std::ffi::CString::new(card)
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "bad card path"))?;
    let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(fd)
    }
}

fn pidfd_open(pid: i32) -> io::Result<RawFd> {
    let r = unsafe { libc::syscall(SYS_PIDFD_OPEN, pid as c_long, 0 as c_long) };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r as RawFd)
    }
}

fn pidfd_getfd(pfd: RawFd, target: RawFd) -> io::Result<RawFd> {
    let r = unsafe {
        libc::syscall(
            SYS_PIDFD_GETFD,
            pfd as c_long,
            target as c_long,
            0 as c_long,
        )
    };
    if r < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(r as RawFd)
    }
}

fn find_card_fds(card: &str) -> Vec<(i32, i32, String)> {
    let mut out = Vec::new();
    let self_pid = std::process::id() as i32;
    let entries = match fs::read_dir("/proc") {
        Ok(e) => e,
        Err(_) => return out,
    };
    for e in entries.flatten() {
        let name = e.file_name();
        let pid: i32 = match name.to_string_lossy().parse() {
            Ok(p) => p,
            Err(_) => continue,
        };
        if pid == self_pid {
            continue;
        }
        let fds = match fs::read_dir(format!("/proc/{}/fd", pid)) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let comm = fs::read_to_string(format!("/proc/{}/comm", pid))
            .unwrap_or_default()
            .trim()
            .to_string();
        for f in fds.flatten() {
            let fdname = f.file_name().to_string_lossy().into_owned();
            let fdnum: i32 = match fdname.parse() {
                Ok(n) => n,
                Err(_) => continue,
            };
            if let Ok(target) = fs::read_link(f.path()) {
                let t = target.to_string_lossy();
                if t == card || t.starts_with(&format!("{} ", card)) {
                    out.push((pid, fdnum, comm.clone()));
                    break;
                }
            }
        }
    }
    out.sort();
    out
}

fn acquire_drm_fd(cfg: &Config) -> io::Result<(RawFd, i32, String, i32)> {
    let mut cands: Vec<(i32, i32, String)> = Vec::new();
    if let Some(pid) = cfg.pid {
        if let Ok(fds) = fs::read_dir(format!("/proc/{}/fd", pid)) {
            let comm = fs::read_to_string(format!("/proc/{}/comm", pid))
                .unwrap_or_default()
                .trim()
                .to_string();
            for f in fds.flatten() {
                let fdname = f.file_name().to_string_lossy().into_owned();
                let fdnum: i32 = match fdname.parse() {
                    Ok(n) => n,
                    Err(_) => continue,
                };
                if let Ok(t) = fs::read_link(f.path()) {
                    let ts = t.to_string_lossy();
                    if ts == cfg.card || ts.starts_with(&format!("{} ", cfg.card)) {
                        cands.push((pid, fdnum, comm.clone()));
                    }
                }
            }
        }
    } else {
        cands = find_card_fds(&cfg.card);
    }
    cands.sort();
    cands.dedup_by(|a, b| a.0 == b.0 && a.1 == b.1);
    if cands.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!("no process currently has {} open", cfg.card),
        ));
    }
    let mut last_err: Option<io::Error> = None;
    for (pid, tfd, comm) in cands {
        println!("candidate: pid={} comm={} fd={}", pid, comm, tfd);
        let pfd = match pidfd_open(pid) {
            Ok(x) => x,
            Err(e) => {
                println!("  pidfd_open: {}", e2s(&e));
                last_err = Some(e);
                continue;
            }
        };
        let dup = pidfd_getfd(pfd, tfd);
        unsafe {
            libc::close(pfd);
        }
        match dup {
            Ok(fd) => match read_resources(fd) {
                Ok(r) if !r.crtcs.is_empty() => return Ok((fd, pid, comm, tfd)),
                Ok(_) => {
                    println!("  duplicated fd has no CRTCs");
                    unsafe { libc::close(fd) };
                }
                Err(e) => {
                    println!("  duplicated fd GETRESOURCES: {}", e2s(&e));
                    unsafe { libc::close(fd) };
                }
            },
            Err(e) => {
                println!("  pidfd_getfd: {}", e2s(&e));
                last_err = Some(e);
            }
        }
    }
    Err(last_err.unwrap_or_else(|| io::Error::other("no usable composer DRM fd")))
}

fn select_plane(infos: &[PlaneInfo], active_idx: usize, want: Option<u32>) -> Option<u32> {
    let usable = |p: &PlaneInfo| {
        p.fb == 0
            && p.crtc == 0
            && active_idx < 32
            && (p.possible & (1u32 << active_idx)) != 0
            && (p.formats.is_empty() || p.formats.iter().any(|&f| mem_layout(f).is_some()))
    };
    if let Some(w) = want {
        return infos.iter().find(|p| p.id == w).map(|p| p.id);
    }
    for id in [120u32, 210, 282, 264, 246, 228, 156, 138, 102, 84] {
        if let Some(p) = infos.iter().find(|p| p.id == id && usable(p)) {
            return Some(p.id);
        }
    }
    infos
        .iter()
        .filter(|p| usable(p))
        .max_by_key(|p| p.id)
        .map(|p| p.id)
}

fn pick_blend(props: &PropMap) -> (Option<(u32, u64)>, String) {
    for n in ["pixel blend mode", "blend mode"] {
        if let Some(p) = props.get(n) {
            let is_enum = p.flags & (DRM_MODE_PROP_ENUM | DRM_MODE_PROP_BITMASK) != 0;
            let has = |v: u64| p.values.contains(&v);
            if is_enum && has(1) {
                return (
                    Some((p.id, 1)),
                    format!("{}: Pre-multiplied (value 1) selected", n),
                );
            }
            if is_enum && has(2) {
                return (
                    Some((p.id, 2)),
                    format!(
                        "{}: only Coverage (2) available; fb must NOT be premultiplied",
                        n
                    ),
                );
            }
            if is_enum && has(0) {
                return (
                    Some((p.id, 0)),
                    format!("{}: only None (0) available; fb alpha will be ignored", n),
                );
            }
            return (
                None,
                format!("{} present but values {:?} unusable", n, p.values),
            );
        }
    }
    (
        None,
        "no pixel blend mode property; alpha handling unverified".to_string(),
    )
}

fn pick_zpos(props: &PropMap, want: Option<u64>) -> (Option<(u32, u64)>, String) {
    if let Some(p) = props.get("zpos") {
        if p.flags & DRM_MODE_PROP_IMMUTABLE != 0 {
            (None, format!("zpos is immutable, value={}", p.value))
        } else {
            let v = want.unwrap_or_else(|| {
                if p.values.len() >= 2 {
                    p.values[1]
                } else {
                    p.value
                }
            });
            (
                Some((p.id, v)),
                format!("zpos range={:?}, setting {}", p.values, v),
            )
        }
    } else {
        (None, "no zpos property".to_string())
    }
}

fn choose_format(formats: &[u32], in_fmts: &InFormats) -> Option<u32> {
    if formats.is_empty() {
        return Some(F_ABGR8888);
    }
    for f in FORMAT_PREF {
        if formats.contains(&f) && mem_layout(f).is_some() && in_fmts.linear_ok(f) {
            return Some(f);
        }
    }
    for f in FORMAT_PREF {
        if formats.contains(&f) && mem_layout(f).is_some() {
            return Some(f);
        }
    }
    formats.iter().copied().find(|f| mem_layout(*f).is_some())
}

fn parse_fourcc_arg(s: &str) -> Option<u32> {
    let up = s.to_ascii_uppercase();
    let f = match up.as_str() {
        "ARGB8888" => F_ARGB8888,
        "XRGB8888" => F_XRGB8888,
        "ABGR8888" => F_ABGR8888,
        "XBGR8888" => F_XBGR8888,
        "RGBA8888" => F_RGBA8888,
        "BGRA8888" => F_BGRA8888,
        "RGBX8888" => F_RGBX8888,
        "BGRX8888" => F_BGRX8888,
        _ => {
            if s.len() == 4 {
                let b = s.as_bytes();
                fourcc(b[0], b[1], b[2], b[3])
            } else {
                return None;
            }
        }
    };
    Some(f)
}

struct Config {
    card: String,
    pid: Option<i32>,
    plane: Option<u32>,
    format: Option<u32>,
    hold: u64,
    w: u32,
    h: u32,
    x: Option<u32>,
    y: Option<u32>,
    zpos: Option<u64>,
    dry_run: bool,
    direct: bool,
    help: bool,
}

fn parse_args() -> Config {
    let mut cfg = Config {
        card: "/dev/dri/card0".to_string(),
        pid: None,
        plane: None,
        format: None,
        hold: 60,
        w: 240,
        h: 240,
        x: None,
        y: None,
        zpos: None,
        dry_run: false,
        direct: false,
        help: false,
    };
    let mut it = std::env::args().skip(1);
    while let Some(a) = it.next() {
        match a.as_str() {
            "--card" => cfg.card = it.next().unwrap_or_default(),
            "--pid" => cfg.pid = it.next().and_then(|v| v.parse().ok()),
            "--plane" => cfg.plane = it.next().and_then(|v| v.parse().ok()),
            "--format" => cfg.format = it.next().and_then(|v| parse_fourcc_arg(&v)),
            "--hold" => cfg.hold = it.next().and_then(|v| v.parse().ok()).unwrap_or(60),
            "--w" => cfg.w = it.next().and_then(|v| v.parse().ok()).unwrap_or(240),
            "--h" => cfg.h = it.next().and_then(|v| v.parse().ok()).unwrap_or(240),
            "--x" => cfg.x = it.next().and_then(|v| v.parse().ok()),
            "--y" => cfg.y = it.next().and_then(|v| v.parse().ok()),
            "--zpos" => cfg.zpos = it.next().and_then(|v| v.parse().ok()),
            "--dry-run" => cfg.dry_run = true,
            "--direct" => cfg.direct = true,
            "-h" | "--help" => cfg.help = true,
            other => {
                eprintln!("unknown argument: {}", other);
                cfg.help = true;
            }
        }
    }
    cfg
}

fn usage() {
    println!("ovl-probe - DRM overlay plane probe (steps 1-3)");
    println!("usage: ovl-probe [options]");
    println!("  --card PATH    DRM device (default /dev/dri/card0)");
    println!("  --pid N        composer pid (default: scan /proc for the card)");
    println!("  --plane N      target plane (default: prefer 15, then 12, 10)");
    println!("  --format FMT   ABGR8888|ARGB8888|XBGR8888|XRGB8888|RGBA8888|BGRA8888|...");
    println!("  --w N --h N    block size (default 240x240)");
    println!("  --x N --y N    block position (default centered / one third down)");
    println!("  --zpos N       override zpos (default: max; lower = below HWC top layer)");
    println!("  --hold SECS    keep overlay on screen (default 60, 0 = until signal)");
    println!("  --dry-run      verify fd + plane capabilities, do not commit");
    println!("  --direct       open the card instead of pidfd_getfd (non-master, planes only)");
    println!("  -h, --help     this help");
}

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_signal(_: c_int) {
    STOP.store(true, Ordering::SeqCst);
}

fn atomic_commit(fd: RawFd, plane: u32, pv: &[(u32, u64)], test_only: bool) -> io::Result<()> {
    let objs = [plane];
    let counts = [pv.len() as u32];
    let ids: Vec<u32> = pv.iter().map(|x| x.0).collect();
    let vals: Vec<u64> = pv.iter().map(|x| x.1).collect();
    let mut a = zeroed::<drm_mode_atomic>();
    a.flags = if test_only {
        DRM_MODE_ATOMIC_TEST_ONLY
    } else {
        0
    };
    a.count_objs = 1;
    a.objs_ptr = objs.as_ptr() as u64;
    a.count_props_ptr = counts.as_ptr() as u64;
    a.props_ptr = ids.as_ptr() as u64;
    a.prop_values_ptr = vals.as_ptr() as u64;
    unsafe { ioctl_mut(fd, iowr::<drm_mode_atomic>(0xbc), &mut a) }
}

fn export_dmabuf(fd: RawFd, handle: u32) -> io::Result<RawFd> {
    let mut p = drm_prime_handle {
        handle,
        flags: DRM_CLOEXEC,
        fd: -1,
    };
    unsafe { ioctl_mut(fd, iowr::<drm_prime_handle>(0x2d), &mut p)? };
    if p.fd < 0 {
        return Err(io::Error::other("PRIME_HANDLE_TO_FD returned no fd"));
    }
    Ok(p.fd)
}

fn dmabuf_sync(fd: RawFd, flags: u64) {
    let mut s = dma_buf_sync { flags };
    unsafe {
        libc::ioctl(
            fd,
            DMA_BUF_IOCTL_SYNC as _,
            &mut s as *mut dma_buf_sync as *mut c_void,
        );
    }
}

struct Overlay {
    fd: RawFd,
    plane: u32,
    crtc: u32,
    fb: u32,
    handle: u32,
    pitch: u32,
    map: *mut u8,
    map_len: usize,
    dmabuf: Option<RawFd>,
    props: PropMap,
    extra: Vec<(u32, u64)>,
    src_w: u32,
    src_h: u32,
    dst_x: u32,
    dst_y: u32,
    dst_w: u32,
    dst_h: u32,
}

impl Overlay {
    fn build_props(&self) -> Result<Vec<(u32, u64)>, String> {
        let need = |n: &str| -> Result<u32, String> {
            self.props
                .get(n)
                .map(|p| p.id)
                .ok_or_else(|| format!("plane is missing required property: {}", n))
        };
        let mut pv = Vec::with_capacity(20);
        pv.push((need("CRTC_ID")?, self.crtc as u64));
        pv.push((need("FB_ID")?, self.fb as u64));
        pv.push((need("SRC_X")?, 0));
        pv.push((need("SRC_Y")?, 0));
        pv.push((need("SRC_W")?, (self.src_w as u64) << 16));
        pv.push((need("SRC_H")?, (self.src_h as u64) << 16));
        pv.push((need("CRTC_X")?, self.dst_x as u64));
        pv.push((need("CRTC_Y")?, self.dst_y as u64));
        pv.push((need("CRTC_W")?, self.dst_w as u64));
        pv.push((need("CRTC_H")?, self.dst_h as u64));
        pv.extend(self.extra.iter().copied());
        Ok(pv)
    }

    fn reassert(&self) -> io::Result<()> {
        let pv = self.build_props().map_err(io::Error::other)?;
        atomic_commit(self.fd, self.plane, &pv, false)
    }

    #[allow(clippy::too_many_arguments)]
    fn create(
        fd: RawFd,
        plane: u32,
        crtc: u32,
        format: u32,
        w: u32,
        h: u32,
        x: u32,
        y: u32,
        props: PropMap,
        extra: Vec<(u32, u64)>,
    ) -> Result<Overlay, String> {
        let mut ov = Overlay {
            fd,
            plane,
            crtc,
            fb: 0,
            handle: 0,
            pitch: 0,
            map: ptr::null_mut(),
            map_len: 0,
            dmabuf: None,
            props,
            extra,
            src_w: w,
            src_h: h,
            dst_x: x,
            dst_y: y,
            dst_w: w,
            dst_h: h,
        };

        let mut d = zeroed::<drm_mode_create_dumb>();
        d.width = w;
        d.height = h;
        d.bpp = 32;
        unsafe { ioctl_mut(fd, iowr::<drm_mode_create_dumb>(0xb2), &mut d) }
            .map_err(|e| format!("CREATE_DUMB {}x{}: {}", w, h, e2s(&e)))?;
        ov.handle = d.handle;
        ov.pitch = d.pitch;
        let size = if d.size == 0 {
            (d.pitch as u64).saturating_mul(d.height as u64)
        } else {
            d.size
        };

        let mut m = zeroed::<drm_mode_map_dumb>();
        m.handle = d.handle;
        unsafe { ioctl_mut(fd, iowr::<drm_mode_map_dumb>(0xb3), &mut m) }
            .map_err(|e| format!("MAP_DUMB: {}", e2s(&e)))?;
        let p = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size as usize,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                fd,
                m.offset as libc::off_t,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(format!(
                "mmap dumb buffer (size={}): {}",
                size,
                io::Error::last_os_error()
            ));
        }
        ov.map = p as *mut u8;
        ov.map_len = size as usize;

        if let Ok(dm) = export_dmabuf(fd, ov.handle) {
            ov.dmabuf = Some(dm);
            dmabuf_sync(dm, DMA_BUF_SYNC_WRITE);
        }

        {
            let buf = unsafe { std::slice::from_raw_parts_mut(ov.map, ov.map_len) };
            fill(
                buf,
                ov.pitch as usize,
                w as usize,
                h as usize,
                format,
                [128, 0, 128, 128],
            );
        }

        if let Some(dm) = ov.dmabuf {
            dmabuf_sync(dm, DMA_BUF_SYNC_END | DMA_BUF_SYNC_WRITE);
        }

        let mut fb = zeroed::<drm_mode_fb_cmd2>();
        fb.width = w;
        fb.height = h;
        fb.pixel_format = format;
        fb.flags = 0;
        fb.handles[0] = ov.handle;
        fb.pitches[0] = ov.pitch;
        fb.offsets[0] = 0;
        unsafe { ioctl_mut(fd, iowr::<drm_mode_fb_cmd2>(0xb8), &mut fb) }.map_err(|e| {
            format!(
                "ADDFB2 {} pitch={} {}x{}: {}",
                fourcc_name(format),
                ov.pitch,
                w,
                h,
                e2s(&e)
            )
        })?;
        ov.fb = fb.fb_id;

        let pv = ov.build_props()?;
        atomic_commit(fd, plane, &pv, true).map_err(|e| {
            format!(
                "TEST_ONLY atomic commit rejected (plane/format/blend/zpos constraint): {}",
                e2s(&e)
            )
        })?;
        atomic_commit(fd, plane, &pv, false)
            .map_err(|e| format!("atomic commit failed: {}", e2s(&e)))?;

        Ok(ov)
    }

    fn disable(&mut self) {
        if self.fb == 0 {
            return;
        }
        let mut pv = Vec::new();
        if let Some(p) = self.props.get("CRTC_ID") {
            pv.push((p.id, 0u64));
        }
        if let Some(p) = self.props.get("FB_ID") {
            pv.push((p.id, 0u64));
        }
        if !pv.is_empty() {
            let _ = atomic_commit(self.fd, self.plane, &pv, false);
        }
    }
}

impl Drop for Overlay {
    fn drop(&mut self) {
        self.disable();
        if self.fb != 0 {
            let mut id = self.fb;
            unsafe {
                libc::ioctl(
                    self.fd,
                    iowr::<u32>(0xaf) as _,
                    &mut id as *mut u32 as *mut c_void,
                );
            }
            self.fb = 0;
        }
        if self.handle != 0 {
            let mut dd = zeroed::<drm_mode_destroy_dumb>();
            dd.handle = self.handle;
            unsafe {
                let _ = ioctl_mut(self.fd, iowr::<drm_mode_destroy_dumb>(0xb4), &mut dd);
            }
        }
        if !self.map.is_null() {
            unsafe {
                libc::munmap(self.map as *mut c_void, self.map_len);
            }
            self.map = ptr::null_mut();
        }
        if let Some(dm) = self.dmabuf.take() {
            unsafe {
                libc::close(dm);
            }
        }
    }
}

fn print_err_hint(e: &io::Error) {
    match e.raw_os_error() {
        Some(1) | Some(13) => {
            println!("hint: EPERM/EACCES. SELinux or Yama is blocking pidfd_getfd.");
            println!("      check: ls -Z /dev/dri/card0; dmesg | tail");
            println!("      test: setenforce 0 (temporary), then rerun.");
            println!("      Magisk su may need to run in a domain allowed to ptrace the composer.");
        }
        Some(3) => println!("hint: ESRCH. composer pid exited; rerun."),
        Some(9) => println!("hint: EBADF. composer closed that fd; rerun."),
        Some(11) => println!("hint: EAGAIN. retry."),
        _ => {}
    }
    println!("hint: you can retry with --direct (open the card, non-master plane commit).");
}

fn run(fd: RawFd, cfg: &Config) -> Result<(), String> {
    let res = read_resources(fd).map_err(|e| format!("GETRESOURCES: {}", e2s(&e)))?;
    println!(
        "resources: crtcs={:?} max={}x{}",
        res.crtcs, res.max_w, res.max_h
    );

    let mut planes = get_plane_resources(fd).unwrap_or_default();
    if planes.is_empty() {
        let _ = set_client_cap(fd, DRM_CLIENT_CAP_UNIVERSAL_PLANES, 1);
        let _ = set_client_cap(fd, DRM_CLIENT_CAP_ATOMIC, 1);
        planes = get_plane_resources(fd).map_err(|e| format!("GETPLANERESOURCES: {}", e2s(&e)))?;
    }
    if planes.is_empty() {
        return Err("driver returned no planes (UNIVERSAL_PLANES unavailable?)".to_string());
    }
    if let Err(e) = set_client_cap(fd, DRM_CLIENT_CAP_ATOMIC, 1) {
        println!("warning: SET_CLIENT_CAP(ATOMIC): {}", e2s(&e));
    }
    planes.sort_unstable();
    planes.dedup();

    let mut infos = Vec::new();
    for &id in &planes {
        match get_plane(fd, id) {
            Ok(p) => infos.push(p),
            Err(e) => println!("plane {:>2}: GETPLANE failed: {}", id, e2s(&e)),
        }
    }
    println!("planes:");
    for p in &infos {
        let fmts: Vec<String> = p.formats.iter().take(8).map(|&f| fourcc_name(f)).collect();
        let mut extra = String::new();
        if let Ok(pp) = get_properties(fd, p.id, DRM_MODE_OBJECT_PLANE) {
            if let Some(z) = pp.get("zpos") {
                extra.push_str(&format!(" zpos={}", z.value));
            }
            if let Some(t) = pp.get("type") {
                extra.push_str(&format!(" type={}", t.value));
            }
        }
        println!(
            "  plane {:>2}: crtc={:<3} fb={:<3} possible=0x{:08x}{} formats[{}{}]",
            p.id,
            p.crtc,
            p.fb,
            p.possible,
            extra,
            fmts.join(", "),
            if p.formats.len() > 8 { ", ..." } else { "" }
        );
    }

    let active = match find_active_crtc(fd, &res.crtcs) {
        Ok(Some(a)) => a,
        _ => {
            let mut fb_crtc = None;
            for p in &infos {
                if p.crtc != 0 {
                    if let Some(idx) = res.crtcs.iter().position(|&c| c == p.crtc) {
                        if let Ok((w, h, valid)) = get_crtc_dims(fd, p.crtc) {
                            if valid != 0 && w > 0 && h > 0 {
                                fb_crtc = Some(ActiveCrtc {
                                    id: p.crtc,
                                    index: idx,
                                    width: w,
                                    height: h,
                                });
                                break;
                            }
                        }
                    }
                }
            }
            match fb_crtc {
                Some(a) => a,
                None => {
                    return Err("no active CRTC; turn the screen on and retry".to_string());
                }
            }
        }
    };
    println!(
        "active crtc: id={} index={} mode={}x{}",
        active.id, active.index, active.width, active.height
    );

    let chosen = match select_plane(&infos, active.index, cfg.plane) {
        Some(x) => x,
        None => {
            return Err(format!(
                "no idle plane usable on crtc {}; HWC may hold all planes (try --plane or --dry-run later)",
                active.id
            ));
        }
    };
    let pinfo = infos.iter().find(|p| p.id == chosen).unwrap();
    if pinfo.fb != 0 || pinfo.crtc != 0 {
        println!(
            "warning: plane {} is currently in use (fb={} crtc={}), the commit may disturb HWC",
            chosen, pinfo.fb, pinfo.crtc
        );
    }
    if chosen == 8 {
        println!("warning: plane 8 is in reserved_win_mask=0x100 on this platform");
    }
    if active.index < 32 && (pinfo.possible & (1u32 << active.index)) == 0 {
        println!(
            "warning: plane {} possible_crtcs=0x{:08x} excludes crtc index {}",
            chosen, pinfo.possible, active.index
        );
    }
    println!("selected plane: {}", chosen);

    let props = get_properties(fd, chosen, DRM_MODE_OBJECT_PLANE)
        .map_err(|e| format!("plane {} properties: {}", chosen, e2s(&e)))?;

    let (blend, blend_msg) = pick_blend(&props);
    let (zpos, zpos_msg) = pick_zpos(&props, cfg.zpos);
    let alpha = props
        .get("alpha")
        .filter(|p| p.flags & DRM_MODE_PROP_IMMUTABLE == 0)
        .map(|p| p.id);
    println!("blend: {}", blend_msg);
    println!("zpos : {}", zpos_msg);
    println!(
        "alpha: {}",
        if alpha.is_some() {
            "writable, setting 0xffff"
        } else {
            "absent or immutable"
        }
    );

    let mut in_fmts = InFormats::default();
    if let Some(p) = props.get("IN_FORMATS") {
        if p.flags & DRM_MODE_PROP_BLOB != 0 {
            match get_blob(fd, p.value as u32) {
                Ok(data) => {
                    in_fmts = parse_in_formats(&data);
                    println!(
                        "IN_FORMATS: {} formats, {} modifier entries",
                        in_fmts.formats.len(),
                        in_fmts.mods.len()
                    );
                    println!(
                        "  formats: {}",
                        in_fmts
                            .formats
                            .iter()
                            .map(|&f| fourcc_name(f))
                            .collect::<Vec<_>>()
                            .join(", ")
                    );
                    for (i, m) in in_fmts.mods.iter().enumerate() {
                        println!(
                            "  mod[{}]: mask=0x{:016x} offset={} modifier=0x{:016x}",
                            i, m.formats_mask, m.offset, m.modifier
                        );
                    }
                }
                Err(e) => println!("warning: GETPROPBLOB IN_FORMATS: {}", e2s(&e)),
            }
        }
    }

    let formats: Vec<u32> = if !pinfo.formats.is_empty() {
        pinfo.formats.clone()
    } else {
        in_fmts.formats.clone()
    };
    if formats.is_empty() {
        println!("warning: plane advertises no format list; falling back to ABGR8888");
    }
    let format = match cfg.format {
        Some(f) => f,
        None => choose_format(&formats, &in_fmts)
            .ok_or_else(|| "no 32bpp RGBA format usable on this plane".to_string())?,
    };
    let layout = mem_layout(format)
        .ok_or_else(|| format!("format {} not supported by this probe", fourcc_name(format)))?;
    let fmt_ok = formats.is_empty() || formats.contains(&format);
    let linear = in_fmts.linear_ok(format);
    println!(
        "format: {} (memory byte map R={} G={} B={} A={}) supported={} linear={}",
        fourcc_name(format),
        layout[0],
        layout[1],
        layout[2],
        layout[3],
        fmt_ok,
        linear
    );
    if !linear {
        println!("warning: IN_FORMATS does not advertise LINEAR for this format; ADDFB2 may fail");
    }
    let premult = matches!(blend, Some((_, 1)));
    let step2 = fmt_ok && premult;
    println!(
        "[STEP2] {}: format_ok={} premultiplied_blend={} explicit_zpos={}",
        if step2 { "PASS" } else { "WARN" },
        fmt_ok,
        premult,
        zpos.is_some()
    );

    if cfg.dry_run {
        println!("dry-run: skipping dumb buffer + atomic commit");
        return Ok(());
    }

    if cfg.w == 0 || cfg.h == 0 || cfg.w > active.width || cfg.h > active.height {
        return Err(format!(
            "bad block size {}x{} for mode {}x{}",
            cfg.w, cfg.h, active.width, active.height
        ));
    }
    let x = cfg
        .x
        .unwrap_or_else(|| active.width.saturating_sub(cfg.w) / 2)
        .min(active.width.saturating_sub(cfg.w));
    let y = cfg
        .y
        .unwrap_or(active.height / 3)
        .min(active.height.saturating_sub(cfg.h));

    let mut extra = Vec::new();
    if let Some(v) = blend {
        extra.push(v);
    }
    if let Some(v) = zpos {
        extra.push(v);
    }
    if let Some(id) = alpha {
        extra.push((id, 0xffff));
    }

    let ov = Overlay::create(
        fd, chosen, active.id, format, cfg.w, cfg.h, x, y, props, extra,
    )?;

    let after = get_properties(fd, chosen, DRM_MODE_OBJECT_PLANE)
        .map_err(|e| format!("readback properties: {}", e2s(&e)))?;
    let rfb = after.get("FB_ID").map(|p| p.value).unwrap_or(0);
    let rcrtc = after.get("CRTC_ID").map(|p| p.value).unwrap_or(0);
    println!(
        "readback: FB_ID={} CRTC_ID={} (expected fb={} crtc={})",
        rfb, rcrtc, ov.fb, ov.crtc
    );
    if rfb != ov.fb as u64 || rcrtc != ov.crtc as u64 {
        println!("[STEP3] WARN: committed state was not retained; HWC may have overwritten it");
    } else {
        println!(
            "[STEP3] PASS: plane {} shows fb {} at {}x{}+{},{} on crtc {}",
            chosen, ov.fb, cfg.w, cfg.h, x, y, ov.crtc
        );
    }

    println!();
    println!("Look at the phone: a translucent magenta block should be visible.");
    println!("From the PC run:  adb exec-out screencap -p > ovl_probe.png");
    println!("The block must be ABSENT from that PNG. If it is present, this path is wrong.");
    if cfg.hold == 0 {
        println!("Holding until Ctrl-C...");
    } else {
        println!("Holding for {}s (Ctrl-C to stop early)...", cfg.hold);
    }

    let deadline = if cfg.hold == 0 {
        None
    } else {
        Some(Instant::now() + Duration::from_secs(cfg.hold))
    };
    let mut last = (ov.fb as u64, ov.crtc as u64);
    let mut reasserts = 0u32;
    loop {
        if STOP.load(Ordering::SeqCst) {
            println!("signal received, cleaning up");
            break;
        }
        if let Some(d) = deadline {
            if Instant::now() >= d {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(16));
        if let Ok(p) = get_properties(fd, chosen, DRM_MODE_OBJECT_PLANE) {
            let cur = (
                p.get("FB_ID").map(|x| x.value).unwrap_or(0),
                p.get("CRTC_ID").map(|x| x.value).unwrap_or(0),
            );
            if cur != last {
                if cur.0 == 0 || cur.1 == 0 {
                    match ov.reassert() {
                        Ok(()) => {
                            reasserts += 1;
                            if reasserts <= 5 || reasserts % 50 == 0 {
                                println!(
                                    "[STEP4] plane was reset (fb {} crtc {}); re-asserted (#{})",
                                    cur.0, cur.1, reasserts
                                );
                            }
                            last = (ov.fb as u64, ov.crtc as u64);
                            continue;
                        }
                        Err(e) => {
                            println!("[STEP4] WARN: re-assert failed: {}", e2s(&e));
                        }
                    }
                }
                println!(
                    "[STEP4] WARN: plane state changed: fb {}->{} crtc {}->{} (HWC took the plane?)",
                    last.0, cur.0, last.1, cur.1
                );
                last = cur;
            }
        }
    }

    drop(ov);
    println!("cleaned up: plane detached, fb removed, dumb buffer destroyed");
    Ok(())
}

fn main() {
    let cfg = parse_args();
    if cfg.help {
        usage();
        return;
    }
    unsafe {
        libc::signal(libc::SIGINT, on_signal as *const () as usize);
        libc::signal(libc::SIGTERM, on_signal as *const () as usize);
    }
    println!("ovl-probe: DRM overlay plane probe (pidfd_getfd + atomic commit)");
    let uid = unsafe { libc::geteuid() };
    println!("uid={}", uid);
    if uid != 0 {
        println!("warning: not root; pidfd_getfd will usually fail with EPERM");
    }

    let fd = if cfg.direct {
        match open_card(&cfg.card) {
            Ok(fd) => {
                println!(
                    "[STEP1] SKIP: --direct, opened {} (not pidfd_getfd)",
                    cfg.card
                );
                fd
            }
            Err(e) => {
                println!("[STEP1] FAIL: open {}: {}", cfg.card, e2s(&e));
                print_err_hint(&e);
                return;
            }
        }
    } else {
        match acquire_drm_fd(&cfg) {
            Ok((fd, pid, comm, tfd)) => {
                println!(
                    "[STEP1] PASS: duplicated DRM fd from pid={} comm={} fd={}",
                    pid, comm, tfd
                );
                fd
            }
            Err(e) => {
                println!("[STEP1] FAIL: cannot obtain composer DRM fd: {}", e2s(&e));
                print_err_hint(&e);
                match open_card(&cfg.card) {
                    Ok(fd) => {
                        println!(
                            "falling back to direct open of {} (non-master; plane updates only)",
                            cfg.card
                        );
                        fd
                    }
                    Err(e2) => {
                        println!("direct open also failed: {}", e2s(&e2));
                        return;
                    }
                }
            }
        }
    };

    let result = run(fd, &cfg);
    unsafe {
        libc::close(fd);
    }
    match result {
        Ok(()) => println!("done"),
        Err(e) => {
            println!("[FAIL] {}", e);
            std::process::exit(1);
        }
    }
}
