//! `hirsel-os-ctl`: Host operator CLI for shell-less Hirsel-OS Zircon nodes.
//!
//! Authenticates mutually via HMAC-SHA256 challenge-response and encrypts all
//! RPC traffic with RFC 8439 ChaCha20-Poly1305 AEAD over TCP (`virtio-net`) or
//! Unix socket (COM2 serial fallback).

use hirsel_proto::crypto::{
    DEFAULT_DEV_KEY_STR, SessionCipher, compute_client_auth_mac, compute_server_auth_mac, ct_eq,
    derive_cluster_key,
};
use hirsel_proto::{
    DEFAULT_CHUNK_SIZE, Request, Response, RunTarget, Tier2Mode, hex_encode,
    read_frame_with_cipher, sha256, write_frame,
};
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::time::{Duration, Instant};

enum Stream {
    Tcp(TcpStream),
    Unix(UnixStream),
}

struct Transport {
    stream: Stream,
    cipher: Option<SessionCipher>,
}

fn fill_random_32() -> [u8; 32] {
    let mut buf = [0u8; 32];
    if let Ok(mut f) = fs::File::open("/dev/urandom") {
        let _ = f.read_exact(&mut buf);
    }
    buf
}

impl Transport {
    fn connect_and_auth(addr: &str, unix: Option<&str>, key_str: &str) -> Result<Self, String> {
        let stream = if let Some(path) = unix {
            let s = UnixStream::connect(path).map_err(|e| format!("unix connect: {e}"))?;
            s.set_read_timeout(Some(Duration::from_secs(60))).ok();
            s.set_write_timeout(Some(Duration::from_secs(60))).ok();
            Stream::Unix(s)
        } else {
            let s = TcpStream::connect(addr).map_err(|e| format!("tcp connect {addr}: {e}"))?;
            s.set_nodelay(true).ok();
            s.set_read_timeout(Some(Duration::from_secs(60))).ok();
            s.set_write_timeout(Some(Duration::from_secs(60))).ok();
            Stream::Tcp(s)
        };
        let mut tx = Self {
            stream,
            cipher: None,
        };
        tx.authenticate(key_str)?;
        Ok(tx)
    }

    fn send_raw(&mut self, wire: &[u8]) -> Result<(), String> {
        match &mut self.stream {
            Stream::Tcp(s) => write_frame(s, wire),
            Stream::Unix(s) => write_frame(s, wire),
        }
        .map_err(|e| format!("write request failed: {e}"))
    }

    fn recv_response(&mut self) -> Result<Response, String> {
        let frame = match &mut self.stream {
            Stream::Tcp(s) => read_frame_with_cipher(s, self.cipher.as_mut()),
            Stream::Unix(s) => read_frame_with_cipher(s, self.cipher.as_mut()),
        }
        .map_err(|e| format!("read response failed: {e}"))?;
        Response::decode_body(frame.msg_type, &frame.payload)
    }

    fn authenticate(&mut self, key_str: &str) -> Result<(), String> {
        let key = derive_cluster_key(key_str);
        let client_nonce = fill_random_32();
        let hello = Request::AuthHello { client_nonce }.encode()?;
        self.send_raw(&hello)?;
        let (server_nonce, server_mac) = match self.recv_response()? {
            Response::AuthChallenge {
                server_nonce,
                server_mac,
            } => (server_nonce, server_mac),
            Response::Error { message } => return Err(message),
            other => return Err(format!("unexpected AuthHello response: {other:?}")),
        };
        let expected_server_mac = compute_server_auth_mac(&key, &client_nonce, &server_nonce);
        if !ct_eq(&expected_server_mac, &server_mac) {
            return Err("server authentication failed: cluster key mismatch".into());
        }
        let client_mac = compute_client_auth_mac(&key, &client_nonce, &server_nonce);
        let verify = Request::AuthVerify { client_mac }.encode()?;
        self.send_raw(&verify)?;
        match self.recv_response()? {
            Response::AuthOk => {
                self.cipher = Some(SessionCipher::new_client(
                    &key,
                    &client_nonce,
                    &server_nonce,
                ));
                Ok(())
            }
            Response::Error { message } => Err(message),
            other => Err(format!("unexpected AuthVerify response: {other:?}")),
        }
    }

    fn call(&mut self, req: &Request) -> Result<Response, String> {
        let wire = match self.cipher.as_mut() {
            Some(c) => req.encode_encrypted(c)?,
            None => req.encode()?,
        };
        self.send_raw(&wire)?;
        self.recv_response()
    }
}

fn slot_label(slot: u8) -> &'static str {
    match slot {
        0 => "A",
        1 => "B",
        _ => "none",
    }
}

