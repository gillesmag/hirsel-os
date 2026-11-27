//! Operator CLI subcommands (`hirsel-os-ctl vm ...`) for managing persistent
//! Tier 2 Linux MicroVMs, declarative VM specs, networking, logs, and SSH access.

use crate::{Transport, ensure_uploaded};
use hirsel_proto::{
    PortForward, Request, Response, RunTarget, VmNetSpec, VmSpec, VmStatus, VmSummary,
};
use std::env;
use std::fs;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{self, Command};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

fn parse_ipv4(s: &str) -> Result<[u8; 4], String> {
    let parts: Vec<&str> = s.trim().split('.').collect();
    if parts.len() != 4 {
        return Err(format!("invalid IPv4 address `{s}`"));
    }
    let mut out = [0u8; 4];
    for (i, p) in parts.iter().enumerate() {
        out[i] = p
            .parse::<u8>()
            .map_err(|_| format!("invalid IPv4 octet `{p}` in `{s}`"))?;
    }
    Ok(out)
}

fn parse_cidr(s: &str) -> Result<([u8; 4], u8), String> {
    if let Some((ip_s, pfx_s)) = s.split_once('/') {
        let ip = parse_ipv4(ip_s)?;
        let pfx = pfx_s
            .parse::<u8>()
            .map_err(|_| format!("invalid prefix `{pfx_s}` in `{s}`"))?;
        Ok((ip, pfx))
    } else {
        Ok((parse_ipv4(s)?, 24))
    }
}

fn parse_port_forward(s: &str) -> Result<PortForward, String> {
    let (h, g) = s
        .split_once(':')
        .ok_or_else(|| format!("expected HOST_PORT:GUEST_PORT, got `{s}`"))?;
    let host_port = h
        .trim()
        .parse::<u16>()
        .map_err(|_| format!("invalid host port `{h}`"))?;
    let guest_port = g
        .trim()
        .parse::<u16>()
        .map_err(|_| format!("invalid guest port `{g}`"))?;
    Ok(PortForward {
        host_port,
        guest_port,
    })
}

fn parse_mac(s: &str) -> Result<[u8; 6], String> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if parts.len() != 6 {
        return Err(format!("invalid MAC address `{s}`"));
    }
    let mut out = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        out[i] = u8::from_str_radix(p, 16)
            .map_err(|_| format!("invalid MAC byte `{p}` in `{s}`"))?;
    }
    Ok(out)
}

#[derive(Default)]
struct FileVmSpec {
    name: Option<String>,
    kernel: Option<String>,
    initrd: Option<String>,
    ram_mib: Option<u32>,
    vcpus: Option<u8>,
    disk: Option<String>,
    image: Option<String>,
    autostart: bool,
    cmdline: Option<String>,
    guest_ip: Option<String>,
    gateway_ip: Option<String>,
    mac: Option<String>,
    ssh_port: Option<u16>,
    ports: Vec<PortForward>,
    no_net: bool,
}

fn parse_spec_file(path: &Path) -> Result<FileVmSpec, String> {
    let text = fs::read_to_string(path)
        .map_err(|e| format!("failed to read VM spec {}: {e}", path.display()))?;
    let mut spec = FileVmSpec::default();
    for (lineno, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with("//") {
            continue;
        }
        let Some((k, v_raw)) = line.split_once('=') else {
            return Err(format!(
                "{}:{}: expected `key = value`",
                path.display(),
                lineno + 1
            ));
        };
        let key = k.trim();
        let val = v_raw.trim().trim_matches('"').trim_matches('\'').to_owned();
        match key {
            "name" => spec.name = Some(val),
            "kernel" => spec.kernel = Some(val),
            "initrd" => spec.initrd = Some(val),
            "ram_mib" => {
                spec.ram_mib = Some(
                    val.parse::<u32>()
                        .map_err(|_| format!("invalid ram_mib `{val}`"))?,
                )
            }
            "vcpus" => {
                spec.vcpus = Some(
                    val.parse::<u8>()
                        .map_err(|_| format!("invalid vcpus `{val}`"))?,
                )
            }
            "disk" => spec.disk = Some(val),
            "image" => spec.image = Some(val),
            "autostart" => spec.autostart = val == "true" || val == "1",
            "cmdline" => spec.cmdline = Some(val),
            "guest_ip" => spec.guest_ip = Some(val),
            "gateway_ip" => spec.gateway_ip = Some(val),
            "mac" => spec.mac = Some(val),
            "ssh_port" => {
                spec.ssh_port = Some(
                    val.parse::<u16>()
                        .map_err(|_| format!("invalid ssh_port `{val}`"))?,
                )
            }
            "port" => spec.ports.push(parse_port_forward(&val)?),
            "ports" => {
                for item in val.split(',').map(str::trim).filter(|s| !s.is_empty()) {
                    spec.ports.push(parse_port_forward(item)?);
                }
            }
            "no_net" => spec.no_net = val == "true" || val == "1",
            other => {
                return Err(format!(
                    "{}:{}: unknown VM spec field `{other}`",
                    path.display(),
                    lineno + 1
                ));
            }
        }
    }
    Ok(spec)
}

