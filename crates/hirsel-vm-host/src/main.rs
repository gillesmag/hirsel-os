//! Tier-2 Linux 6.1.102 MicroVM host daemon for Hirsel-OS Desktop (`vm-dev` pane).
//!
//! Spawned by `hirsel_shell` with:
//!   - PA_USER0[0]: fuchsia.kernel.HypervisorResource handle
//!   - PA_USER0[1]: fuchsia.kernel.VmexResource handle
//!   - /pkg: read-only package dir containing `data/vm/vmlinux.stripped` & `data/vm/initramfs-ssh.cpio`
//!   - /data: optional persistent Fxfs storage dir for `vm-dev.img` (16 MiB ext2 volume -> /mnt/data)
//!   - stdin/stdout: newline-delimited host <-> VM control protocol (`EXEC`, `STDIN`, `STOP` / `LOG`, `READY`, `EXEC_RES`)

#[cfg(target_os = "fuchsia")]
mod fuchsia_impl {
    use hirsel_vmm::ManagedVmControl;
    use hirsel_vmm::virtio_blk_mmio::{BLK_SECTOR_SIZE, MemBlockBackend, VmBlockBackend};
    use std::fs::{File, OpenOptions};
    use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
    use std::sync::{Arc, Mutex};
    use std::thread;
    use zx_sys::*;

    struct FileBlockBackend {
        file: File,
        sectors: u64,
    }

