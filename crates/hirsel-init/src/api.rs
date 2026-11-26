//! Talos-style authenticated & AEAD-encrypted multi-transport control plane API
//! server for Hirsel-OS on Zircon.
//!
//! Brings up Ring-3 hardware drivers directly on Zircon capabilities:
//! - **`virtio-blk` + `DiskImageStore`**: Persistent content-addressed image store,
//!   A/B ZBI upgrade slots (`HRSLAB01`), and Tier 1 `Dir` file namespace (`HRSLFS01`).
//! - **`virtio-net` + `smoltcp` TCP/IP**: Encrypted Ethernet/IPv4/TCP API on `10.0.2.15:4444`.
//! - **COM2 16550 UART (`0x2F8`)**: Encrypted serial control plane fallback.

#![cfg(target_os = "fuchsia")]

use crate::caps::DirBackend;
use crate::runner::{BootContext, Tier1LaunchOpts};
use core::arch::asm;
use hirsel_proto::crypto::{
    DEFAULT_DEV_KEY_STR, SessionCipher, compute_client_auth_mac, compute_server_auth_mac, ct_eq,
    derive_cluster_key,
};
use hirsel_proto::{
    FLAG_ENCRYPTED, FRAME_HEADER_LEN, FRAME_MAGIC, ImageEntry, MAX_FRAME_PAYLOAD, NodeInfo,
    PROTO_VERSION, Request, Response, RunResult, RunTarget, Tier2Mode, UpgradeInfo, crc32,
    hex_encode, read_frame_with_cipher, sha256, write_frame,
};
use hirsel_virtio::DiskImageStore;
use hirsel_virtio::blk::VirtioBlk;
use hirsel_virtio::dma::ZirconIommu;
use hirsel_virtio::net::VirtioNet;
use hirsel_virtio::pci::PciBus;
use smoltcp::iface::{Config as IfaceConfig, Interface, SocketSet};
use smoltcp::socket::tcp;
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address};
use std::collections::HashMap;
use std::io::{self, Read, Write};
use std::sync::{Arc, Mutex};
use std::thread;
use zx_sys::*;

const COM2_BASE: u16 = 0x02F8;
const COM2_LEN: u32 = 8;
const API_TCP_PORT: u16 = 4444;
const LINUX_SHIM_BOOTFS_PATH: &str = "bin/hirsel-linux-shim.elf";

#[inline]
unsafe fn inb(port: u16) -> u8 {
    let val: u8;
    unsafe {
        asm!("in al, dx", out("al") val, in("dx") port, options(nomem, nostack, preserves_flags));
    }
    val
}

#[inline]
unsafe fn outb(port: u16, val: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") val, options(nomem, nostack, preserves_flags));
    }
}

/// Ring-3 hardware 16550 UART transport on COM2 (`0x2F8..0x300`).
struct Com2Uart {
    base: u16,
    idle_poll: bool,
}

impl Com2Uart {
    fn open(ioport_resource: zx_handle_t) -> Result<Self, String> {
        if ioport_resource == ZX_HANDLE_INVALID {
            return Err("ioport_resource not delivered by userboot".into());
        }
        let st = unsafe { zx_ioports_request(ioport_resource, COM2_BASE, COM2_LEN) };
        if st != ZX_OK {
            return Err(format!("zx_ioports_request(COM2) failed: zx_status={st}"));
        }
        unsafe {
            outb(COM2_BASE + 1, 0x00);
            outb(COM2_BASE + 3, 0x80);
            outb(COM2_BASE + 0, 0x01);
            outb(COM2_BASE + 1, 0x00);
            outb(COM2_BASE + 3, 0x03);
            outb(COM2_BASE + 2, 0xC7);
            outb(COM2_BASE + 4, 0x0B);
            for _ in 0..64 {
                if (inb(COM2_BASE + 5) & 0x01) == 0 {
                    break;
                }
                let _ = inb(COM2_BASE);
            }
        }
        Ok(Self {
            base: COM2_BASE,
            idle_poll: true,
        })
    }

    fn read_byte(&mut self) -> u8 {
        let mut spins: u32 = 0;
        loop {
            if unsafe { (inb(self.base + 5) & 0x01) != 0 } {
                self.idle_poll = false;
                return unsafe { inb(self.base) };
            }
            spins = spins.wrapping_add(1);
            if self.idle_poll || spins > 10_000 {
                let delay = if self.idle_poll { 2_000_000 } else { 50_000 };
                let deadline = unsafe { zx_clock_get_monotonic() }.saturating_add(delay);
                unsafe { zx_nanosleep(deadline) };
                spins = 0;
            } else {
                core::hint::spin_loop();
            }
        }
    }