fn fmt_ip(ip: [u8; 4]) -> String {
    format!("{}.{}.{}.{}", ip[0], ip[1], ip[2], ip[3])
}

fn fmt_mac(m: [u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        m[0], m[1], m[2], m[3], m[4], m[5]
    )
}

fn print_summary_detail(s: &VmSummary) {
    println!("VM `{}` ({}):", s.id, s.status.as_str());
    println!("  vcpus:      {}", s.vcpus);
    println!("  ram_mib:    {} MiB", s.ram_mib);
    println!(
        "  disk:       {} (blk reads={} writes={})",
        if s.disk.is_empty() { "none" } else { &s.disk },
        s.blk_reads,
        s.blk_writes
    );
    println!("  autostart:  {}", s.autostart);
    println!("  uptime_ms:  {} ms", s.uptime_ms);
    println!("  exit_code:  {}", s.exit_code);
    if !s.error.is_empty() {
        println!("  error:      {}", s.error);
    }
    println!("  vm_exits:   io={} mmio={}", s.io_exits, s.mmio_exits);
    if let Some(ref n) = s.net {
        let ports: Vec<String> = n
            .ports
            .iter()
            .map(|p| format!("{}:{}", p.host_port, p.guest_port))
            .collect();
        println!(
            "  network:    guest={}/{} gw={} mac={} ports=[{}]",
            fmt_ip(n.guest_ip),
            n.prefix_len,
            fmt_ip(n.gateway_ip),
            fmt_mac(n.mac),
            ports.join(", ")
        );
        println!(
            "  net_pkts:   rx={} tx={}",
            s.net_rx_packets, s.net_tx_packets
        );
    } else {
        println!("  network:    disabled");
    }
}

pub fn vm_usage() {
    eprintln!(
        "Usage: hirsel-os-ctl [global-opts] vm <subcommand>\n\
         \n\
         Subcommands:\n\
           vm start [options]              Start a persistent managed Linux MicroVM\n\
             --spec FILE                   Load declarative VM spec file (key = value)\n\
             --name ID                     VM name/ID (default: auto-assigned)\n\
             --kernel PATH                 Linux x86-64 vmlinux/bzImage path\n\
             --initrd PATH                 cpio initramfs path\n\
             --ram-mib N                   Guest RAM in MiB (default: 128)\n\
             --vcpus N                     Number of vCPUs 1..8 (default: 1)\n\
             --disk VOLUME                 Attach persistent named volume as /dev/vda\n\
             --image OCI_IMAGE             Build ext4 from Docker/OCI image & attach as /dev/vda\n\
             --autostart                   Persist spec in HRSLFS01 & auto-start on cold boot\n\
             --cmdline STRING              Kernel command line\n\
             --guest-ip IP[/PFX]           Guest eth0 IPv4/CIDR (default: 10.0.3.2/24)\n\
             --gateway-ip IP               Virtual gateway IPv4 (default: 10.0.3.1)\n\
             --mac AA:BB:CC:DD:EE:FF       Guest eth0 MAC address\n\
             --ssh-port PORT               Forward node TCP PORT -> guest :22 (default: 2222, 0=off)\n\
             --port HOST:GUEST             Add extra TCP port forward (repeatable)\n\
             --no-net                      Disable Virtio-Net networking\n\
           vm ls                           List managed VMs\n\
           vm inspect <id>                 Show VM spec, network, disk & live counters\n\
           vm logs <id> [--offset N] [-f]  Read or follow live COM1 serial console logs\n\
           vm console <id>                 Interactive raw-TTY serial console (Ctrl-] to detach)\n\
           vm exec <id> [--timeout-ms MS] -- <cmd...>\n\
                                           Execute command via virtio-console /dev/hvc0 (works without network!)\n\
           vm send <id> <text>             Send text + newline to VM serial console\n\
           vm ssh <id> [-i KEY] [-- CMD]   Open an SSH session directly into the running VM\n\
           vm stop <id>                    Stop a running VM and free its resources/ports\n\
           vm rm <id>                      Remove a stopped VM record & persisted autostart spec"
    );
}

