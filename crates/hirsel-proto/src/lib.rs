//! Zero-dependency binary RPC wire protocol, CRC32, SHA-256, and RFC 8439
//! ChaCha20-Poly1305 AEAD transport shared between `hirsel-init` and `hirsel-os-ctl`.
//!
//! Frame layout (little-endian):
//! ```text
//!   [0..4]   magic       = b"HRSL"
//!   [4]      version     = 1
//!   [5]      msg_type    = u8
//!   [6..8]   flags       = u16 (bit 0 = FLAG_ENCRYPTED)
//!   [8..12]  payload_len = u32 (<= MAX_FRAME_PAYLOAD)
//!   [12..16] crc32       = IEEE CRC32 over header[0..12] || payload
//!   [16..]   payload     = [u8; payload_len] (AEAD ciphertext||tag if FLAG_ENCRYPTED)
//! ```

pub mod crypto;

use crate::crypto::SessionCipher;
use std::io::{self, Read, Write};

pub const FRAME_MAGIC: [u8; 4] = *b"HRSL";
pub const PROTO_VERSION: u8 = 1;
pub const FRAME_HEADER_LEN: usize = 16;
pub const MAX_FRAME_PAYLOAD: usize = 16 * 1024 * 1024;
pub const DEFAULT_CHUNK_SIZE: usize = 16 * 1024;
pub const FLAG_ENCRYPTED: u16 = 0x0001;

// Request message types (0x01..0x7F)
const REQ_PING: u8 = 0x01;
const REQ_INFO: u8 = 0x02;
const REQ_HAS_IMAGE: u8 = 0x03;
const REQ_PUT_IMAGE_START: u8 = 0x04;
const REQ_PUT_IMAGE_CHUNK: u8 = 0x05;
const REQ_PUT_IMAGE_FINISH: u8 = 0x06;
const REQ_RUN_TIER1: u8 = 0x07;
const REQ_RUN_TIER2: u8 = 0x08;
const REQ_AUTH_HELLO: u8 = 0x09;
const REQ_AUTH_VERIFY: u8 = 0x0A;
const REQ_LIST_IMAGES: u8 = 0x0B;
const REQ_UPGRADE_START: u8 = 0x0C;
const REQ_UPGRADE_CHUNK: u8 = 0x0D;
const REQ_UPGRADE_FINISH: u8 = 0x0E;
const REQ_UPGRADE_STATUS: u8 = 0x0F;