    impl FileBlockBackend {
        fn open_or_create(path: &str, size_bytes: u64) -> Result<Self, String> {
            let file = OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)
                .map_err(|e| format!("open {path}: {e}"))?;
            let meta = file.metadata().map_err(|e| format!("metadata {path}: {e}"))?;
            if meta.len() < size_bytes {
                file.set_len(size_bytes)
                    .map_err(|e| format!("set_len {path}: {e}"))?;
            }
            let sectors = size_bytes / BLK_SECTOR_SIZE as u64;
            Ok(Self { file, sectors })
        }
    }

    impl VmBlockBackend for FileBlockBackend {
        fn capacity_sectors(&self) -> u64 {
            self.sectors
        }

        fn read_sectors(&mut self, sector: u64, dst: &mut [u8]) -> Result<(), String> {
            let off = sector.saturating_mul(BLK_SECTOR_SIZE as u64);
            self.file
                .seek(SeekFrom::Start(off))
                .map_err(|e| format!("seek: {e}"))?;
            self.file
                .read_exact(dst)
                .map_err(|e| format!("read: {e}"))
        }

        fn write_sectors(&mut self, sector: u64, src: &[u8]) -> Result<(), String> {
            let off = sector.saturating_mul(BLK_SECTOR_SIZE as u64);
            self.file
                .seek(SeekFrom::Start(off))
                .map_err(|e| format!("seek: {e}"))?;
            self.file
                .write_all(src)
                .map_err(|e| format!("write: {e}"))
        }

        fn flush(&mut self) -> Result<(), String> {
            self.file.sync_data().map_err(|e| format!("sync: {e}"))
        }
    }

    fn b64_encode(input: &[u8]) -> String {
        const TBL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
        for chunk in input.chunks(3) {
            let b0 = chunk[0];
            let b1 = if chunk.len() > 1 { chunk[1] } else { 0 };
            let b2 = if chunk.len() > 2 { chunk[2] } else { 0 };
            out.push(TBL[(b0 >> 2) as usize] as char);
            out.push(TBL[(((b0 & 3) << 4) | (b1 >> 4)) as usize] as char);
            if chunk.len() > 1 {
                out.push(TBL[(((b1 & 0xf) << 2) | (b2 >> 6)) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(TBL[(b2 & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out
    }

    fn b64_decode(s: &str) -> Vec<u8> {
        fn val(b: u8) -> Option<u8> {
            match b {
                b'A'..=b'Z' => Some(b - b'A'),
                b'a'..=b'z' => Some(b - b'a' + 26),
                b'0'..=b'9' => Some(b - b'0' + 52),
                b'+' => Some(62),
                b'/' => Some(63),
                _ => None,
            }
        }
        let bytes: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        let mut out = Vec::with_capacity((bytes.len() / 4) * 3);
        for chunk in bytes.chunks(4) {
            if chunk.len() < 2 {
                break;
            }
            let Some(v0) = val(chunk[0]) else { break };
            let Some(v1) = val(chunk[1]) else { break };
            out.push((v0 << 2) | (v1 >> 4));
            if chunk.len() > 2 && chunk[2] != b'=' {
                if let Some(v2) = val(chunk[2]) {
                    out.push((v1 << 4) | (v2 >> 2));
                    if chunk.len() > 3 && chunk[3] != b'=' {
                        if let Some(v3) = val(chunk[3]) {
                            out.push((v2 << 6) | v3);
                        }
                    }
                }
            }
        }
        out
    }

    fn emit_line(out: &Arc<Mutex<std::io::Stdout>>, line: &str) {
        if let Ok(mut w) = out.lock() {
            let _ = writeln!(w, "{line}");
            let _ = w.flush();
        }
    }

    pub fn run() -> Result<(), String> {
        let out = Arc::new(Mutex::new(std::io::stdout()));
        let emit_log = |msg: &str| {
            emit_line(&out, &format!("LOG {}", b64_encode(msg.as_bytes())));
        };

        let hyp_res = unsafe { zx_take_startup_handle(pa_hnd(PA_USER0, 0)) };
        let vmex_res = unsafe { zx_take_startup_handle(pa_hnd(PA_USER0, 1)) };
        if hyp_res == ZX_HANDLE_INVALID || vmex_res == ZX_HANDLE_INVALID {
            return Err(format!(
                "missing startup resource handles (hyp={hyp_res}, vmex={vmex_res})"
            ));
        }

        let kernel = std::fs::read("/pkg/data/vm/vmlinux.stripped")
            .map_err(|e| format!("read /pkg/data/vm/vmlinux.stripped: {e}"))?;
        let initrd = std::fs::read("/pkg/data/vm/initramfs-ssh.cpio")
            .map_err(|e| format!("read /pkg/data/vm/initramfs-ssh.cpio: {e}"))?;

        let blk_backend: Arc<Mutex<dyn VmBlockBackend>> =
            match FileBlockBackend::open_or_create("/data/vm-dev.img", 16 * 1024 * 1024) {
                Ok(fb) => {
                    emit_log("[hirsel-vm-host] attached persistent 16 MiB Fxfs disk /data/vm-dev.img -> /dev/vda");
                    Arc::new(Mutex::new(fb))
                }
                Err(e) => {
                    emit_log(&format!("[hirsel-vm-host] /data unavailable ({e}), using 8 MiB RAM disk"));
                    Arc::new(Mutex::new(MemBlockBackend::new(vec![0u8; 8 * 1024 * 1024])))
                }
            };

        emit_log(&format!(
            "[hirsel-vm-host] booting Linux 6.1.102 (kernel={} KiB, initrd={} KiB, 2 vCPUs, 128 MiB RAM, AMD-SVM/NPT)",
            kernel.len() / 1024,
            initrd.len() / 1024,
        ));

        let ctrl = ManagedVmControl::new(None);

        // Background thread: stream guest COM1 console output to hirsel_shell and detect READY.
        let ctrl_log = Arc::clone(&ctrl);
        let out_log = Arc::clone(&out);
        thread::spawn(move || {
            let mut offset = 0u64;
            let mut partial = Vec::<u8>::new();
            let mut sent_ready = false;
            loop {
                let (chunk, next) = ctrl_log.read_console_logs(offset, 4096);
                if !chunk.is_empty() {
                    offset = next;
                    for b in chunk {
                        if b == b'\n' {
                            let line = String::from_utf8_lossy(&partial).trim_end_matches('\r').to_string();
                            partial.clear();
                            if !line.is_empty() {
                                emit_line(&out_log, &format!("LOG {}", b64_encode(line.as_bytes())));
                                if !sent_ready && line.contains("virtio-console guest agent active") {
                                    sent_ready = true;
                                    emit_line(&out_log, "READY");
                                }
                            }
                        } else if partial.len() < 1024 {
                            partial.push(b);
                        }
                    }
                } else {
                    let now = unsafe { zx_clock_get_monotonic() };
                    unsafe { zx_nanosleep(now.saturating_add(15_000_000)) };
                }
                if ctrl_log.is_stop_requested() {
                    break;
                }
            }
        });

        // Background thread: read commands from hirsel_shell on stdin.
        let ctrl_cmd = Arc::clone(&ctrl);
        let out_cmd = Arc::clone(&out);
        thread::spawn(move || {
            let stdin = std::io::stdin();
            let reader = BufReader::new(stdin.lock());
            for line in reader.lines() {
                let Ok(line) = line else { break };
                let trimmed = line.trim();
                if trimmed == "STOP" {
                    ctrl_cmd.request_stop();
                    break;
                }
                if let Some(rest) = trimmed.strip_prefix("STDIN ") {
                    let bytes = b64_decode(rest);
                    if !bytes.is_empty() {
                        ctrl_cmd.write_console_in(&bytes);
                    }
                    continue;
                }
                if let Some(rest) = trimmed.strip_prefix("EXEC ") {
                    let mut parts = rest.splitn(2, ' ');
                    let id = parts.next().unwrap_or("0");
                    let b64_cmd = parts.next().unwrap_or("");
                    let ch = &ctrl_cmd.console_channel;
                    ch.host_clear_rx();
                    let req = format!("HREXEC {b64_cmd}\n");
                    ch.host_write(req.as_bytes());
                    let deadline = unsafe { zx_clock_get_monotonic() }.saturating_add(15_000_000_000);
                    let mut replied = false;
                    loop {
                        if let Some(resp) = ch.host_try_read_line() {
                            if let Some(done) = resp.strip_prefix("HRDONE ") {
                                let (rc_s, b64_out) = done.split_once(' ').unwrap_or((done, ""));
                                emit_line(&out_cmd, &format!("EXEC_RES {id} {} {}", rc_s.trim(), b64_out.trim()));
                                replied = true;
                                break;
                            }
                        }
                        let now = unsafe { zx_clock_get_monotonic() };
                        if now >= deadline || ctrl_cmd.is_stop_requested() {
                            break;
                        }
                        unsafe { zx_nanosleep(now.saturating_add(5_000_000)) };
                    }
                    if !replied {
                        let msg = b64_encode(b"vm-dev exec timed out waiting for /dev/hvc0");
                        emit_line(&out_cmd, &format!("EXEC_RES {id} -1 {msg}"));
                    }
                }
            }
        });

        let report = hirsel_vmm::run_managed_linux_vm(
            hyp_res,
            vmex_res,
            &kernel,
            Some(&initrd),
            "console=ttyS0 reboot=k panic=1 pci=off nomodules hirsel.name=vm-dev",
            128 * 1024 * 1024,
            2,
            None,
            Some(blk_backend),
            ctrl,
        )?;
        emit_log(&format!("[hirsel-vm-host] VM exited cleanly rc={}", report.exit_code));
        Ok(())
    }
}

fn main() {
    #[cfg(target_os = "fuchsia")]
    {
        if let Err(e) = fuchsia_impl::run() {
            println!("ERR {e}");
            std::process::exit(1);
        }
    }
    #[cfg(not(target_os = "fuchsia"))]
    {
        eprintln!("hirsel-vm-host runs on x86_64-unknown-fuchsia");
        std::process::exit(1);
    }
}