/// Exports a Docker/OCI container image into a standalone ext4 filesystem image using `mkfs.ext4 -d`.
fn build_ext4_from_oci(image: &str, size_mib: u32) -> Result<Vec<u8>, String> {
    let tmp = env::temp_dir().join(format!("hirsel-oci-{}", process::id()));
    let rootfs = tmp.join("rootfs");
    let img_path = tmp.join("rootfs.ext4");
    let _ = fs::remove_dir_all(&tmp);
    fs::create_dir_all(&rootfs).map_err(|e| format!("mkdir {}: {e}", rootfs.display()))?;

    eprintln!("[hirsel-os-ctl] exporting OCI image `{image}` into {size_mib} MiB ext4...");
    let create_out = Command::new("docker")
        .args(["create", image])
        .output()
        .map_err(|e| format!("failed to run docker create: {e}"))?;
    if !create_out.status.success() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!(
            "docker create `{image}` failed: {}",
            String::from_utf8_lossy(&create_out.stderr)
        ));
    }
    let cid = String::from_utf8_lossy(&create_out.stdout).trim().to_owned();
    let tar_path = tmp.join("rootfs.tar");
    let export_status = Command::new("docker")
        .args(["export", "-o", tar_path.to_str().unwrap(), &cid])
        .status()
        .map_err(|e| format!("docker export failed: {e}"))?;
    let _ = Command::new("docker").args(["rm", "-f", &cid]).output();
    if !export_status.success() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!("docker export failed with {export_status}"));
    }

    let untar = Command::new("tar")
        .args(["-xf", tar_path.to_str().unwrap(), "-C", rootfs.to_str().unwrap()])
        .status()
        .map_err(|e| format!("tar extract failed: {e}"))?;
    if !untar.success() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!("tar extract failed with {untar}"));
    }

    let mkfs_bin = if Path::new("/usr/sbin/mkfs.ext4").is_file() {
        "/usr/sbin/mkfs.ext4"
    } else {
        "mkfs.ext4"
    };
    let mkfs = Command::new(mkfs_bin)
        .args([
            "-q",
            "-F",
            "-d",
            rootfs.to_str().unwrap(),
            img_path.to_str().unwrap(),
            &format!("{size_mib}M"),
        ])
        .status()
        .map_err(|e| format!("mkfs.ext4 failed: {e}"))?;
    if !mkfs.success() {
        let _ = fs::remove_dir_all(&tmp);
        return Err(format!("mkfs.ext4 exited with {mkfs}"));
    }

    let bytes = fs::read(&img_path).map_err(|e| format!("read ext4 image: {e}"))?;
    let _ = fs::remove_dir_all(&tmp);
    Ok(bytes)
}