// Response message types (0x81..0xFF)
const RESP_PONG: u8 = 0x81;
const RESP_INFO: u8 = 0x82;
const RESP_IMAGE_STATUS: u8 = 0x83;
const RESP_OK: u8 = 0x84;
const RESP_CHUNK_ACK: u8 = 0x85;
const RESP_RUN_RESULT: u8 = 0x86;
const RESP_AUTH_CHALLENGE: u8 = 0x87;
const RESP_AUTH_OK: u8 = 0x88;
const RESP_IMAGE_LIST: u8 = 0x89;
const RESP_UPGRADE_INFO: u8 = 0x8A;
const RESP_ERROR: u8 = 0xFF;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RunTarget {
    /// Content-addressed SHA-256 digest of a previously uploaded image.
    Digest([u8; 32]),
    /// Path inside the immutable BOOTFS image (e.g. `bin/hirsel-hello`).
    Bootfs(String),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tier2Mode {
    /// Freestanding 64-bit Ring-0 ELF binary booted directly inside AMD SVM / Intel VMX.
    RawElf = 0,
    /// Unmodified Linux x86-64 static ELF booted in Ring 3 inside `bin/hirsel-linux-shim.elf`.
    LinuxUserElf = 1,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Request {
    AuthHello {
        client_nonce: [u8; 32],
    },
    AuthVerify {
        client_mac: [u8; 32],
    },
    Ping {
        nonce: u64,
    },
    Info,
    ListImages,
    HasImage {
        digest: [u8; 32],
    },
    PutImageStart {
        digest: [u8; 32],
        total_size: u64,
        name: String,
    },
    PutImageChunk {
        offset: u64,
        data: Vec<u8>,
    },
    PutImageFinish {
        digest: [u8; 32],
    },
    RunTier1 {
        target: RunTarget,
        name: String,
        args: Vec<String>,
        env: Vec<String>,
        timeout_ms: u64,
        grant_dir: bool,
        grant_net: bool,
    },
    RunTier2 {
        target: RunTarget,
        ram_mib: u32,
        max_vm_exits: u64,
        mode: Tier2Mode,
    },
    UpgradeStart {
        digest: [u8; 32],
        total_size: u64,
    },
    UpgradeChunk {
        offset: u64,
        data: Vec<u8>,
    },
    UpgradeFinish {
        digest: [u8; 32],
    },
    UpgradeStatus,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NodeInfo {
    pub mono_ns: u64,
    pub boot_ns: u64,
    pub has_vmex: bool,
    pub has_hypervisor: bool,
    pub has_net: bool,
    pub has_blk: bool,
    pub auth_enabled: bool,
    pub active_slot: u8,
    pub pending_slot: u8,
    pub disk_mib: u32,
    pub persisted_images: u32,
    pub cached_images: u32,
    pub tier1_runs: u64,
    pub tier2_runs: u64,
    pub bootfs_entries: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageEntry {
    pub digest: [u8; 32],
    pub name: String,
    pub size: u64,
    pub persisted: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UpgradeInfo {
    pub active_slot: u8,
    pub pending_slot: u8,
    pub staged_digest: [u8; 32],
    pub staged_size: u64,
    pub boot_attempts: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunResult {
    pub exit_code: i64,
    pub stdout: Vec<u8>,
    pub elapsed_us: u64,
    pub io_exits: u64,
    pub mmio_exits: u64,
    pub bells: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Response {
    AuthChallenge {
        server_nonce: [u8; 32],
        server_mac: [u8; 32],
    },
    AuthOk,
    Pong {
        nonce: u64,
        mono_ns: u64,
    },
    Info(NodeInfo),
    ImageList(Vec<ImageEntry>),
    ImageStatus {
        present: bool,
        size: u64,
    },
    Ok,
    ChunkAck {
        received: u64,
    },
    RunResult(RunResult),
    UpgradeInfo(UpgradeInfo),
    Error {
        message: String,
    },
}

/// Computes IEEE 802.3 CRC-32 (polynomial 0xEDB88320).
pub fn crc32(parts: &[&[u8]]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for part in parts {
        for &byte in *part {
            crc ^= u32::from(byte);
            for _ in 0..8 {
                let mask = (crc & 1).wrapping_neg();
                crc = (crc >> 1) ^ (0xEDB8_8320 & mask);
            }
        }
    }
    !crc
}

/// Pure-Rust FIPS 180-4 SHA-256 digest.
pub fn sha256(data: &[u8]) -> [u8; 32] {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab,
        0x5be0cd19,
    ];

    let bit_len = (data.len() as u64).wrapping_mul(8);
    let mut chunks = data.chunks_exact(64);
    for block in &mut chunks {
        compress_sha256(&mut h, block, &K);
    }
    let rem = chunks.remainder();
    let mut tail = [0u8; 128];
    tail[..rem.len()].copy_from_slice(rem);
    tail[rem.len()] = 0x80;
    let total_tail = if rem.len() < 56 { 64 } else { 128 };
    tail[total_tail - 8..total_tail].copy_from_slice(&bit_len.to_be_bytes());
    compress_sha256(&mut h, &tail[..64], &K);
    if total_tail == 128 {
        compress_sha256(&mut h, &tail[64..128], &K);
    }

    let mut out = [0u8; 32];
    for (i, word) in h.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn compress_sha256(h: &mut [u32; 8], block: &[u8], k: &[u32; 64]) {
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes(block[i * 4..(i + 1) * 4].try_into().unwrap());
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut hh] = *h;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ ((!e) & g);
        let t1 = hh
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(k[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);

        hh = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    h[0] = h[0].wrapping_add(a);
    h[1] = h[1].wrapping_add(b);
    h[2] = h[2].wrapping_add(c);
    h[3] = h[3].wrapping_add(d);
    h[4] = h[4].wrapping_add(e);
    h[5] = h[5].wrapping_add(f);
    h[6] = h[6].wrapping_add(g);
    h[7] = h[7].wrapping_add(hh);
}

pub fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut s = String::with_capacity(bytes.len() * 2);
    for &b in bytes {
        s.push(HEX[(b >> 4) as usize] as char);
        s.push(HEX[(b & 0x0f) as usize] as char);
    }
    s
}

struct Cursor<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(buf: &'a [u8]) -> Self {
        Self { buf, pos: 0 }
    }
    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| "frame decode overflow".to_string())?;
        if end > self.buf.len() {
            return Err("unexpected EOF in frame payload".into());
        }
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }
    fn u8(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, String> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn i64(&mut self) -> Result<i64, String> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn digest(&mut self) -> Result<[u8; 32], String> {
        Ok(self.take(32)?.try_into().unwrap())
    }
    fn bytes(&mut self) -> Result<Vec<u8>, String> {
        let len = self.u32()? as usize;
        Ok(self.take(len)?.to_vec())
    }
    fn string(&mut self) -> Result<String, String> {
        let raw = self.bytes()?;
        String::from_utf8(raw).map_err(|e| format!("invalid UTF-8: {e}"))
    }
    fn string_vec(&mut self) -> Result<Vec<String>, String> {
        let count = self.u32()? as usize;
        let mut out = Vec::with_capacity(count);
        for _ in 0..count {
            out.push(self.string()?);
        }
        Ok(out)
    }
    fn target(&mut self) -> Result<RunTarget, String> {
        match self.u8()? {
            0 => Ok(RunTarget::Digest(self.digest()?)),
            1 => Ok(RunTarget::Bootfs(self.string()?)),
            tag => Err(format!("unknown RunTarget tag {tag}")),
        }
    }
}

fn put_bytes(out: &mut Vec<u8>, data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
}

fn put_string(out: &mut Vec<u8>, s: &str) {
    put_bytes(out, s.as_bytes());
}

fn put_string_vec(out: &mut Vec<u8>, xs: &[String]) {
    out.extend_from_slice(&(xs.len() as u32).to_le_bytes());
    for s in xs {
        put_string(out, s);
    }
}

fn put_target(out: &mut Vec<u8>, t: &RunTarget) {
    match t {
        RunTarget::Digest(d) => {
            out.push(0);
            out.extend_from_slice(d);
        }
        RunTarget::Bootfs(p) => {
            out.push(1);
            put_string(out, p);
        }
    }
}

pub fn encode_frame_with_cipher(
    msg_type: u8,
    payload: &[u8],
    cipher: Option<&mut SessionCipher>,
) -> Result<Vec<u8>, String> {
    let flags = if cipher.is_some() { FLAG_ENCRYPTED } else { 0 };
    let wire_len = payload.len() + if cipher.is_some() { 16 } else { 0 };
    if wire_len > MAX_FRAME_PAYLOAD {
        return Err(format!("frame payload {wire_len} exceeds max"));
    }
    let mut header = [0u8; FRAME_HEADER_LEN];
    header[0..4].copy_from_slice(&FRAME_MAGIC);
    header[4] = PROTO_VERSION;
    header[5] = msg_type;
    header[6..8].copy_from_slice(&flags.to_le_bytes());
    header[8..12].copy_from_slice(&(wire_len as u32).to_le_bytes());

    let wire_payload = match cipher {
        Some(c) => c.seal(&header[0..12], payload),
        None => payload.to_vec(),
    };
    let sum = crc32(&[&header[0..12], &wire_payload]);
    header[12..16].copy_from_slice(&sum.to_le_bytes());

    let mut frame = Vec::with_capacity(FRAME_HEADER_LEN + wire_payload.len());
    frame.extend_from_slice(&header);
    frame.extend_from_slice(&wire_payload);
    Ok(frame)
}

pub fn encode_frame(msg_type: u8, payload: &[u8]) -> Result<Vec<u8>, String> {
    encode_frame_with_cipher(msg_type, payload, None)
}

impl Request {
    pub fn to_parts(&self) -> (u8, Vec<u8>) {
        let mut p = Vec::new();
        let ty = match self {
            Request::AuthHello { client_nonce } => {
                p.extend_from_slice(client_nonce);
                REQ_AUTH_HELLO
            }
            Request::AuthVerify { client_mac } => {
                p.extend_from_slice(client_mac);
                REQ_AUTH_VERIFY
            }
            Request::Ping { nonce } => {
                p.extend_from_slice(&nonce.to_le_bytes());
                REQ_PING
            }
            Request::Info => REQ_INFO,
            Request::ListImages => REQ_LIST_IMAGES,
            Request::HasImage { digest } => {
                p.extend_from_slice(digest);
                REQ_HAS_IMAGE
            }
            Request::PutImageStart {
                digest,
                total_size,
                name,
            } => {
                p.extend_from_slice(digest);
                p.extend_from_slice(&total_size.to_le_bytes());
                put_string(&mut p, name);
                REQ_PUT_IMAGE_START
            }
            Request::PutImageChunk { offset, data } => {
                p.extend_from_slice(&offset.to_le_bytes());
                put_bytes(&mut p, data);
                REQ_PUT_IMAGE_CHUNK
            }
            Request::PutImageFinish { digest } => {
                p.extend_from_slice(digest);
                REQ_PUT_IMAGE_FINISH
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
                put_target(&mut p, target);
                put_string(&mut p, name);
                put_string_vec(&mut p, args);
                put_string_vec(&mut p, env);
                p.extend_from_slice(&timeout_ms.to_le_bytes());
                p.push(u8::from(*grant_dir));
                p.push(u8::from(*grant_net));
                REQ_RUN_TIER1
            }
            Request::RunTier2 {
                target,
                ram_mib,
                max_vm_exits,
                mode,
            } => {
                put_target(&mut p, target);
                p.extend_from_slice(&ram_mib.to_le_bytes());
                p.extend_from_slice(&max_vm_exits.to_le_bytes());
                p.push(*mode as u8);
                REQ_RUN_TIER2
            }
            Request::UpgradeStart { digest, total_size } => {
                p.extend_from_slice(digest);
                p.extend_from_slice(&total_size.to_le_bytes());
                REQ_UPGRADE_START
            }
            Request::UpgradeChunk { offset, data } => {
                p.extend_from_slice(&offset.to_le_bytes());
                put_bytes(&mut p, data);
                REQ_UPGRADE_CHUNK
            }
            Request::UpgradeFinish { digest } => {
                p.extend_from_slice(digest);
                REQ_UPGRADE_FINISH
            }
            Request::UpgradeStatus => REQ_UPGRADE_STATUS,
        };
        (ty, p)
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let (ty, p) = self.to_parts();
        encode_frame(ty, &p)
    }

    pub fn encode_encrypted(&self, cipher: &mut SessionCipher) -> Result<Vec<u8>, String> {
        let (ty, p) = self.to_parts();
        encode_frame_with_cipher(ty, &p, Some(cipher))
    }

    pub fn decode_body(msg_type: u8, payload: &[u8]) -> Result<Self, String> {
        let mut c = Cursor::new(payload);
        match msg_type {
            REQ_AUTH_HELLO => Ok(Request::AuthHello {
                client_nonce: c.digest()?,
            }),
            REQ_AUTH_VERIFY => Ok(Request::AuthVerify {
                client_mac: c.digest()?,
            }),
            REQ_PING => Ok(Request::Ping { nonce: c.u64()? }),
            REQ_INFO => Ok(Request::Info),
            REQ_LIST_IMAGES => Ok(Request::ListImages),
            REQ_HAS_IMAGE => Ok(Request::HasImage { digest: c.digest()? }),
            REQ_PUT_IMAGE_START => Ok(Request::PutImageStart {
                digest: c.digest()?,
                total_size: c.u64()?,
                name: c.string()?,
            }),
            REQ_PUT_IMAGE_CHUNK => Ok(Request::PutImageChunk {
                offset: c.u64()?,
                data: c.bytes()?,
            }),
            REQ_PUT_IMAGE_FINISH => Ok(Request::PutImageFinish { digest: c.digest()? }),
            REQ_RUN_TIER1 => Ok(Request::RunTier1 {
                target: c.target()?,
                name: c.string()?,
                args: c.string_vec()?,
                env: c.string_vec()?,
                timeout_ms: c.u64()?,
                grant_dir: c.u8()? != 0,
                grant_net: c.u8()? != 0,
            }),
            REQ_RUN_TIER2 => {
                let target = c.target()?;
                let ram_mib = c.u32()?;
                let max_vm_exits = c.u64()?;
                let mode = match c.u8()? {
                    0 => Tier2Mode::RawElf,
                    1 => Tier2Mode::LinuxUserElf,
                    other => return Err(format!("unknown Tier2Mode {other}")),
                };
                Ok(Request::RunTier2 {
                    target,
                    ram_mib,
                    max_vm_exits,
                    mode,
                })
            }
            REQ_UPGRADE_START => Ok(Request::UpgradeStart {
                digest: c.digest()?,
                total_size: c.u64()?,
            }),
            REQ_UPGRADE_CHUNK => Ok(Request::UpgradeChunk {
                offset: c.u64()?,
                data: c.bytes()?,
            }),
            REQ_UPGRADE_FINISH => Ok(Request::UpgradeFinish { digest: c.digest()? }),
            REQ_UPGRADE_STATUS => Ok(Request::UpgradeStatus),
            other => Err(format!("unknown Request type {other:#x}")),
        }
    }
}

impl Response {
    pub fn to_parts(&self) -> (u8, Vec<u8>) {
        let mut p = Vec::new();
        let ty = match self {
            Response::AuthChallenge {
                server_nonce,
                server_mac,
            } => {
                p.extend_from_slice(server_nonce);
                p.extend_from_slice(server_mac);
                RESP_AUTH_CHALLENGE
            }
            Response::AuthOk => RESP_AUTH_OK,
            Response::Pong { nonce, mono_ns } => {
                p.extend_from_slice(&nonce.to_le_bytes());
                p.extend_from_slice(&mono_ns.to_le_bytes());
                RESP_PONG
            }
            Response::Info(info) => {
                p.extend_from_slice(&info.mono_ns.to_le_bytes());
                p.extend_from_slice(&info.boot_ns.to_le_bytes());
                p.push(u8::from(info.has_vmex));
                p.push(u8::from(info.has_hypervisor));
                p.push(u8::from(info.has_net));
                p.push(u8::from(info.has_blk));
                p.push(u8::from(info.auth_enabled));
                p.push(info.active_slot);
                p.push(info.pending_slot);
                p.extend_from_slice(&info.disk_mib.to_le_bytes());
                p.extend_from_slice(&info.persisted_images.to_le_bytes());
                p.extend_from_slice(&info.cached_images.to_le_bytes());
                p.extend_from_slice(&info.tier1_runs.to_le_bytes());
                p.extend_from_slice(&info.tier2_runs.to_le_bytes());
                put_string_vec(&mut p, &info.bootfs_entries);
                RESP_INFO
            }
            Response::ImageList(list) => {
                p.extend_from_slice(&(list.len() as u32).to_le_bytes());
                for item in list {
                    p.extend_from_slice(&item.digest);
                    put_string(&mut p, &item.name);
                    p.extend_from_slice(&item.size.to_le_bytes());
                    p.push(u8::from(item.persisted));
                }
                RESP_IMAGE_LIST
            }
            Response::ImageStatus { present, size } => {
                p.push(u8::from(*present));
                p.extend_from_slice(&size.to_le_bytes());
                RESP_IMAGE_STATUS
            }
            Response::Ok => RESP_OK,
            Response::ChunkAck { received } => {
                p.extend_from_slice(&received.to_le_bytes());
                RESP_CHUNK_ACK
            }
            Response::RunResult(r) => {
                p.extend_from_slice(&r.exit_code.to_le_bytes());
                put_bytes(&mut p, &r.stdout);
                p.extend_from_slice(&r.elapsed_us.to_le_bytes());
                p.extend_from_slice(&r.io_exits.to_le_bytes());
                p.extend_from_slice(&r.mmio_exits.to_le_bytes());
                p.extend_from_slice(&r.bells.to_le_bytes());
                RESP_RUN_RESULT
            }
            Response::UpgradeInfo(u) => {
                p.push(u.active_slot);
                p.push(u.pending_slot);
                p.extend_from_slice(&u.staged_digest);
                p.extend_from_slice(&u.staged_size.to_le_bytes());
                p.extend_from_slice(&u.boot_attempts.to_le_bytes());
                RESP_UPGRADE_INFO
            }
            Response::Error { message } => {
                put_string(&mut p, message);
                RESP_ERROR
            }
        };
        (ty, p)
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let (ty, p) = self.to_parts();
        encode_frame(ty, &p)
    }

    pub fn encode_encrypted(&self, cipher: &mut SessionCipher) -> Result<Vec<u8>, String> {
        let (ty, p) = self.to_parts();
        encode_frame_with_cipher(ty, &p, Some(cipher))
    }

    pub fn decode_body(msg_type: u8, payload: &[u8]) -> Result<Self, String> {
        let mut c = Cursor::new(payload);
        match msg_type {
            RESP_AUTH_CHALLENGE => Ok(Response::AuthChallenge {
                server_nonce: c.digest()?,
                server_mac: c.digest()?,
            }),
            RESP_AUTH_OK => Ok(Response::AuthOk),
            RESP_PONG => Ok(Response::Pong {
                nonce: c.u64()?,
                mono_ns: c.u64()?,
            }),
            RESP_INFO => Ok(Response::Info(NodeInfo {
                mono_ns: c.u64()?,
                boot_ns: c.u64()?,
                has_vmex: c.u8()? != 0,
                has_hypervisor: c.u8()? != 0,
                has_net: c.u8()? != 0,
                has_blk: c.u8()? != 0,
                auth_enabled: c.u8()? != 0,
                active_slot: c.u8()?,
                pending_slot: c.u8()?,
                disk_mib: c.u32()?,
                persisted_images: c.u32()?,
                cached_images: c.u32()?,
                tier1_runs: c.u64()?,
                tier2_runs: c.u64()?,
                bootfs_entries: c.string_vec()?,
            })),
            RESP_IMAGE_LIST => {
                let count = c.u32()? as usize;
                let mut list = Vec::with_capacity(count);
                for _ in 0..count {
                    list.push(ImageEntry {
                        digest: c.digest()?,
                        name: c.string()?,
                        size: c.u64()?,
                        persisted: c.u8()? != 0,
                    });
                }
                Ok(Response::ImageList(list))
            }
            RESP_IMAGE_STATUS => Ok(Response::ImageStatus {
                present: c.u8()? != 0,
                size: c.u64()?,
            }),
            RESP_OK => Ok(Response::Ok),
            RESP_CHUNK_ACK => Ok(Response::ChunkAck { received: c.u64()? }),
            RESP_RUN_RESULT => Ok(Response::RunResult(RunResult {
                exit_code: c.i64()?,
                stdout: c.bytes()?,
                elapsed_us: c.u64()?,
                io_exits: c.u64()?,
                mmio_exits: c.u64()?,
                bells: c.u64()?,
            })),
            RESP_UPGRADE_INFO => Ok(Response::UpgradeInfo(UpgradeInfo {
                active_slot: c.u8()?,
                pending_slot: c.u8()?,
                staged_digest: c.digest()?,
                staged_size: c.u64()?,
                boot_attempts: c.u32()?,
            })),
            RESP_ERROR => Ok(Response::Error {
                message: c.string()?,
            }),
            other => Err(format!("unknown Response type {other:#x}")),
        }
    }
}

/// Decoded frame metadata + plaintext payload.
pub struct DecodedFrame {
    pub msg_type: u8,
    pub flags: u16,
    pub payload: Vec<u8>,
}

/// Reads and verifies one framed message from `r`, optionally decrypting if `FLAG_ENCRYPTED`.
pub fn read_frame_with_cipher<R: Read>(
    r: &mut R,
    cipher: Option<&mut SessionCipher>,
) -> io::Result<DecodedFrame> {
    let mut matched = 0usize;
    let mut byte = [0u8; 1];
    while matched < 4 {
        r.read_exact(&mut byte)?;
        if byte[0] == FRAME_MAGIC[matched] {
            matched += 1;
        } else if byte[0] == FRAME_MAGIC[0] {
            matched = 1;
        } else {
            matched = 0;
        }
    }
    let mut rest = [0u8; FRAME_HEADER_LEN - 4];
    r.read_exact(&mut rest)?;
    let mut header = [0u8; FRAME_HEADER_LEN];
    header[0..4].copy_from_slice(&FRAME_MAGIC);
    header[4..16].copy_from_slice(&rest);

    let version = header[4];
    if version != PROTO_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unsupported protocol version {version}"),
        ));
    }
    let msg_type = header[5];
    let flags = u16::from_le_bytes(header[6..8].try_into().unwrap());
    let payload_len = u32::from_le_bytes(header[8..12].try_into().unwrap()) as usize;
    if payload_len > MAX_FRAME_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("frame payload {payload_len} exceeds limit"),
        ));
    }
    let expected_crc = u32::from_le_bytes(header[12..16].try_into().unwrap());
    let mut payload = vec![0u8; payload_len];
    if payload_len > 0 {
        r.read_exact(&mut payload)?;
    }
    let actual_crc = crc32(&[&header[0..12], &payload]);
    if actual_crc != expected_crc {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("CRC32 mismatch: expected {expected_crc:#010x}, got {actual_crc:#010x}"),
        ));
    }
    let plain = if (flags & FLAG_ENCRYPTED) != 0 {
        let Some(c) = cipher else {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "received encrypted frame without active session cipher",
            ));
        };
        c.open(&header[0..12], &payload)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
    } else {
        payload
    };
    Ok(DecodedFrame {
        msg_type,
        flags,
        payload: plain,
    })
}