fn usage() {
    eprintln!(
        "Usage: hirsel-os-ctl [--addr 127.0.0.1:4444 | --unix PATH] [--key SECRET] <command>\n\
         \n\
         Commands:\n\
           ping                                         Check node liveness & AEAD RPC RTT\n\
           info                                         Show node capabilities, slots & image cache\n\
           images                                       List cached & persisted content-addressed images\n\
           upgrade <path-to-zbi>                        Stream & verify ZBI into inactive A/B boot slot\n\
           upgrade-status                               Show A/B boot slot state & staged ZBI digest\n\
           run [options] <path>                         Compile (if Hirsel dir), upload & run\n\
           run-bootfs [options] <bootfs-path>           Run a built-in BOOTFS image\n\
         \n\
         Run options:\n\
           --tier 1|2                                   Execution tier (default: 1)\n\
           --ram-mib N                                  Tier 2 MicroVM RAM in MiB (default: 8, or 128 for Linux kernel)\n\
           --linux-elf                                  Tier 2: boot unmodified Linux x86-64 static ELF in Ring 3\n\
           --linux-kernel                               Tier 2: boot real upstream Linux vmlinux/bzImage kernel\n\
           --initrd PATH                                Tier 2: attach cpio initramfs to Linux kernel microVM\n\
           --cmdline STRING                             Tier 2: kernel command line\n\
           --arg VALUE                                  Tier 1: pass positional arg (repeatable)\n\
           --env KEY=VAL                                Tier 1: pass environment variable (repeatable)\n\
           --timeout-ms MS                              Tier 1: wall-clock kill timeout in ms\n\
           --grant-dir                                  Tier 1: grant persistent Zircon-channel Dir capability\n\
           --grant-net                                  Tier 1: grant attenuated Zircon-channel Net capability"
    );
}

fn compile_hirsel_package(pkg_dir: &Path) -> Result<Vec<u8>, String> {
    let root = env::var("HIRSEL_OS_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/workspace/hirsel-os"));
    let hirsel_dir = env::var("HIRSEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("/workspace/hirsel"));

    let hirsel_bin = hirsel_dir.join("target/debug/hirsel");
    let cc = root.join("scripts/fuchsia-clang");
    let rt = root.join("target/hirsel-rt-fuchsia/x86_64-unknown-fuchsia/release/libhirsel_core_rt.a");
    let libs = hirsel_dir.join("core");
    let strip = root.join("third-party/fuchsia/prebuilt/third_party/clang/linux-x64/bin/llvm-strip");

    let out_path = env::temp_dir().join(format!("hirsel-ctl-{}.elf", process::id()));
    let _ = fs::remove_file(&out_path);

    eprintln!(
        "[hirsel-os-ctl] compiling Hirsel package {} -> x86_64-unknown-fuchsia...",
        pkg_dir.display()
    );
    let status = Command::new(&hirsel_bin)
        .arg("build")
        .arg(pkg_dir)
        .args(["--target", "x86_64-unknown-fuchsia"])
        .args(["--release", "--backend", "cranelift"])
        .arg("--cc")
        .arg(&cc)
        .arg("--runtime")
        .arg(&rt)
        .arg("--libraries")
        .arg(&libs)
        .args(["--source-core", "--source-std"])
        .arg("-o")
        .arg(&out_path)
        .status()
        .map_err(|e| format!("failed to launch {}: {e}", hirsel_bin.display()))?;
    if !status.success() {
        return Err(format!("hirsel build failed with {status}"));
    }
    if strip.is_file() {
        let _ = Command::new(&strip)
            .arg("--strip-all")
            .arg(&out_path)
            .status();
    }
    let bytes = fs::read(&out_path).map_err(|e| format!("read compiled ELF: {e}"))?;
    let _ = fs::remove_file(&out_path);
    Ok(bytes)
}