pub fn handle_vol_command(tx: &mut Transport, mut args: Vec<String>) -> Result<(), String> {
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        eprintln!(
            "Usage: hirsel-os-ctl vol <subcommand>\n\
             \n\
             Subcommands:\n\
               vol create <name> <size-mib> [--from-file PATH | --from-oci IMAGE]\n\
               vol ls\n\
               vol rm <name>"
        );
        return Ok(());
    }
    let sub = args.remove(0);
    match sub.as_str() {
        "create" => {
            if args.len() < 2 {
                return Err(
                    "usage: vol create <name> <size-mib> [--from-file PATH | --from-oci IMAGE]"
                        .into(),
                );
            }
            let name = args.remove(0);
            let size_mib = args
                .remove(0)
                .parse::<u32>()
                .map_err(|_| "invalid size-mib".to_string())?;
            let mut from_file: Option<String> = None;
            let mut from_oci: Option<String> = None;
            let mut i = 0;
            while i < args.len() {
                match args[i].as_str() {
                    "--from-file" if i + 1 < args.len() => {
                        from_file = Some(args[i + 1].clone());
                        i += 2;
                    }
                    "--from-oci" if i + 1 < args.len() => {
                        from_oci = Some(args[i + 1].clone());
                        i += 2;
                    }
                    other => return Err(format!("unknown option `{other}` for `vol create`")),
                }
            }
            let initial_image = if let Some(path) = from_file {
                let bytes = fs::read(&path).map_err(|e| format!("read {path}: {e}"))?;
                let d = ensure_uploaded(tx, &format!("vol-{name}"), &bytes)?;
                Some(RunTarget::Digest(d))
            } else if let Some(image) = from_oci {
                let bytes = build_ext4_from_oci(&image, size_mib)?;
                let d = ensure_uploaded(tx, &format!("oci-{name}"), &bytes)?;
                Some(RunTarget::Digest(d))
            } else {
                None
            };
            match tx.call(&Request::VolumeCreate {
                name,
                size_mib,
                initial_image,
            })? {
                Response::VolumeInfo(v) => {
                    println!(
                        "Volume `{}` created: {} MiB ({} sectors @ {})",
                        v.name, v.size_mib, v.sectors, v.start_sector
                    );
                    Ok(())
                }
                Response::Error { message } => Err(message),
                other => Err(format!("unexpected VolumeCreate response: {other:?}")),
            }
        }
        "ls" | "list" => match tx.call(&Request::VolumeList)? {
            Response::VolumeList(list) => {
                if list.is_empty() {
                    println!("(no persistent volumes)");
                } else {
                    println!("{:<24} {:>8} {:>10} {:>12}", "NAME", "SIZE", "SECTORS", "START_SECTOR");
                    for v in list {
                        println!(
                            "{:<24} {:>6}MiB {:>10} {:>12}",
                            v.name, v.size_mib, v.sectors, v.start_sector
                        );
                    }
                }
                Ok(())
            }
            Response::Error { message } => Err(message),
            other => Err(format!("unexpected VolumeList response: {other:?}")),
        },
        "rm" | "remove" => {
            let name = args.first().ok_or("usage: vol rm <name>")?.clone();
            match tx.call(&Request::VolumeRemove { name: name.clone() })? {
                Response::Ok => {
                    println!("Volume `{name}` removed");
                    Ok(())
                }
                Response::Error { message } => Err(message),
                other => Err(format!("unexpected VolumeRemove response: {other:?}")),
            }
        }
        other => Err(format!("unknown vol subcommand `{other}`")),
    }
}

pub fn handle_vm_command(
    tx: &mut Transport,
    node_addr: &str,
    unix: Option<&str>,
    key: &str,
    mut args: Vec<String>,
) -> Result<(), String> {
    if args.is_empty() || args[0] == "-h" || args[0] == "--help" {
        vm_usage();
        return Ok(());
    }
    let sub = args.remove(0);
    match sub.as_str() {
        "start" => cmd_vm_start(tx, args),
        "ls" | "list" => cmd_vm_ls(tx),
        "inspect" => {
            let id = args.first().ok_or("usage: vm inspect <id>")?;
            match tx.call(&Request::VmInspect { id: id.clone() })? {
                Response::VmInfo(s) => {
                    print_summary_detail(&s);
                    Ok(())
                }
                Response::Error { message } => Err(message),
                other => Err(format!("unexpected response: {other:?}")),
            }
        }
        "logs" => cmd_vm_logs(tx, args),
        "console" => cmd_vm_console(tx, node_addr, unix, key, args),
        "exec" => cmd_vm_exec(tx, args),
        "send" => {
            if args.len() < 2 {
                return Err("usage: vm send <id> <text>".into());
            }
            let id = args.remove(0);
            let mut text = args.join(" ");
            text.push('\n');
            match tx.call(&Request::VmConsoleWrite {
                id,
                data: text.into_bytes(),
            })? {
                Response::Ok => Ok(()),
                Response::Error { message } => Err(message),
                other => Err(format!("unexpected response: {other:?}")),
            }
        }
        "ssh" => cmd_vm_ssh(tx, node_addr, args),
        "stop" => {
            let id = args.first().ok_or("usage: vm stop <id>")?;
            match tx.call(&Request::VmStop { id: id.clone() })? {
                Response::VmInfo(s) => {
                    println!(
                        "VM `{}` stopped (status={}, exit_code={}, uptime={} ms)",
                        s.id,
                        s.status.as_str(),
                        s.exit_code,
                        s.uptime_ms
                    );
                    Ok(())
                }
                Response::Error { message } => Err(message),
                other => Err(format!("unexpected response: {other:?}")),
            }
        }
        "rm" | "remove" => {
            let id = args.first().ok_or("usage: vm rm <id>")?;
            match tx.call(&Request::VmRemove { id: id.clone() })? {
                Response::Ok => {
                    println!("VM `{id}` removed");
                    Ok(())
                }
                Response::Error { message } => Err(message),
                other => Err(format!("unexpected response: {other:?}")),
            }
        }
        other => Err(format!("unknown vm subcommand `{other}` (run `vm --help`)")),
    }
}