pub fn read_frame<R: Read>(r: &mut R) -> io::Result<(u8, Vec<u8>)> {
    let f = read_frame_with_cipher(r, None)?;
    Ok((f.msg_type, f.payload))
}

pub fn write_frame<W: Write>(w: &mut W, frame: &[u8]) -> io::Result<()> {
    w.write_all(frame)?;
    w.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::{
        client_auth_mac, ct_eq_32, derive_cluster_key, derive_session_keys, server_auth_mac,
    };

    #[test]
    fn sha256_known_vector() {
        assert_eq!(
            hex_encode(&sha256(b"abc")),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn crc32_known_vector() {
        assert_eq!(crc32(&[b"123456789"]), 0xCBF4_3926);
    }

    #[test]
    fn encrypted_session_roundtrip() {
        let key = derive_cluster_key(crypto::DEFAULT_DEV_KEY_STR);
        let cn = sha256(b"client-nonce");
        let sn = sha256(b"server-nonce");
        let smac = server_auth_mac(&key, &cn, &sn);
        let cmac = client_auth_mac(&key, &cn, &sn);
        assert!(ct_eq_32(&smac, &server_auth_mac(&key, &cn, &sn)));
        assert!(ct_eq_32(&cmac, &client_auth_mac(&key, &cn, &sn)));

        let (c2s, s2c) = derive_session_keys(&key, &cn, &sn);
        let mut cli = SessionCipher::new(c2s, s2c);
        let mut srv = SessionCipher::new(s2c, c2s);

        let req = Request::RunTier1 {
            target: RunTarget::Digest(sha256(b"elf")),
            name: "app".into(),
            args: vec!["--flag".into(), "42".into()],
            env: vec!["MODE=prod".into()],
            timeout_ms: 5000,
            grant_dir: true,
            grant_net: true,
        };
        let wire = req.encode_encrypted(&mut cli).unwrap();
        let mut slice = wire.as_slice();
        let dec = read_frame_with_cipher(&mut slice, Some(&mut srv)).unwrap();
        assert_eq!(dec.flags & FLAG_ENCRYPTED, FLAG_ENCRYPTED);
        assert_eq!(Request::decode_body(dec.msg_type, &dec.payload).unwrap(), req);
    }
}