fn ensure_uploaded(tx: &mut Transport, name: &str, bytes: &[u8]) -> Result<[u8; 32], String> {
    let digest = sha256(bytes);
    let short = &hex_encode(&digest)[..12];

    match tx.call(&Request::HasImage { digest })? {
        Response::ImageStatus {
            present: true,
            size,
        } => {
            eprintln!(
                "[hirsel-os-ctl] cache HIT on node: sha256:{short} ({size} bytes, zero-copy VMO)"
            );
            return Ok(digest);
        }
        Response::ImageStatus { present: false, .. } => {}
        Response::Error { message } => return Err(message),
        other => return Err(format!("unexpected HasImage response: {other:?}")),
    }

    let t0 = Instant::now();
    eprintln!(
        "[hirsel-os-ctl] uploading {name} ({} bytes, sha256:{short}, AEAD encrypted)...",
        bytes.len()
    );
    match tx.call(&Request::PutImageStart {
        digest,
        total_size: bytes.len() as u64,
        name: name.to_owned(),
    })? {
        Response::Ok => {}
        Response::Error { message } => return Err(message),
        other => return Err(format!("unexpected PutImageStart response: {other:?}")),
    }

    let mut offset: usize = 0;
    while offset < bytes.len() {
        let end = (offset + DEFAULT_CHUNK_SIZE).min(bytes.len());
        let chunk = bytes[offset..end].to_vec();
        match tx.call(&Request::PutImageChunk {
            offset: offset as u64,
            data: chunk,
        })? {
            Response::ChunkAck { .. } => {}
            Response::Error { message } => return Err(message),
            other => return Err(format!("unexpected PutImageChunk response: {other:?}")),
        }
        offset = end;
    }

    match tx.call(&Request::PutImageFinish { digest })? {
        Response::ImageStatus { present: true, size } => {
            let ms = t0.elapsed().as_millis();
            eprintln!(
                "[hirsel-os-ctl] uploaded & verified sha256:{short} ({size} bytes in {ms} ms)"
            );
            Ok(digest)
        }
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected PutImageFinish response: {other:?}")),
    }
}

fn print_run_result(tier: u8, name: &str, r: hirsel_proto::RunResult) {
    let _ = io::stdout().write_all(&r.stdout);
    let _ = io::stdout().flush();
    if tier == 1 {
        eprintln!(
            "[hirsel-os-ctl] Tier 1 `{name}` exited code={} in {} us",
            r.exit_code, r.elapsed_us
        );
    } else {
        eprintln!(
            "[hirsel-os-ctl] Tier 2 `{name}` exited code={} in {} us (io_exits={}, mmio_exits={}, bells={})",
            r.exit_code, r.elapsed_us, r.io_exits, r.mmio_exits, r.bells
        );
    }
    if r.exit_code != 0 {
        process::exit(r.exit_code as i32);
    }
}