fn cmd_vm_start(tx: &mut Transport, args: Vec<String>) -> Result<(), String> {
    let mut spec_file: Option<PathBuf> = None;
    let mut name: Option<String> = None;
    let mut kernel_path: Option<String> = None;
    let mut initrd_path: Option<String> = None;
    let mut ram_mib: Option<u32> = None;
    let mut vcpus: Option<u8> = None;
    let mut disk: Option<String> = None;
    let mut image: Option<String> = None;
    let mut autostart = false;
    let mut cmdline: Option<String> = None;
    let mut guest_ip_str: Option<String> = None;
    let mut gateway_ip_str: Option<String> = None;
    let mut mac_str: Option<String> = None;
    let mut ssh_port: Option<u16> = None;
    let mut extra_ports: Vec<PortForward> = Vec::new();
    let mut no_net = false;

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--spec" if i + 1 < args.len() => {
                spec_file = Some(PathBuf::from(&args[i + 1]));
                i += 2;
            }
            "--name" if i + 1 < args.len() => {
                name = Some(args[i + 1].clone());
                i += 2;
            }
            "--kernel" if i + 1 < args.len() => {
                kernel_path = Some(args[i + 1].clone());
                i += 2;
            }
            "--initrd" if i + 1 < args.len() => {
                initrd_path = Some(args[i + 1].clone());
                i += 2;
            }
            "--ram-mib" if i + 1 < args.len() => {
                ram_mib = Some(
                    args[i + 1]
                        .parse::<u32>()
                        .map_err(|_| format!("invalid --ram-mib `{}`", args[i + 1]))?,
                );
                i += 2;
            }
            "--vcpus" if i + 1 < args.len() => {
                vcpus = Some(
                    args[i + 1]
                        .parse::<u8>()
                        .map_err(|_| format!("invalid --vcpus `{}`", args[i + 1]))?,
                );
                i += 2;
            }
            "--disk" if i + 1 < args.len() => {
                disk = Some(args[i + 1].clone());
                i += 2;
            }
            "--image" if i + 1 < args.len() => {
                image = Some(args[i + 1].clone());
                i += 2;
            }
            "--autostart" => {
                autostart = true;
                i += 1;
            }
            "--cmdline" if i + 1 < args.len() => {
                cmdline = Some(args[i + 1].clone());
                i += 2;
            }
            "--guest-ip" if i + 1 < args.len() => {
                guest_ip_str = Some(args[i + 1].clone());
                i += 2;
            }
            "--gateway-ip" | "--gw-ip" if i + 1 < args.len() => {
                gateway_ip_str = Some(args[i + 1].clone());
                i += 2;
            }
            "--mac" if i + 1 < args.len() => {
                mac_str = Some(args[i + 1].clone());
                i += 2;
            }
            "--ssh-port" if i + 1 < args.len() => {
                ssh_port = Some(
                    args[i + 1]
                        .parse::<u16>()
                        .map_err(|_| format!("invalid --ssh-port `{}`", args[i + 1]))?,
                );
                i += 2;
            }
            "--port" if i + 1 < args.len() => {
                extra_ports.push(parse_port_forward(&args[i + 1])?);
                i += 2;
            }
            "--no-net" => {
                no_net = true;
                i += 1;
            }
            other => return Err(format!("unknown option `{other}` for `vm start`")),
        }
    }

    let file_spec = match spec_file {
        Some(ref p) => parse_spec_file(p)?,
        None => FileVmSpec::default(),
    };

    let eff_name = name.or(file_spec.name).unwrap_or_default();
    let eff_kernel = kernel_path
        .or(file_spec.kernel)
        .ok_or("missing --kernel PATH (or `kernel = ...` in --spec)")?;
    let eff_initrd = initrd_path.or(file_spec.initrd);
    let eff_ram = ram_mib.or(file_spec.ram_mib).unwrap_or(128);
    let eff_vcpus = vcpus.or(file_spec.vcpus).unwrap_or(1).clamp(1, 8);
    let eff_autostart = autostart || file_spec.autostart;
    let eff_cmdline = cmdline.or(file_spec.cmdline).unwrap_or_default();
    let eff_no_net = no_net || file_spec.no_net;

    // Optional OCI image -> ext4 volume creation.
    let mut eff_disk = disk.or(file_spec.disk);
    if let Some(oci_ref) = image.or(file_spec.image) {
        let vol_name = eff_disk.clone().unwrap_or_else(|| {
            if eff_name.is_empty() {
                "oci-rootfs".into()
            } else {
                format!("{eff_name}-root")
            }
        });
        let ext4_bytes = build_ext4_from_oci(&oci_ref, 32)?;
        let digest = ensure_uploaded(tx, &format!("oci-{vol_name}"), &ext4_bytes)?;
        match tx.call(&Request::VolumeCreate {
            name: vol_name.clone(),
            size_mib: 32,
            initial_image: Some(RunTarget::Digest(digest)),
        })? {
            Response::VolumeInfo(v) => {
                eprintln!(
                    "[hirsel-os-ctl] created OCI volume `{}` ({} MiB)",
                    v.name, v.size_mib
                );
            }
            Response::Error { message } if message.contains("already exists") => {
                eprintln!("[hirsel-os-ctl] reusing existing persistent volume `{vol_name}`");
            }
            Response::Error { message } => return Err(message),
            other => return Err(format!("unexpected VolumeCreate response: {other:?}")),
        }
        eff_disk = Some(vol_name);
    }

    let net = if eff_no_net {
        None
    } else {
        let (guest_ip, prefix_len) = match guest_ip_str.or(file_spec.guest_ip) {
            Some(s) => parse_cidr(&s)?,
            None => ([10, 0, 3, 2], 24),
        };
        let gateway_ip = match gateway_ip_str.or(file_spec.gateway_ip) {
            Some(s) => parse_ipv4(&s)?,
            None => [guest_ip[0], guest_ip[1], guest_ip[2], 1],
        };
        let mac = match mac_str.or(file_spec.mac) {
            Some(s) => parse_mac(&s)?,
            None => [0u8; 6],
        };
        let mut ports = file_spec.ports;
        ports.extend(extra_ports);
        let eff_ssh = ssh_port.or(file_spec.ssh_port).unwrap_or(2222);
        if eff_ssh != 0 && !ports.iter().any(|p| p.host_port == eff_ssh) {
            ports.insert(
                0,
                PortForward {
                    host_port: eff_ssh,
                    guest_port: 22,
                },
            );
        }
        Some(VmNetSpec {
            guest_ip,
            gateway_ip,
            prefix_len,
            mac,
            ports,
        })
    };

    // Upload kernel & optional initrd (content-addressed, 0-byte cache HIT if already on node).
    let kp = Path::new(&eff_kernel);
    let kname = kp
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("vmlinux")
        .to_owned();
    let kbytes = fs::read(kp).map_err(|e| format!("failed to read kernel {}: {e}", kp.display()))?;
    let kdigest = ensure_uploaded(tx, &kname, &kbytes)?;

    let initrd_target = match eff_initrd {
        Some(rd_path) => {
            let rp = Path::new(&rd_path);
            let rname = rp
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("initramfs.cpio")
                .to_owned();
            let rbytes =
                fs::read(rp).map_err(|e| format!("failed to read initrd {}: {e}", rp.display()))?;
            let rdigest = ensure_uploaded(tx, &rname, &rbytes)?;
            Some(RunTarget::Digest(rdigest))
        }
        None => None,
    };

    let spec = VmSpec {
        name: eff_name,
        kernel: RunTarget::Digest(kdigest),
        initrd: initrd_target,
        ram_mib: eff_ram,
        vcpus: eff_vcpus,
        cmdline: eff_cmdline,
        net,
        disk: eff_disk,
        autostart: eff_autostart,
    };

    match tx.call(&Request::VmStart(spec))? {
        Response::VmInfo(s) => {
            print_summary_detail(&s);
            Ok(())
        }
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected VmStart response: {other:?}")),
    }
}

