//! Tier 1 Capability Servers (`Dir` and attenuated `Net`) over Zircon Channels.
//!
//! Each Tier 1 Hirsel process starts with zero ambient filesystem or network access.
//! Only when the operator explicitly grants `--grant-dir` or `--grant-net` does
//! `hirsel-init` attach a capability channel (`PA_HND(PA_USER0, 1)` for `Dir`,
//! `PA_HND(PA_USER0, 2)` for `Net`) and serve it on a dedicated background thread
//! until the child process exits.

#![cfg(target_os = "fuchsia")]

use hirsel_virtio::DiskImageStore;
use hirsel_virtio::blk::VirtioBlk;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use zx_sys::*;

const OP_OPEN_ROOT: u32 = 1;
const OP_OPEN: u32 = 2;
const OP_READ: u32 = 3;
const OP_WRITE: u32 = 4;
const OP_STAT: u32 = 5;
const OP_STAT_PATH: u32 = 6;
const OP_SYNC: u32 = 7;
const OP_CLOSE: u32 = 8;
const OP_UNLINK: u32 = 9;
const OP_RENAME: u32 = 10;
const OP_MKDIR: u32 = 11;

const REQ_HDR_LEN: usize = 28;

// Platform-independent Zircon Dir channel open flags.
const DIR_FLAG_READ: u32 = 1 << 0;
const DIR_FLAG_WRITE: u32 = 1 << 1;
const DIR_FLAG_CREAT: u32 = 1 << 2;
const DIR_FLAG_EXCL: u32 = 1 << 3;
const DIR_FLAG_TRUNC: u32 = 1 << 4;
const DIR_FLAG_APPEND: u32 = 1 << 5;

const S_IFREG: u32 = 0o100000;
const S_IFDIR: u32 = 0o040000;

const ENOENT: i32 = 2;
const EBADF: i32 = 9;
const EACCES: i32 = 13;
const EEXIST: i32 = 17;
const EINVAL: i32 = 22;
const EIO: i32 = 5;

/// Backing storage for Tier 1 `Dir` capability: `virtio-blk` persistent `HRSLFS01`
/// when a disk is attached, or an in-memory fallback map otherwise.
pub struct DirBackend {
    pub disk: Option<DiskImageStore<VirtioBlk>>,
    pub mem_files: HashMap<String, (Vec<u8>, u64)>,
}

impl DirBackend {
    fn get(&self, name: &str) -> Option<(Vec<u8>, u64)> {
        if let Some(disk) = self.disk.as_ref() {
            disk.fs_get(name).map(|f| (f.data.clone(), f.mtime_ns))
        } else {
            self.mem_files.get(name).cloned()
        }
    }

    fn put(&mut self, name: &str, data: Vec<u8>, mtime_ns: u64) -> Result<(), i32> {
        if let Some(disk) = self.disk.as_mut() {
            disk.fs_put(name, data, mtime_ns).map_err(|_| EIO)
        } else {
            self.mem_files.insert(name.to_owned(), (data, mtime_ns));
            Ok(())
        }
    }

    fn remove(&mut self, name: &str) -> Result<bool, i32> {
        if let Some(disk) = self.disk.as_mut() {
            disk.fs_remove(name).map_err(|_| EIO)
        } else {
            Ok(self.mem_files.remove(name).is_some())
        }
    }