    fn write_byte(&self, byte: u8) {
        let mut spins: u32 = 0;
        while unsafe { (inb(self.base + 5) & 0x20) == 0 } {
            spins = spins.wrapping_add(1);
            if spins > 10_000 {
                let deadline = unsafe { zx_clock_get_monotonic() }.saturating_add(50_000);
                unsafe { zx_nanosleep(deadline) };
                spins = 0;
            } else {
                core::hint::spin_loop();
            }
        }
        unsafe { outb(self.base, byte) };
    }
}

impl Read for Com2Uart {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        buf[0] = self.read_byte();
        let mut n = 1;
        while n < buf.len() && unsafe { (inb(self.base + 5) & 0x01) != 0 } {
            buf[n] = unsafe { inb(self.base) };
            n += 1;
        }
        Ok(n)
    }
}

impl Write for Com2Uart {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        for &b in buf {
            self.write_byte(b);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct CachedImage {
    name: String,
    bytes: Vec<u8>,
    exec_vmo: zx_handle_t,
    persisted: bool,
}

struct PendingUpload {
    digest: [u8; 32],
    total_size: usize,
    name: String,
    buf: Vec<u8>,
}

struct PendingUpgrade {
    digest: [u8; 32],
    total_size: usize,
    buf: Vec<u8>,
}

enum AuthState {
    Unauthenticated,
    ChallengeSent {
        client_nonce: [u8; 32],
        server_nonce: [u8; 32],
    },
    Authenticated(SessionCipher),
}

struct SessionState {
    auth: AuthState,
    pending: Option<PendingUpload>,
    pending_upgrade: Option<PendingUpgrade>,
}

impl SessionState {
    fn new() -> Self {
        Self {
            auth: AuthState::Unauthenticated,
            pending: None,
            pending_upgrade: None,
        }
    }

    fn reset(&mut self) {
        self.auth = AuthState::Unauthenticated;
        self.pending = None;
        self.pending_upgrade = None;
    }

