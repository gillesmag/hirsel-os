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
    use hirsel_vmm::virtio_net_mmio::VirtioNetChannel;
    use smoltcp::iface::{Config as IfaceConfig, Interface, SocketSet};
    use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
    use smoltcp::socket::tcp::{Socket as TcpSocket, SocketBuffer as TcpSocketBuffer};
    use smoltcp::time::Instant as SmolInstant;
    use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address};
    use std::collections::VecDeque;
    use std::fs::{File, OpenOptions};
    use std::io::{BufRead, BufReader, Read, Seek, SeekFrom, Write};
    use std::net::TcpStream;
    use std::sync::{Arc, Mutex};
    use std::thread;
    use zx_sys::*;

    struct BridgeNetDev {
        ch: Arc<VirtioNetChannel>,
        rx_buf: VecDeque<Vec<u8>>,
    }
    struct BridgeRx(Vec<u8>);
    impl RxToken for BridgeRx {
        fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R { f(&self.0) }
    }
    struct BridgeTx(Arc<VirtioNetChannel>);
    impl TxToken for BridgeTx {
        fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
            let mut buf = vec![0u8; len];
            let r = f(&mut buf);
            self.0.host_push_tx(buf);
            r
        }
    }
    impl Device for BridgeNetDev {
        type RxToken<'a> = BridgeRx where Self: 'a;
        type TxToken<'a> = BridgeTx where Self: 'a;
        fn receive(&mut self, _: SmolInstant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
            let frame = self.rx_buf.pop_front()?;
            Some((BridgeRx(frame), BridgeTx(Arc::clone(&self.ch))))
        }
        fn transmit(&mut self, _: SmolInstant) -> Option<Self::TxToken<'_>> {
            Some(BridgeTx(Arc::clone(&self.ch)))
        }
        fn capabilities(&self) -> DeviceCapabilities {
            let mut caps = DeviceCapabilities::default();
            caps.medium = Medium::Ethernet;
            caps.max_transmission_unit = 1514;
            caps.max_burst_size = Some(16);
            caps
        }
    }

    fn spawn_egress_net_bridge(ch: Arc<VirtioNetChannel>, ctrl: Arc<ManagedVmControl>) {
        thread::spawn(move || {
            let mut dev = BridgeNetDev { ch: Arc::clone(&ch), rx_buf: VecDeque::new() };
            let gw_mac = EthernetAddress([0x52, 0x54, 0x00, 0x48, 0x52, 0x00]);
            let cfg = IfaceConfig::new(HardwareAddress::Ethernet(gw_mac));
            let now_ms = || -> SmolInstant {
                let ns = unsafe { zx_clock_get_monotonic() };
                SmolInstant::from_millis((ns / 1_000_000) as i64)
            };
            let mut iface = Interface::new(cfg, &mut dev, now_ms());
            iface.update_ip_addrs(|addrs| {
                let _ = addrs.push(IpCidr::new(IpAddress::Ipv4(Ipv4Address::new(10, 0, 3, 1)), 24));
            });

            let mut sockets = SocketSet::new(vec![]);
            let tcp_rx = TcpSocketBuffer::new(vec![0u8; 16384]);
            let tcp_tx = TcpSocketBuffer::new(vec![0u8; 16384]);
            let mut sock = TcpSocket::new(tcp_rx, tcp_tx);
            let _ = sock.listen(7781);
            let handle = sockets.add(sock);

            let mut host_conn: Option<TcpStream> = None;
            let mut to_host_buf = Vec::<u8>::new();
            let mut to_guest_buf = Vec::<u8>::new();
            let mut tmp = [0u8; 4096];

            while !ctrl.is_stop_requested() {
                let mut did_work = false;
                while let Some(frame) = ch.host_pop_rx() {
                    dev.rx_buf.push_back(frame);
                    did_work = true;
                }
                let t = now_ms();
                iface.poll(t, &mut dev, &mut sockets);

                let sock = sockets.get_mut::<TcpSocket>(handle);
                if sock.is_active() {
                    if host_conn.is_none() {
                        match TcpStream::connect("127.0.0.1:7781") {
                            Ok(s) => {
                                let _ = s.set_nonblocking(true);
                                host_conn = Some(s);
                                to_host_buf.clear();
                                to_guest_buf.clear();
                            }
                            Err(_) => {
                                sock.abort();
                            }
                        }
                    }
                    if sock.can_recv() {
                        if let Ok(n) = sock.recv_slice(&mut tmp) {
                            if n > 0 {
                                to_host_buf.extend_from_slice(&tmp[..n]);
                                did_work = true;
                            }
                        }
                    }
                    let mut host_eof = false;
                    if let Some(ref mut hc) = host_conn {
                        if !to_host_buf.is_empty() {
                            match hc.write(&to_host_buf) {
                                Ok(n) if n > 0 => {
                                    to_host_buf.drain(..n);
                                    did_work = true;
                                }
                                Ok(_) => {}
                                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                                Err(_) => host_eof = true,
                            }
                        }
                        if to_guest_buf.len() < 16384 {
                            match hc.read(&mut tmp) {
                                Ok(0) => host_eof = true,
                                Ok(n) => {
                                    to_guest_buf.extend_from_slice(&tmp[..n]);
                                    did_work = true;
                                }
                                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                                Err(_) => host_eof = true,
                            }
                        }
                    }
                    if !to_guest_buf.is_empty() && sock.can_send() {
                        if let Ok(n) = sock.send_slice(&to_guest_buf) {
                            if n > 0 {
                                to_guest_buf.drain(..n);
                                did_work = true;
                            }
                        }
                    }
                    if host_eof && to_guest_buf.is_empty() {
                        sock.close();
                        host_conn = None;
                    }
                } else {
                    host_conn = None;
                    to_host_buf.clear();
                    to_guest_buf.clear();
                    if !sock.is_listening() {
                        let _ = sock.listen(7781);
                    }
                }

                iface.poll(now_ms(), &mut dev, &mut sockets);
                if !did_work {
                    let now = unsafe { zx_clock_get_monotonic() };
                    unsafe { zx_nanosleep(now.saturating_add(2_000_000)) };
                }
            }
        });
    }

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

        let net_ch = VirtioNetChannel::new();
        let ctrl = ManagedVmControl::new(Some(Arc::clone(&net_ch)));
        spawn_egress_net_bridge(Arc::clone(&net_ch), Arc::clone(&ctrl));
        emit_log("[hirsel-vm-host] attached capability-gated virtio-net eth0 (10.0.3.2 -> gw 10.0.3.1:7781 egress_gw.cm ONLY)");

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
            "console=ttyS0 reboot=k panic=1 pci=off nomodules hirsel.name=vm-dev hirsel.ip=10.0.3.2/24 hirsel.gw=10.0.3.1",
            128 * 1024 * 1024,
            2,
            Some([0x52, 0x54, 0x00, 0x48, 0x52, 0x01]),
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