fn cmd_vm_ls(tx: &mut Transport) -> Result<(), String> {
    match tx.call(&Request::VmList)? {
        Response::VmList(list) => {
            if list.is_empty() {
                println!("(no managed VMs)");
                return Ok(());
            }
            println!(
                "{:<14} {:<8} {:>4} {:>5} {:<10} {:<15} {:<11} {:>7}",
                "ID", "STATE", "CPU", "RAM", "DISK", "GUEST_IP", "PORTS", "UPTIME"
            );
            for vm in list {
                let (ip, ports) = match vm.net.as_ref() {
                    Some(n) => (
                        format!("{}/{}", fmt_ip(n.guest_ip), n.prefix_len),
                        n.ports
                            .iter()
                            .map(|p| format!("{}:{}", p.host_port, p.guest_port))
                            .collect::<Vec<_>>()
                            .join(","),
                    ),
                    None => ("-".into(), "-".into()),
                };
                println!(
                    "{:<14} {:<8} {:>4} {:>4}M {:<10} {:<15} {:<11} {:>5}ms",
                    vm.id,
                    vm.status.as_str(),
                    vm.vcpus,
                    vm.ram_mib,
                    if vm.disk.is_empty() { "-" } else { &vm.disk },
                    ip,
                    ports,
                    vm.uptime_ms,
                );
            }
            Ok(())
        }
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected VmList response: {other:?}")),
    }
}