    fn cipher_mut(&mut self) -> Option<&mut SessionCipher> {
        match &mut self.auth {
            AuthState::Authenticated(c) => Some(c),
            _ => None,
        }
    }
}

struct NodeState {
    cache: HashMap<[u8; 32], CachedImage>,
    dir_backend: Arc<Mutex<DirBackend>>,
    auth_key: [u8; 32],
    has_net: bool,
    has_blk: bool,
    disk_mib: u32,
    tier1_runs: u64,
    tier2_runs: u64,
}

/// Initializes Virtio-Blk + Virtio-Net + COM2 and runs the encrypted control-plane servers forever.
pub fn serve_control_plane(ctx: Arc<BootContext>, initial_t1: u64, initial_t2: u64) -> ! {
    let mut disk_store: Option<DiskImageStore<VirtioBlk>> = None;
    let mut net_dev: Option<VirtioNet> = None;
    let mut _iommu: Option<ZirconIommu> = None;

    if ctx.iommu_resource != ZX_HANDLE_INVALID
        && ctx.mmio_resource != ZX_HANDLE_INVALID
        && ctx.ioport_resource != ZX_HANDLE_INVALID
    {
        match PciBus::open(ctx.ioport_resource)
            .and_then(|bus| ZirconIommu::create(ctx.iommu_resource).map(|iommu| (bus, iommu)))
        {
            Ok((bus, iommu)) => {
                match VirtioBlk::probe(&bus, &iommu, ctx.mmio_resource) {
                    Ok(Some(blk)) => match DiskImageStore::open_or_format(blk) {
                        Ok((store, formatted)) => {
                            let up = store.upgrade_info();
                            println!(
                                "[hirsel-stored] virtio-blk ready: {} MiB ({}, {} images, {} dir files, active_slot={}, pending_slot={})",
                                store.capacity_mib(),
                                if formatted { "formatted fresh HRSLBLK1" } else { "mounted existing HRSLBLK1" },
                                store.entry_count(),
                                store.fs_list().len(),
                                slot_label(up.active_slot),
                                slot_label(up.pending_slot),
                            );
                            disk_store = Some(store);
                        }
                        Err(e) => println!("[hirsel-stored] disk store init failed: {e}"),
                    },
                    Ok(None) => println!("[hirsel-stored] no virtio-blk PCI device attached"),
                    Err(e) => println!("[hirsel-stored] virtio-blk probe error: {e}"),
                }

                match VirtioNet::probe(&bus, &iommu, ctx.mmio_resource) {
                    Ok(Some(net)) => {
                        println!(
                            "[hirsel-netd] virtio-net ready: mac={:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}, ip=10.0.2.15/24, gw=10.0.2.2",
                            net.mac[0], net.mac[1], net.mac[2], net.mac[3], net.mac[4], net.mac[5],
                        );
                        net_dev = Some(net);
                    }
                    Ok(None) => println!("[hirsel-netd] no virtio-net PCI device attached"),
                    Err(e) => println!("[hirsel-netd] virtio-net probe error: {e}"),
                }

                _iommu = Some(iommu);
            }
            Err(e) => println!("[hirsel-os] PCI/IOMMU init skipped: {e}"),
        }
    }

    // Hydrate persisted images from `virtio-blk` into Zircon's immutable executable VMO cache!
    let mut cache: HashMap<[u8; 32], CachedImage> = HashMap::new();
    let (has_blk, disk_mib) = if let Some(store) = disk_store.as_mut() {
        let metas: Vec<_> = store.entries().to_vec();
        for meta in metas {
            match store.read_image(&meta).and_then(|bytes| {
                ctx.seal_executable_vmo(&meta.name, &bytes)
                    .map(|vmo| (bytes, vmo))
            }) {
                Ok((bytes, exec_vmo)) => {
                    let short = &hex_encode(&meta.digest)[..12];
                    println!(
                        "[hirsel-stored] hydrated `{}` (sha256:{short}, {} bytes) from virtio-blk",
                        meta.name, meta.byte_len
                    );
                    cache.insert(
                        meta.digest,
                        CachedImage {
                            name: meta.name,
                            bytes,
                            exec_vmo,
                            persisted: true,
                        },
                    );
                }
                Err(e) => println!("[hirsel-stored] failed to hydrate `{}`: {e}", meta.name),
            }
        }
        (true, store.capacity_mib())
    } else {
        (false, 0)
    };

    let dir_backend = Arc::new(Mutex::new(DirBackend {
        disk: disk_store,
        mem_files: HashMap::new(),
    }));

    let auth_key = derive_cluster_key(DEFAULT_DEV_KEY_STR);
    let has_net = net_dev.is_some();
    let state = Arc::new(Mutex::new(NodeState {
        cache,
        dir_backend,
        auth_key,
        has_net,
        has_blk,
        disk_mib,
        tier1_runs: initial_t1,
        tier2_runs: initial_t2,
    }));

    // Spawn virtio-net + smoltcp TCP/IP server if NIC is present.
    if let Some(net) = net_dev {
        let ctx_net = Arc::clone(&ctx);
        let state_net = Arc::clone(&state);
        thread::spawn(move || serve_tcp_control_plane(ctx_net, state_net, net));
    }

    // Serve COM2 serial control plane on the main thread (encrypted dual-transport fallback).
    match Com2Uart::open(ctx.ioport_resource) {
        Ok(mut uart) => {
            println!(
                "[hirsel-os] Control Plane API ready on COM2 (0x2F8, mutual-auth ChaCha20-Poly1305)"
            );
            let mut session = SessionState::new();
            loop {
                uart.idle_poll = true;
                let frame = match read_frame_with_cipher(&mut uart, session.cipher_mut()) {
                    Ok(f) => f,
                    Err(e) => {
                        let resp = Response::Error {
                            message: format!("frame error: {e}"),
                        };
                        if let Ok(wire) = encode_session_response(&mut session, &resp, false) {
                            let _ = write_frame(&mut uart, &wire);
                        }
                        continue;
                    }
                };
                let (resp, force_plain) =
                    dispatch_session_frame(&ctx, &state, &mut session, frame.msg_type, frame.flags, &frame.payload);
                if let Ok(wire) = encode_session_response(&mut session, &resp, force_plain) {
                    let _ = write_frame(&mut uart, &wire);
                }
            }
        }
        Err(e) => {
            println!("[hirsel-os] COM2 unavailable ({e}); keeping TCP/root thread alive.");
            loop {
                let next = unsafe { zx_clock_get_monotonic() }.saturating_add(5_000_000_000);
                unsafe { zx_nanosleep(next) };
            }
        }
    }
}

fn slot_label(slot: u8) -> &'static str {
    match slot {
        0 => "A",
        1 => "B",
        _ => "none",
    }
}

fn encode_session_response(
    session: &mut SessionState,
    resp: &Response,
    force_plain: bool,
) -> Result<Vec<u8>, String> {
    if !force_plain {
        if let Some(c) = session.cipher_mut() {
            return resp.encode_encrypted(c);
        }
    }
    resp.encode()
}

fn smol_now() -> SmolInstant {
    let us = (unsafe { zx_clock_get_monotonic() } / 1000) as i64;
    SmolInstant::from_micros(us)
}

struct RawFrame {
    header_aad: [u8; 12],
    msg_type: u8,
    flags: u16,
    payload: Vec<u8>,
}

/// Attempts to parse one complete `HRSL` frame from the front of `buf`.
fn try_pop_frame(buf: &mut Vec<u8>) -> Option<Result<RawFrame, String>> {
    if let Some(pos) = buf.windows(4).position(|w| w == FRAME_MAGIC) {
        if pos > 0 {
            buf.drain(0..pos);
        }
    } else {
        if buf.len() > 3 {
            let keep = buf.len() - 3;
            buf.drain(0..keep);
        }
        return None;
    }

    if buf.len() < FRAME_HEADER_LEN {
        return None;
    }
    let version = buf[4];
    let msg_type = buf[5];
    let flags = u16::from_le_bytes(buf[6..8].try_into().unwrap());
    let payload_len = u32::from_le_bytes(buf[8..12].try_into().unwrap()) as usize;
    if version != PROTO_VERSION || payload_len > MAX_FRAME_PAYLOAD {
        buf.drain(0..4);
        return Some(Err(format!(
            "invalid frame header (version={version}, len={payload_len})"
        )));
    }
    let total = FRAME_HEADER_LEN + payload_len;
    if buf.len() < total {
        return None;
    }

    let expected_crc = u32::from_le_bytes(buf[12..16].try_into().unwrap());
    let actual_crc = crc32(&[&buf[0..12], &buf[FRAME_HEADER_LEN..total]]);
    let header_aad: [u8; 12] = buf[0..12].try_into().unwrap();
    let payload = buf[FRAME_HEADER_LEN..total].to_vec();
    buf.drain(0..total);
    if actual_crc != expected_crc {
        return Some(Err(format!(
            "CRC32 mismatch: expected {expected_crc:#010x}, got {actual_crc:#010x}"
        )));
    }
    Some(Ok(RawFrame {
        header_aad,
        msg_type,
        flags,
        payload,
    }))
}

fn serve_tcp_control_plane(
    ctx: Arc<BootContext>,
    state: Arc<Mutex<NodeState>>,
    mut net: VirtioNet,
) {
    let hw_addr = HardwareAddress::Ethernet(EthernetAddress(net.mac));
    let mut cfg = IfaceConfig::new(hw_addr);
    let mut seed_bytes = [0u8; 8];
    unsafe { zx_cprng_draw(seed_bytes.as_mut_ptr(), seed_bytes.len()) };
    cfg.random_seed = u64::from_le_bytes(seed_bytes);

    let mut iface = Interface::new(cfg, &mut net, smol_now());
    iface.update_ip_addrs(|addrs| {
        let _ = addrs.push(IpCidr::new(IpAddress::v4(10, 0, 2, 15), 24));
    });
    let _ = iface
        .routes_mut()
        .add_default_ipv4_route(Ipv4Address::new(10, 0, 2, 2));

    struct ConnSlot {
        handle: smoltcp::iface::SocketHandle,
        rx_accum: Vec<u8>,
        tx_queue: Vec<u8>,
        tx_offset: usize,
        session: SessionState,
    }

    const TCP_POOL_SIZE: usize = 4;
    let mut sockets = SocketSet::new(vec![]);
    let mut conns = Vec::with_capacity(TCP_POOL_SIZE);
    for _ in 0..TCP_POOL_SIZE {
        let rx_buf = tcp::SocketBuffer::new(vec![0u8; 64 * 1024]);
        let tx_buf = tcp::SocketBuffer::new(vec![0u8; 64 * 1024]);
        let mut sock = tcp::Socket::new(rx_buf, tx_buf);
        sock.set_nagle_enabled(false);
        sock.set_ack_delay(None);
        let _ = sock.listen(API_TCP_PORT);
        let handle = sockets.add(sock);
        conns.push(ConnSlot {
            handle,
            rx_accum: Vec::with_capacity(32 * 1024),
            tx_queue: Vec::new(),
            tx_offset: 0,
            session: SessionState::new(),
        });
    }

    println!(
        "[hirsel-netd] Control Plane TCP API listening on 10.0.2.15:{API_TCP_PORT} ({TCP_POOL_SIZE} slots, mutual-auth ChaCha20-Poly1305)"
    );

    let mut scratch = [0u8; 16 * 1024];

    loop {
        let now = smol_now();
        let poll_res = iface.poll(now, &mut net, &mut sockets);
        let mut active = !matches!(poll_res, smoltcp::iface::PollResult::None);

        for slot in &mut conns {
            let socket = sockets.get_mut::<tcp::Socket>(slot.handle);
            if !socket.is_open() {
                slot.rx_accum.clear();
                slot.tx_queue.clear();
                slot.tx_offset = 0;
                slot.session.reset();
                let _ = socket.listen(API_TCP_PORT);
                continue;
            }

            while socket.can_recv() {
                match socket.recv_slice(&mut scratch) {
                    Ok(n) if n > 0 => {
                        slot.rx_accum.extend_from_slice(&scratch[..n]);
                        active = true;
                    }
                    _ => break,
                }
            }

            while let Some(frame_res) = try_pop_frame(&mut slot.rx_accum) {
                active = true;
                let (resp, force_plain) = match frame_res {
                    Ok(raw) => {
                        let plaintext = if (raw.flags & FLAG_ENCRYPTED) != 0 {
                            match slot.session.cipher_mut() {
                                Some(c) => match c.open(&raw.header_aad, &raw.payload) {
                                    Ok(pt) => Ok(pt),
                                    Err(e) => Err(format!("AEAD auth error: {e}")),
                                },
                                None => Err("encrypted frame received before auth handshake".into()),
                            }
                        } else {
                            Ok(raw.payload)
                        };
                        match plaintext {
                            Ok(body) => dispatch_session_frame(
                                &ctx,
                                &state,
                                &mut slot.session,
                                raw.msg_type,
                                raw.flags,
                                &body,
                            ),
                            Err(e) => (Response::Error { message: e }, true),
                        }
                    }
                    Err(e) => (Response::Error { message: e }, true),
                };
                if let Ok(wire) = encode_session_response(&mut slot.session, &resp, force_plain) {
                    slot.tx_queue.extend_from_slice(&wire);
                }
            }

            if slot.tx_offset < slot.tx_queue.len() && socket.can_send() {
                if let Ok(sent) = socket.send_slice(&slot.tx_queue[slot.tx_offset..]) {
                    if sent > 0 {
                        slot.tx_offset += sent;
                        active = true;
                    }
                }
                if slot.tx_offset == slot.tx_queue.len() {
                    slot.tx_queue.clear();
                    slot.tx_offset = 0;
                }
            }

            if !socket.may_recv() && socket.may_send() && slot.tx_offset == slot.tx_queue.len() {
                socket.close();
                active = true;
            }
        }

        let _ = iface.poll(smol_now(), &mut net, &mut sockets);

        if !active {
            let next = unsafe { zx_clock_get_monotonic() }.saturating_add(250_000);
            unsafe { zx_nanosleep(next) };
        }
    }
}

/// Enforces mutual authentication + AEAD encryption before dispatching RPCs.
/// Returns `(Response, force_plaintext_reply)`.
fn dispatch_session_frame(
    ctx: &BootContext,
    state: &Mutex<NodeState>,
    session: &mut SessionState,
    msg_type: u8,
    flags: u16,
    body: &[u8],
) -> (Response, bool) {
    let req = match Request::decode_body(msg_type, body) {
        Ok(r) => r,
        Err(e) => return (Response::Error { message: e }, false),
    };

    // Always allow starting a fresh mutual-auth handshake (supports serial reconnects).
    match req {
        Request::AuthHello { client_nonce } => {
            let key = state.lock().unwrap().auth_key;
            let mut server_nonce = [0u8; 32];
            unsafe { zx_cprng_draw(server_nonce.as_mut_ptr(), server_nonce.len()) };
            let server_mac = compute_server_auth_mac(&key, &client_nonce, &server_nonce);
            session.pending = None;
            session.pending_upgrade = None;
            session.auth = AuthState::ChallengeSent {
                client_nonce,
                server_nonce,
            };
            return (
                Response::AuthChallenge {
                    server_nonce,
                    server_mac,
                },
                true,
            );
        }
        Request::AuthVerify { client_mac } => {
            let (client_nonce, server_nonce) = match session.auth {
                AuthState::ChallengeSent {
                    client_nonce,
                    server_nonce,
                } => (client_nonce, server_nonce),
                _ => {
                    session.reset();
                    return (
                        Response::Error {
                            message: "AuthVerify without preceding AuthHello".into(),
                        },
                        true,
                    );
                }
            };
            let key = state.lock().unwrap().auth_key;
            let expected = compute_client_auth_mac(&key, &client_nonce, &server_nonce);
            if !ct_eq(&expected, &client_mac) {
                session.reset();
                println!("[hirsel-api] REJECTED unauthenticated client (invalid cluster key MAC)");
                return (
                    Response::Error {
                        message: "authentication failed: invalid cluster key".into(),
                    },
                    true,
                );
            }
            let cipher = SessionCipher::new_server(&key, &client_nonce, &server_nonce);
            session.auth = AuthState::Authenticated(cipher);
            // AuthOk is sent plaintext; all subsequent frames use AEAD encryption.
            return (Response::AuthOk, true);
        }
        _ => {}
    }

    // All operational RPCs require an authenticated AEAD session.
    if !matches!(session.auth, AuthState::Authenticated(_)) {
        return (
            Response::Error {
                message: "authentication required: send AuthHello first".into(),
            },
            true,
        );
    }
    if (flags & FLAG_ENCRYPTED) == 0 {
        return (
            Response::Error {
                message: "plaintext operational frame rejected: ChaCha20-Poly1305 AEAD required"
                    .into(),
            },
            false,
        );
    }

    let resp = handle_request(ctx, state, session, req);
    (resp, false)
}

fn handle_request(
    ctx: &BootContext,
    state: &Mutex<NodeState>,
    session: &mut SessionState,
    req: Request,
) -> Response {
    match req {
        Request::AuthHello { .. } | Request::AuthVerify { .. } => unreachable!(),
        Request::Ping { nonce } => Response::Pong {
            nonce,
            mono_ns: unsafe { zx_clock_get_monotonic() } as u64,
        },
        Request::Info => {
            let mut entries: Vec<String> = ctx.bootfs_index.keys().cloned().collect();
            entries.sort();
            let st = state.lock().unwrap();
            let (persisted_images, active_slot, pending_slot) = {
                let db = st.dir_backend.lock().unwrap();
                match db.disk.as_ref() {
                    Some(d) => {
                        let up = d.upgrade_info();
                        (d.entry_count(), up.active_slot, up.pending_slot)
                    }
                    None => (0, 0, 0xFF),
                }
            };
            Response::Info(NodeInfo {
                mono_ns: unsafe { zx_clock_get_monotonic() } as u64,
                boot_ns: unsafe { zx_clock_get_boot() } as u64,
                has_vmex: ctx.vmex_resource != ZX_HANDLE_INVALID,
                has_hypervisor: ctx.hypervisor_resource != ZX_HANDLE_INVALID,
                has_net: st.has_net,
                has_blk: st.has_blk,
                auth_enabled: true,
                active_slot,
                pending_slot,
                disk_mib: st.disk_mib,
                persisted_images,
                cached_images: st.cache.len() as u32,
                tier1_runs: st.tier1_runs,
                tier2_runs: st.tier2_runs,
                bootfs_entries: entries,
            })
        }
        Request::ListImages => {
            let st = state.lock().unwrap();
            let mut list: Vec<ImageEntry> = st
                .cache
                .iter()
                .map(|(&digest, img)| ImageEntry {
                    digest,
                    name: img.name.clone(),
                    size: img.bytes.len() as u64,
                    persisted: img.persisted,
                })
                .collect();
            list.sort_by(|a, b| a.name.cmp(&b.name));
            Response::ImageList(list)
        }
        Request::HasImage { digest } => {
            let st = state.lock().unwrap();
            match st.cache.get(&digest) {
                Some(img) => Response::ImageStatus {
                    present: true,
                    size: img.bytes.len() as u64,
                },
                None => Response::ImageStatus {
                    present: false,
                    size: 0,
                },
            }
        }
        Request::PutImageStart {
            digest,
            total_size,
            name,
        } => {
            let max_size = 32 * 1024 * 1024u64;
            if total_size == 0 || total_size > max_size {
                return Response::Error {
                    message: format!("invalid image size {total_size}"),
                };
            }
            session.pending = Some(PendingUpload {
                digest,
                total_size: total_size as usize,
                name,
                buf: Vec::with_capacity(total_size as usize),
            });
            Response::Ok
        }
        Request::PutImageChunk { offset, data } => {
            let Some(up) = session.pending.as_mut() else {
                return Response::Error {
                    message: "no active image upload".into(),
                };
            };
            if offset as usize != up.buf.len() {
                return Response::Error {
                    message: format!(
                        "chunk offset mismatch: got {offset}, expected {}",
                        up.buf.len()
                    ),
                };
            }
            if up.buf.len() + data.len() > up.total_size {
                return Response::Error {
                    message: "chunk exceeds declared total_size".into(),
                };
            }
            up.buf.extend_from_slice(&data);
            Response::ChunkAck {
                received: up.buf.len() as u64,
            }
        }
        Request::PutImageFinish { digest } => {
            let Some(up) = session.pending.take() else {
                return Response::Error {
                    message: "no active image upload".into(),
                };
            };
            if up.digest != digest {
                return Response::Error {
                    message: "finish digest does not match upload start".into(),
                };
            }
            if up.buf.len() != up.total_size {
                return Response::Error {
                    message: format!(
                        "incomplete upload: got {}, expected {}",
                        up.buf.len(),
                        up.total_size
                    ),
                };
            }
            let actual = sha256(&up.buf);
            if actual != digest {
                return Response::Error {
                    message: format!(
                        "SHA-256 mismatch: expected {}, got {}",
                        hex_encode(&digest),
                        hex_encode(&actual)
                    ),
                };
            }
            let exec_vmo = match ctx.seal_executable_vmo(&up.name, &up.buf) {
                Ok(v) => v,
                Err(e) => return Response::Error { message: e },
            };
            let size = up.buf.len() as u64;
            let short = &hex_encode(&digest)[..12];

            let mut st = state.lock().unwrap();
            let persisted = {
                let mut db = st.dir_backend.lock().unwrap();
                if let Some(disk) = db.disk.as_mut() {
                    match disk.put_image(digest, &up.name, &up.buf) {
                        Ok(_) => true,
                        Err(e) => {
                            println!(
                                "[hirsel-stored] warning: persist failed for `{}`: {e}",
                                up.name
                            );
                            false
                        }
                    }
                } else {
                    false
                }
            };

            println!(
                "[hirsel-api] admitted immutable VMO `{}` (sha256:{short}, {size} bytes, persisted={persisted})",
                up.name
            );
            if let Some(old) = st.cache.insert(
                digest,
                CachedImage {
                    name: up.name,
                    bytes: up.buf,
                    exec_vmo,
                    persisted,
                },
            ) {
                unsafe { zx_handle_close(old.exec_vmo) };
            }
            Response::ImageStatus {
                present: true,
                size,
            }
        }
        Request::UpgradeStart { digest, total_size } => {
            let max_size = 16 * 1024 * 1024u64;
            if total_size < 32 || total_size > max_size {
                return Response::Error {
                    message: format!("invalid ZBI upgrade size {total_size}"),
                };
            }
            session.pending_upgrade = Some(PendingUpgrade {
                digest,
                total_size: total_size as usize,
                buf: Vec::with_capacity(total_size as usize),
            });
            Response::Ok
        }
        Request::UpgradeChunk { offset, data } => {
            let Some(up) = session.pending_upgrade.as_mut() else {
                return Response::Error {
                    message: "no active ZBI upgrade upload".into(),
                };
            };
            if offset as usize != up.buf.len() {
                return Response::Error {
                    message: format!(
                        "upgrade chunk offset mismatch: got {offset}, expected {}",
                        up.buf.len()
                    ),
                };
            }
            if up.buf.len() + data.len() > up.total_size {
                return Response::Error {
                    message: "upgrade chunk exceeds total_size".into(),
                };
            }
            up.buf.extend_from_slice(&data);
            Response::ChunkAck {
                received: up.buf.len() as u64,
            }
        }
        Request::UpgradeFinish { digest } => {
            let Some(up) = session.pending_upgrade.take() else {
                return Response::Error {
                    message: "no active ZBI upgrade upload".into(),
                };
            };
            if up.digest != digest || up.buf.len() != up.total_size {
                return Response::Error {
                    message: "incomplete or mismatched ZBI upgrade".into(),
                };
            }
            let dir_backend = Arc::clone(&state.lock().unwrap().dir_backend);
            let mut db = dir_backend.lock().unwrap();
            let Some(disk) = db.disk.as_mut() else {
                return Response::Error {
                    message: "A/B ZBI upgrade requires persistent virtio-blk disk".into(),
                };
            };
            match disk.stage_upgrade_zbi(digest, &up.buf) {
                Ok(info) => {
                    let short = &hex_encode(&info.staged_digest)[..12];
                    println!(
                        "[hirsel-upgrade] staged verified ZBI (sha256:{short}, {} bytes) to slot {}",
                        info.staged_size,
                        slot_label(info.pending_slot),
                    );
                    Response::UpgradeInfo(info)
                }
                Err(e) => Response::Error { message: e },
            }
        }
        Request::UpgradeStatus => {
            let dir_backend = Arc::clone(&state.lock().unwrap().dir_backend);
            let db = dir_backend.lock().unwrap();
            let info = match db.disk.as_ref() {
                Some(d) => d.upgrade_info(),
                None => UpgradeInfo {
                    active_slot: 0,
                    pending_slot: 0xFF,
                    staged_digest: [0u8; 32],
                    staged_size: 0,
                    boot_attempts: 0,
                },
            };
            Response::UpgradeInfo(info)
        }
        Request::RunTier1 {
            target,
            name,
            args,
            env,
            timeout_ms,
            grant_dir,
            grant_net,
        } => {
            let t0 = unsafe { zx_clock_get_monotonic() };
            let dir_backend = if grant_dir {
                Some(Arc::clone(&state.lock().unwrap().dir_backend))
            } else {
                None
            };
            let opts = Tier1LaunchOpts {
                args,
                env,
                timeout_ms,
                dir_backend,
                grant_net,
            };
            let res = match target {
                RunTarget::Bootfs(path) => ctx.run_tier1_job_with_opts(&path, opts),
                RunTarget::Digest(digest) => {
                    let cached = {
                        let st = state.lock().unwrap();
                        st.cache
                            .get(&digest)
                            .map(|img| (img.name.clone(), img.exec_vmo))
                    };
                    match cached {
                        Some((img_name, vmo)) => {
                            ctx.run_tier1_cached_vmo_with_opts(&img_name, vmo, opts)
                        }
                        None => Err(format!("image not in cache: sha256:{}", hex_encode(&digest))),
                    }
                }
            };
            match res {
                Ok((exit_code, stdout)) => {
                    let mut st = state.lock().unwrap();
                    st.tier1_runs += 1;
                    let elapsed_us = ((unsafe { zx_clock_get_monotonic() } - t0) / 1000) as u64;
                    println!(
                        "[hirsel-api] Tier 1 `{name}` exited code={exit_code} in {elapsed_us} us (dir={grant_dir}, net={grant_net})"
                    );
                    Response::RunResult(RunResult {
                        exit_code,
                        stdout,
                        elapsed_us,
                        io_exits: 0,
                        mmio_exits: 0,
                        bells: 0,
                    })
                }
                Err(e) => Response::Error { message: e },
            }
        }
        Request::RunTier2 {
            target,
            ram_mib,
            max_vm_exits,
            mode,
        } => {
            let cfg = hirsel_vmm::MicroVmConfig {
                ram_bytes: (ram_mib.max(2) as usize) * 1024 * 1024,
                max_vm_exits: if max_vm_exits == 0 {
                    200_000
                } else {
                    max_vm_exits
                },
            };
            let payload_res = match target {
                RunTarget::Bootfs(path) => ctx.read_bootfs_bytes(&path),
                RunTarget::Digest(digest) => {
                    let bytes = {
                        let st = state.lock().unwrap();
                        st.cache.get(&digest).map(|img| img.bytes.clone())
                    };
                    bytes.ok_or_else(|| {
                        format!("image not in cache: sha256:{}", hex_encode(&digest))
                    })
                }
            };
            let res = payload_res.and_then(|elf| match mode {
                Tier2Mode::RawElf => hirsel_vmm::run_elf_microvm(
                    ctx.hypervisor_resource,
                    ctx.vmex_resource,
                    &elf,
                    cfg,
                ),
                Tier2Mode::LinuxUserElf => {
                    let shim = ctx.read_bootfs_bytes(LINUX_SHIM_BOOTFS_PATH)?;
                    hirsel_vmm::run_linux_user_elf(
                        ctx.hypervisor_resource,
                        ctx.vmex_resource,
                        &shim,
                        &elf,
                        cfg,
                    )
                }
            });
            match res {
                Ok(report) => {
                    let mut st = state.lock().unwrap();
                    st.tier2_runs += 1;
                    println!(
                        "[hirsel-api] Tier 2 MicroVM ({mode:?}) exited code={} in {} us (io={}, mmio={}, bells={})",
                        report.exit_code,
                        report.elapsed_us,
                        report.io_exits,
                        report.mmio_exits,
                        report.bell_count,
                    );
                    Response::RunResult(RunResult {
                        exit_code: i64::from(report.exit_code),
                        stdout: report.stdout,
                        elapsed_us: report.elapsed_us,
                        io_exits: report.io_exits,
                        mmio_exits: report.mmio_exits,
                        bells: report.bell_count,
                    })
                }
                Err(e) => Response::Error { message: e },
            }
        }
    }
}