fn main() {
    let mut args: Vec<String> = env::args().skip(1).collect();
    let mut addr = String::from("127.0.0.1:4444");
    let mut unix: Option<String> = None;
    let mut key = env::var("HIRSEL_OS_KEY").unwrap_or_else(|_| DEFAULT_DEV_KEY_STR.to_owned());

    while args.len() >= 2 {
        if args[0] == "--addr" {
            addr = args.remove(1);
            args.remove(0);
        } else if args[0] == "--unix" {
            unix = Some(args.remove(1));
            args.remove(0);
        } else if args[0] == "--key" {
            key = args.remove(1);
            args.remove(0);
        } else {
            break;
        }
    }

    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        usage();
        process::exit(if args.is_empty() { 2 } else { 0 });
    }

    let cmd = args.remove(0);
    let mut tx = match Transport::connect_and_auth(&addr, unix.as_deref(), &key) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            process::exit(1);
        }
    };

    let res: Result<(), String> = match cmd.as_str() {
        "ping" => {
            let t0 = Instant::now();
            let nonce = 0x4852_534C_0000_0001u64;
            match tx.call(&Request::Ping { nonce }) {
                Ok(Response::Pong {
                    nonce: got,
                    mono_ns,
                }) => {
                    println!(
                        "pong (AEAD): nonce={got:#x} node_mono_ns={mono_ns} rtt={} us",
                        t0.elapsed().as_micros()
                    );
                    Ok(())
                }
                Ok(Response::Error { message }) => Err(message),
                Ok(other) => Err(format!("unexpected response: {other:?}")),
                Err(e) => Err(e),
            }
        }
        "info" => match tx.call(&Request::Info) {
            Ok(Response::Info(info)) => {
                println!("Hirsel-OS Zircon Node:");
                println!("  mono_ns:          {}", info.mono_ns);
                println!("  boot_ns:          {}", info.boot_ns);
                println!("  auth_aead:        {}", info.auth_enabled);
                println!(
                    "  ab_slots:         active={} pending={}",
                    slot_label(info.active_slot),
                    slot_label(info.pending_slot)
                );
                println!("  vmex:             {}", info.has_vmex);
                println!("  hypervisor:       {}", info.has_hypervisor);
                println!("  virtio_net:       {}", info.has_net);
                println!("  virtio_blk:       {} ({} MiB)", info.has_blk, info.disk_mib);
                println!("  persisted_images: {}", info.persisted_images);
                println!("  cached_images:    {}", info.cached_images);
                println!("  tier1_runs:       {}", info.tier1_runs);
                println!("  tier2_runs:       {}", info.tier2_runs);
                println!("  bootfs_entries:   {}", info.bootfs_entries.join(", "));
                Ok(())
            }
            Ok(Response::Error { message }) => Err(message),
            Ok(other) => Err(format!("unexpected response: {other:?}")),
            Err(e) => Err(e),
        },
        "images" => match tx.call(&Request::ListImages) {
            Ok(Response::ImageList(list)) => {
                if list.is_empty() {
                    println!("(no cached images)");
                } else {
                    for img in list {
                        println!(
                            "sha256:{}  {:>9} bytes  persisted={:<5}  {}",
                            &hex_encode(&img.digest)[..16],
                            img.size,
                            img.persisted,
                            img.name
                        );
                    }
                }
                Ok(())
            }
            Ok(Response::Error { message }) => Err(message),
            Ok(other) => Err(format!("unexpected response: {other:?}")),
            Err(e) => Err(e),
        },
        "upgrade-status" => match tx.call(&Request::UpgradeStatus) {
            Ok(Response::UpgradeInfo(u)) => {
                println!("A/B Boot Slot Status:");
                println!("  active_slot:   {}", slot_label(u.active_slot));
                println!("  pending_slot:  {}", slot_label(u.pending_slot));
                println!("  staged_size:   {} bytes", u.staged_size);
                println!("  staged_digest: sha256:{}", hex_encode(&u.staged_digest));
                println!("  boot_attempts: {}", u.boot_attempts);
                Ok(())
            }
            Ok(Response::Error { message }) => Err(message),
            Ok(other) => Err(format!("unexpected response: {other:?}")),
            Err(e) => Err(e),
        },
        "upgrade" => {
            let Some(zbi_path) = args.first() else {
                usage();
                process::exit(2);
            };
            let bytes = match fs::read(zbi_path) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("error: failed to read {zbi_path}: {e}");
                    process::exit(1);
                }
            };
            let digest = sha256(&bytes);
            let short = &hex_encode(&digest)[..12];
            let t0 = Instant::now();
            eprintln!(
                "[hirsel-os-ctl] streaming A/B ZBI upgrade {zbi_path} ({} bytes, sha256:{short})...",
                bytes.len()
            );
            let step = (|| -> Result<(), String> {
                match tx.call(&Request::UpgradeStart {
                    digest,
                    total_size: bytes.len() as u64,
                })? {
                    Response::Ok => {}
                    Response::Error { message } => return Err(message),
                    other => return Err(format!("unexpected UpgradeStart response: {other:?}")),
                }
                let mut offset = 0usize;
                while offset < bytes.len() {
                    let end = (offset + DEFAULT_CHUNK_SIZE).min(bytes.len());
                    match tx.call(&Request::UpgradeChunk {
                        offset: offset as u64,
                        data: bytes[offset..end].to_vec(),
                    })? {
                        Response::ChunkAck { .. } => {}
                        Response::Error { message } => return Err(message),
                        other => return Err(format!("unexpected UpgradeChunk response: {other:?}")),
                    }
                    offset = end;
                }
                match tx.call(&Request::UpgradeFinish { digest })? {
                    Response::UpgradeInfo(u) => {
                        println!(
                            "A/B ZBI upgrade staged in {} ms: active={} pending={} size={} sha256:{}",
                            t0.elapsed().as_millis(),
                            slot_label(u.active_slot),
                            slot_label(u.pending_slot),
                            u.staged_size,
                            &hex_encode(&u.staged_digest)[..16],
                        );
                        Ok(())
                    }
                    Response::Error { message } => Err(message),
                    other => Err(format!("unexpected UpgradeFinish response: {other:?}")),
                }
            })();
            step
        }
        "run" | "run-bootfs" => {
            let mut tier: u8 = 1;
            let mut ram_mib: u32 = 0;
            let mut linux_elf = false;
            let mut linux_kernel = false;
            let mut initrd_path: Option<String> = None;
            let mut cmdline = String::new();
            let mut job_args: Vec<String> = Vec::new();
            let mut job_env: Vec<String> = Vec::new();
            let mut timeout_ms: u64 = 0;
            let mut grant_dir = false;
            let mut grant_net = false;
            let mut target_arg: Option<String> = None;

            let mut i = 0;
            while i < args.len() {
                match args[i].as_str() {
                    "--tier" if i + 1 < args.len() => {
                        tier = args[i + 1].parse().unwrap_or(1);
                        i += 2;
                    }
                    "--ram-mib" if i + 1 < args.len() => {
                        ram_mib = args[i + 1].parse().unwrap_or(0);
                        i += 2;
                    }
                    "--linux-elf" => {
                        linux_elf = true;
                        tier = 2;
                        i += 1;
                    }
                    "--linux-kernel" => {
                        linux_kernel = true;
                        tier = 2;
                        i += 1;
                    }
                    "--initrd" if i + 1 < args.len() => {
                        initrd_path = Some(args[i + 1].clone());
                        i += 2;
                    }
                    "--cmdline" if i + 1 < args.len() => {
                        cmdline = args[i + 1].clone();
                        i += 2;
                    }
                    "--arg" if i + 1 < args.len() => {
                        job_args.push(args[i + 1].clone());
                        i += 2;
                    }
                    "--env" if i + 1 < args.len() => {
                        job_env.push(args[i + 1].clone());
                        i += 2;
                    }
                    "--timeout-ms" if i + 1 < args.len() => {
                        timeout_ms = args[i + 1].parse().unwrap_or(0);
                        i += 2;
                    }
                    "--grant-dir" => {
                        grant_dir = true;
                        i += 1;
                    }
                    "--grant-net" => {
                        grant_net = true;
                        i += 1;
                    }
                    s => {
                        target_arg = Some(s.to_owned());
                        i += 1;
                    }
                }
            }
            let Some(path_str) = target_arg else {
                usage();
                process::exit(2);
            };

            let (name, target) = if cmd == "run-bootfs" {
                (path_str.clone(), RunTarget::Bootfs(path_str))
            } else {
                let p = Path::new(&path_str);
                let (name, bytes) = if p.is_dir() && p.join("hirsel.toml").is_file() {
                    if tier != 1 {
                        eprintln!("error: Hirsel package source directories target Tier 1");
                        process::exit(2);
                    }
                    let nm = p
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("hirsel-app")
                        .to_owned();
                    let b = match compile_hirsel_package(p) {
                        Ok(b) => b,
                        Err(e) => {
                            eprintln!("error: {e}");
                            process::exit(1);
                        }
                    };
                    (nm, b)
                } else {
                    let nm = p
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("workload")
                        .to_owned();
                    let b = match fs::read(p) {
                        Ok(b) => b,
                        Err(e) => {
                            eprintln!("error: failed to read {}: {e}", p.display());
                            process::exit(1);
                        }
                    };
                    (nm, b)
                };
                match ensure_uploaded(&mut tx, &name, &bytes) {
                    Ok(digest) => (name, RunTarget::Digest(digest)),
                    Err(e) => {
                        eprintln!("error: {e}");
                        process::exit(1);
                    }
                }
            };

            let initrd_target = if let Some(rd_path) = initrd_path {
                let p = Path::new(&rd_path);
                let nm = p
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("initramfs.cpio")
                    .to_owned();
                let bytes = match fs::read(p) {
                    Ok(b) => b,
                    Err(e) => {
                        eprintln!("error: failed to read initrd {}: {e}", p.display());
                        process::exit(1);
                    }
                };
                match ensure_uploaded(&mut tx, &nm, &bytes) {
                    Ok(digest) => Some(RunTarget::Digest(digest)),
                    Err(e) => {
                        eprintln!("error: {e}");
                        process::exit(1);
                    }
                }
            } else {
                None
            };

            let req = if tier == 1 {
                Request::RunTier1 {
                    target,
                    name: name.clone(),
                    args: job_args,
                    env: job_env,
                    timeout_ms,
                    grant_dir,
                    grant_net,
                }
            } else {
                let mode = if linux_kernel {
                    Tier2Mode::LinuxKernel
                } else if linux_elf {
                    Tier2Mode::LinuxUserElf
                } else {
                    Tier2Mode::RawElf
                };
                let eff_ram = if ram_mib > 0 {
                    ram_mib
                } else if linux_kernel {
                    128
                } else if linux_elf {
                    16
                } else {
                    8
                };
                Request::RunTier2 {
                    target,
                    ram_mib: eff_ram,
                    max_vm_exits: if linux_kernel { 2_000_000 } else { 200_000 },
                    mode,
                    cmdline,
                    initrd: initrd_target,
                }
            };
            match tx.call(&req) {
                Ok(Response::RunResult(r)) => {
                    print_run_result(tier, &name, r);
                    Ok(())
                }
                Ok(Response::Error { message }) => Err(message),
                Ok(other) => Err(format!("unexpected response: {other:?}")),
                Err(e) => Err(e),
            }
        }
        other => Err(format!("unknown command `{other}`")),
    };

    if let Err(e) = res {
        eprintln!("error: {e}");
        process::exit(1);
    }
}