fn cmd_vm_exec(tx: &mut Transport, args: Vec<String>) -> Result<(), String> {
    let mut id: Option<String> = None;
    let mut timeout_ms: u64 = 15_000;
    let mut cmd_parts: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--timeout-ms" if i + 1 < args.len() => {
                timeout_ms = args[i + 1].parse().unwrap_or(15_000);
                i += 2;
            }
            "--" => {
                cmd_parts.extend_from_slice(&args[i + 1..]);
                break;
            }
            s if id.is_none() => {
                id = Some(s.to_owned());
                i += 1;
            }
            s => {
                cmd_parts.push(s.to_owned());
                i += 1;
            }
        }
    }
    let id = id.ok_or("usage: vm exec <id> [--timeout-ms MS] -- <command...>")?;
    if cmd_parts.is_empty() {
        return Err("missing command for `vm exec`".into());
    }
    let command = cmd_parts.join(" ");
    match tx.call(&Request::VmExec {
        id,
        command,
        timeout_ms,
    })? {
        Response::ExecResult { exit_code, stdout } => {
            let _ = io::stdout().write_all(&stdout);
            let _ = io::stdout().flush();
            if exit_code != 0 {
                process::exit(exit_code);
            }
            Ok(())
        }
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected VmExec response: {other:?}")),
    }
}

struct SttyGuard(Option<String>);

impl SttyGuard {
    fn enter_raw() -> Self {
        let saved = Command::new("stty")
            .arg("-g")
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
        if saved.is_some() {
            let _ = Command::new("stty").args(["raw", "-echo"]).status();
        }
        Self(saved)
    }
}

impl Drop for SttyGuard {
    fn drop(&mut self) {
        if let Some(ref state) = self.0 {
            let _ = Command::new("stty").arg(state).status();
        }
    }
}

fn cmd_vm_console(
    tx: &mut Transport,
    node_addr: &str,
    unix: Option<&str>,
    key: &str,
    args: Vec<String>,
) -> Result<(), String> {
    let id = args
        .first()
        .ok_or("usage: vm console <id>")?
        .clone();

    // Verify VM exists and fetch current console tail offset.
    let start_offset = match tx.call(&Request::VmLogs {
        id: id.clone(),
        offset: 0,
        max_bytes: 64 * 1024,
    })? {
        Response::VmLogs {
            data,
            next_offset,
            status,
        } => {
            if !data.is_empty() {
                let _ = io::stdout().write_all(&data);
                let _ = io::stdout().flush();
            }
            if status != VmStatus::Running {
                return Err(format!("VM `{id}` is not running (status={})", status.as_str()));
            }
            next_offset
        }
        Response::Error { message } => return Err(message),
        other => return Err(format!("unexpected VmLogs response: {other:?}")),
    };

    eprintln!("\r\n[hirsel-os-ctl] attached to VM `{id}` console (press Ctrl-] to detach)\r\n");
    let _raw = SttyGuard::enter_raw();
    let stop = Arc::new(AtomicBool::new(false));
    let stop_r = Arc::clone(&stop);

    // Background log follower on a dedicated authenticated PFS transport.
    let addr_c = node_addr.to_owned();
    let unix_c = unix.map(str::to_owned);
    let key_c = key.to_owned();
    let id_c = id.clone();
    let reader_thread = thread::spawn(move || {
        let Ok(mut rtx) = Transport::connect_and_auth(&addr_c, unix_c.as_deref(), &key_c) else {
            return;
        };
        let mut offset = start_offset;
        while !stop_r.load(Ordering::Acquire) {
            match rtx.call(&Request::VmLogs {
                id: id_c.clone(),
                offset,
                max_bytes: 16 * 1024,
            }) {
                Ok(Response::VmLogs {
                    data,
                    next_offset,
                    status,
                }) => {
                    if !data.is_empty() {
                        let _ = io::stdout().write_all(&data);
                        let _ = io::stdout().flush();
                        offset = next_offset;
                        continue;
                    }
                    if status != VmStatus::Running {
                        stop_r.store(true, Ordering::Release);
                        break;
                    }
                }
                _ => break,
            }
            thread::sleep(Duration::from_millis(25));
        }
    });

    // Forward stdin bytes to VM COM1 until Ctrl-] (0x1D) or EOF.
    let mut stdin = io::stdin().lock();
    let mut byte = [0u8; 1];
    while !stop.load(Ordering::Acquire) {
        match stdin.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => {
                if byte[0] == 0x1D {
                    break;
                }
                let _ = tx.call(&Request::VmConsoleWrite {
                    id: id.clone(),
                    data: vec![byte[0]],
                });
            }
            Err(_) => break,
        }
    }

    stop.store(true, Ordering::Release);
    let _ = reader_thread.join();
    drop(_raw);
    eprintln!("\n[hirsel-os-ctl] detached from VM `{id}` console");
    Ok(())
}