    fn rename(&mut self, from: &str, to: &str) -> Result<bool, i32> {
        if let Some(disk) = self.disk.as_mut() {
            disk.fs_rename(from, to).map_err(|_| EIO)
        } else if let Some(val) = self.mem_files.remove(from) {
            self.mem_files.insert(to.to_owned(), val);
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

struct OpenHandle {
    name: String,
    readable: bool,
    writable: bool,
    append: bool,
    cursor: usize,
    data: Vec<u8>,
    mtime_ns: u64,
    dirty: bool,
}

fn sanitize_path(raw: &[u8]) -> Result<String, i32> {
    let s = std::str::from_utf8(raw).map_err(|_| EINVAL)?;
    let trimmed = s.trim_start_matches("./");
    if trimmed.is_empty()
        || trimmed.starts_with('/')
        || trimmed.contains("..")
        || trimmed.contains('\0')
        || trimmed.len() > 44
    {
        return Err(EACCES);
    }
    Ok(trimmed.to_owned())
}

fn write_reply(
    ch: zx_handle_t,
    txid: u32,
    errno: i32,
    arg0: u32,
    arg1: u32,
    val0: u64,
    val1: i64,
    payload: &[u8],
) {
    let mut buf = Vec::with_capacity(36 + payload.len());
    buf.extend_from_slice(&txid.to_le_bytes());
    buf.extend_from_slice(&errno.to_le_bytes());
    buf.extend_from_slice(&arg0.to_le_bytes());
    buf.extend_from_slice(&arg1.to_le_bytes());
    buf.extend_from_slice(&val0.to_le_bytes());
    buf.extend_from_slice(&val1.to_le_bytes());
    buf.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    buf.extend_from_slice(payload);
    unsafe {
        let _ = zx_channel_write(
            ch,
            0,
            buf.as_ptr(),
            buf.len() as u32,
            std::ptr::null(),
            0,
        );
    }
}

/// Serves a Tier 1 process's `Dir` Zircon channel until the child closes it.
pub fn serve_dir_channel(ch: zx_handle_t, backend: Arc<Mutex<DirBackend>>) {
    let mut open_files: HashMap<u32, OpenHandle> = HashMap::new();
    let mut next_id: u32 = 1;
    let mut buf = vec![0u8; 65536];

    loop {
        let mut obs: zx_signals_t = 0;
        let st = unsafe {
            zx_object_wait_one(
                ch,
                ZX_CHANNEL_READABLE | ZX_CHANNEL_PEER_CLOSED,
                ZX_TIME_INFINITE,
                &mut obs,
            )
        };
        if st != ZX_OK || (obs & ZX_CHANNEL_READABLE) == 0 {
            break;
        }
        let mut nbytes = 0u32;
        let mut nhandles = 0u32;
        let rd = unsafe {
            zx_channel_read(
                ch,
                0,
                buf.as_mut_ptr(),
                std::ptr::null_mut(),
                buf.len() as u32,
                0,
                &mut nbytes,
                &mut nhandles,
            )
        };
        if rd != ZX_OK || (nbytes as usize) < REQ_HDR_LEN {
            break;
        }

        let txid = u32::from_le_bytes(buf[0..4].try_into().unwrap());
        let op = u32::from_le_bytes(buf[4..8].try_into().unwrap());
        let arg0 = u32::from_le_bytes(buf[8..12].try_into().unwrap());
        let arg1 = u32::from_le_bytes(buf[12..16].try_into().unwrap());
        let offset = i64::from_le_bytes(buf[16..24].try_into().unwrap());
        let plen = u32::from_le_bytes(buf[24..28].try_into().unwrap()) as usize;
        let payload = &buf[REQ_HDR_LEN..(REQ_HDR_LEN + plen).min(nbytes as usize)];

        match op {
            OP_OPEN_ROOT => {
                write_reply(ch, txid, 0, 0, 0, 0, 0, &[]);
            }
            OP_OPEN => {
                let name = match sanitize_path(payload) {
                    Ok(n) => n,
                    Err(e) => {
                        write_reply(ch, txid, e, 0, 0, 0, 0, &[]);
                        continue;
                    }
                };
                let flags = arg0;
                let readable = (flags & DIR_FLAG_READ) != 0;
                let writable = (flags & DIR_FLAG_WRITE) != 0;
                let creat = (flags & DIR_FLAG_CREAT) != 0;
                let excl = (flags & DIR_FLAG_EXCL) != 0;
                let trunc = (flags & DIR_FLAG_TRUNC) != 0;
                let append = (flags & DIR_FLAG_APPEND) != 0;

                let mut guard = backend.lock().unwrap();
                let existing = guard.get(&name);
                let now_ns = unsafe { zx_clock_get_monotonic() } as u64;
                let (data, mtime_ns, dirty) = match existing {
                    Some((d, mt)) => {
                        if creat && excl {
                            write_reply(ch, txid, EEXIST, 0, 0, 0, 0, &[]);
                            continue;
                        }
                        if trunc && writable {
                            (Vec::new(), now_ns, true)
                        } else {
                            (d, mt, false)
                        }
                    }
                    None => {
                        if !creat {
                            write_reply(ch, txid, ENOENT, 0, 0, 0, 0, &[]);
                            continue;
                        }
                        if let Err(e) = guard.put(&name, Vec::new(), now_ns) {
                            write_reply(ch, txid, e, 0, 0, 0, 0, &[]);
                            continue;
                        }
                        (Vec::new(), now_ns, true)
                    }
                };
                if dirty {
                    let _ = guard.put(&name, data.clone(), mtime_ns);
                }
                drop(guard);

                let id = next_id;
                next_id = next_id.wrapping_add(1).max(1);
                let cursor = if append { data.len() } else { 0 };
                open_files.insert(
                    id,
                    OpenHandle {
                        name,
                        readable,
                        writable,
                        append,
                        cursor,
                        data,
                        mtime_ns,
                        dirty: false,
                    },
                );
                write_reply(ch, txid, 0, id, 0, 0, 0, &[]);
            }
            OP_READ => {
                let Some(f) = open_files.get_mut(&arg0) else {
                    write_reply(ch, txid, EBADF, 0, 0, 0, 0, &[]);
                    continue;
                };
                if !f.readable {
                    write_reply(ch, txid, EBADF, 0, 0, 0, 0, &[]);
                    continue;
                }
                let start = if offset >= 0 { offset as usize } else { f.cursor };
                let max_len = (arg1 as usize).min(60_000);
                if start >= f.data.len() {
                    write_reply(ch, txid, 0, 0, 0, 0, 0, &[]);
                    continue;
                }
                let end = (start + max_len).min(f.data.len());
                let slice = f.data[start..end].to_vec();
                if offset < 0 {
                    f.cursor = end;
                }
                write_reply(ch, txid, 0, slice.len() as u32, 0, 0, 0, &slice);
            }
            OP_WRITE => {
                let Some(f) = open_files.get_mut(&arg0) else {
                    write_reply(ch, txid, EBADF, 0, 0, 0, 0, &[]);
                    continue;
                };
                if !f.writable {
                    write_reply(ch, txid, EBADF, 0, 0, 0, 0, &[]);
                    continue;
                }
                let pos = if offset >= 0 {
                    offset as usize
                } else if f.append {
                    f.data.len()
                } else {
                    f.cursor
                };
                let end = pos + payload.len();
                if end > f.data.len() {
                    f.data.resize(end, 0);
                }
                f.data[pos..end].copy_from_slice(payload);
                if offset < 0 {
                    f.cursor = end;
                }
                f.mtime_ns = unsafe { zx_clock_get_monotonic() } as u64;
                f.dirty = true;
                // Write-through to persistent store so durability survives unexpected exit.
                let res = backend.lock().unwrap().put(&f.name, f.data.clone(), f.mtime_ns);
                match res {
                    Ok(()) => {
                        f.dirty = false;
                        write_reply(ch, txid, 0, payload.len() as u32, 0, 0, 0, &[]);
                    }
                    Err(e) => write_reply(ch, txid, e, 0, 0, 0, 0, &[]),
                }
            }
            OP_STAT => {
                if arg0 == 0 {
                    write_reply(ch, txid, 0, S_IFDIR | 0o700, 0, 0, 0, &[]);
                } else if let Some(f) = open_files.get(&arg0) {
                    write_reply(
                        ch,
                        txid,
                        0,
                        S_IFREG | 0o600,
                        0,
                        f.data.len() as u64,
                        f.mtime_ns as i64,
                        &[],
                    );
                } else {
                    write_reply(ch, txid, EBADF, 0, 0, 0, 0, &[]);
                }
            }
            OP_STAT_PATH => {
                let name = match sanitize_path(payload) {
                    Ok(n) => n,
                    Err(e) => {
                        write_reply(ch, txid, e, 0, 0, 0, 0, &[]);
                        continue;
                    }
                };
                let guard = backend.lock().unwrap();
                match guard.get(&name) {
                    Some((d, mt)) => {
                        write_reply(ch, txid, 0, S_IFREG | 0o600, 0, d.len() as u64, mt as i64, &[]);
                    }
                    None => write_reply(ch, txid, ENOENT, 0, 0, 0, 0, &[]),
                }
            }
            OP_SYNC => {
                if let Some(f) = open_files.get_mut(&arg0) {
                    if f.dirty {
                        let _ = backend.lock().unwrap().put(&f.name, f.data.clone(), f.mtime_ns);
                        f.dirty = false;
                    }
                }
                write_reply(ch, txid, 0, 0, 0, 0, 0, &[]);
            }
            OP_CLOSE => {
                if let Some(f) = open_files.remove(&arg0) {
                    if f.dirty {
                        let _ = backend.lock().unwrap().put(&f.name, f.data, f.mtime_ns);
                    }
                }
                write_reply(ch, txid, 0, 0, 0, 0, 0, &[]);
            }
            OP_UNLINK => {
                let name = match sanitize_path(payload) {
                    Ok(n) => n,
                    Err(e) => {
                        write_reply(ch, txid, e, 0, 0, 0, 0, &[]);
                        continue;
                    }
                };
                match backend.lock().unwrap().remove(&name) {
                    Ok(true) => write_reply(ch, txid, 0, 0, 0, 0, 0, &[]),
                    Ok(false) => write_reply(ch, txid, ENOENT, 0, 0, 0, 0, &[]),
                    Err(e) => write_reply(ch, txid, e, 0, 0, 0, 0, &[]),
                }
            }
            OP_RENAME => {
                let split = arg0 as usize;
                if split > payload.len() {
                    write_reply(ch, txid, EINVAL, 0, 0, 0, 0, &[]);
                    continue;
                }
                let from = match sanitize_path(&payload[..split]) {
                    Ok(n) => n,
                    Err(e) => {
                        write_reply(ch, txid, e, 0, 0, 0, 0, &[]);
                        continue;
                    }
                };
                let to = match sanitize_path(&payload[split..]) {
                    Ok(n) => n,
                    Err(e) => {
                        write_reply(ch, txid, e, 0, 0, 0, 0, &[]);
                        continue;
                    }
                };
                match backend.lock().unwrap().rename(&from, &to) {
                    Ok(true) => write_reply(ch, txid, 0, 0, 0, 0, 0, &[]),
                    Ok(false) => write_reply(ch, txid, ENOENT, 0, 0, 0, 0, &[]),
                    Err(e) => write_reply(ch, txid, e, 0, 0, 0, 0, &[]),
                }
            }
            OP_MKDIR => {
                write_reply(ch, txid, 0, 0, 0, 0, 0, &[]);
            }
            _ => {
                write_reply(ch, txid, EINVAL, 0, 0, 0, 0, &[]);
            }
        }
    }

    // Flush any remaining dirty files on channel teardown.
    for (_, f) in open_files {
        if f.dirty {
            let _ = backend.lock().unwrap().put(&f.name, f.data, f.mtime_ns);
        }
    }
    unsafe { zx_handle_close(ch) };
}