fn cmd_vm_logs(tx: &mut Transport, args: Vec<String>) -> Result<(), String> {
    let mut id: Option<String> = None;
    let mut offset: u64 = 0;
    let mut follow = false;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--offset" if i + 1 < args.len() => {
                offset = args[i + 1].parse().unwrap_or(0);
                i += 2;
            }
            "-f" | "--follow" => {
                follow = true;
                i += 1;
            }
            s => {
                id = Some(s.to_owned());
                i += 1;
            }
        }
    }
    let id = id.ok_or("usage: vm logs <id> [--offset N] [-f|--follow]")?;

    loop {
        match tx.call(&Request::VmLogs {
            id: id.clone(),
            offset,
            max_bytes: 32 * 1024,
        })? {
            Response::VmLogs {
                data,
                next_offset,
                status,
            } => {
                if !data.is_empty() {
                    let _ = io::stdout().write_all(&data);
                    let _ = io::stdout().flush();
                    offset = next_offset;
                    continue;
                }
                if !follow || status != VmStatus::Running {
                    break;
                }
                thread::sleep(Duration::from_millis(100));
            }
            Response::Error { message } => return Err(message),
            other => return Err(format!("unexpected VmLogs response: {other:?}")),
        }
    }
    Ok(())
}

fn cmd_vm_ssh(tx: &mut Transport, node_addr: &str, args: Vec<String>) -> Result<(), String> {
    let mut id: Option<String> = None;
    let mut identity: Option<String> = None;
    let mut user = String::from("root");
    let mut remote_cmd: Vec<String> = Vec::new();

    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-i" | "--identity" if i + 1 < args.len() => {
                identity = Some(args[i + 1].clone());
                i += 2;
            }
            "-u" | "--user" if i + 1 < args.len() => {
                user = args[i + 1].clone();
                i += 2;
            }
            "--" => {
                remote_cmd.extend_from_slice(&args[i + 1..]);
                break;
            }
            s if id.is_none() => {
                id = Some(s.to_owned());
                i += 1;
            }
            s => {
                remote_cmd.push(s.to_owned());
                i += 1;
            }
        }
    }
    let id = id.ok_or("usage: vm ssh <id> [-i KEY] [-u USER] [-- <command...>]")?;

    let summary = match tx.call(&Request::VmInspect { id: id.clone() })? {
        Response::VmInfo(s) => s,
        Response::Error { message } => return Err(message),
        other => return Err(format!("unexpected VmInspect response: {other:?}")),
    };
    if summary.status != VmStatus::Running {
        return Err(format!("VM `{id}` is not running (status={})", summary.status.as_str()));
    }
    let ssh_port = summary
        .net
        .as_ref()
        .and_then(|n| n.ports.iter().find(|p| p.guest_port == 22).map(|p| p.host_port))
        .ok_or_else(|| format!("VM `{id}` has no forwarded SSH port (guest :22)"))?;

    let host = node_addr
        .rsplit_once(':')
        .map(|(h, _)| h)
        .unwrap_or("127.0.0.1");

    let mut cmd = Command::new("ssh");
    cmd.args([
        "-o",
        "StrictHostKeyChecking=no",
        "-o",
        "UserKnownHostsFile=/dev/null",
        "-o",
        "LogLevel=ERROR",
        "-p",
        &ssh_port.to_string(),
    ]);
    if let Some(ref key) = identity {
        cmd.arg("-i").arg(key);
    } else if Path::new("out/linux/id_ed25519").is_file() {
        cmd.arg("-i").arg("out/linux/id_ed25519");
    }
    cmd.arg(format!("{user}@{host}"));
    for arg in remote_cmd {
        cmd.arg(arg);
    }
    let status = cmd
        .status()
        .map_err(|e| format!("failed to launch ssh: {e}"))?;
    if !status.success() {
        return Err(format!("ssh exited with {status}"));
    }
    Ok(())
}
