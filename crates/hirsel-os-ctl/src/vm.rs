//! Operator CLI subcommands (`hirsel-os-ctl vm ...`) for managing persistent
//! Tier 2 Linux MicroVMs, declarative VM specs, networking, logs, and SSH access.

use crate::{Transport, ensure_uploaded};
use hirsel_proto::{
    PortForward, Request, Response, RunTarget, VmNetSpec, VmSpec, VmStatus, VmSummary,
};
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
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
    println!("  ram_mib:    {} MiB", s.ram_mib);
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
             --cmdline STRING              Kernel command line\n\
             --guest-ip IP[/PFX]           Guest eth0 IPv4/CIDR (default: 10.0.3.2/24)\n\
             --gateway-ip IP               Virtual gateway IPv4 (default: 10.0.3.1)\n\
             --mac AA:BB:CC:DD:EE:FF       Guest eth0 MAC address\n\
             --ssh-port PORT               Forward node TCP PORT -> guest :22 (default: 2222, 0=off)\n\
             --port HOST:GUEST             Add extra TCP port forward (repeatable)\n\
             --no-net                      Disable Virtio-Net networking\n\
           vm ls                           List managed VMs\n\
           vm inspect <id>                 Show VM spec, network, and live counters\n\
           vm logs <id> [--offset N] [-f]  Read or follow live COM1 serial console logs\n\
           vm send <id> <text>             Send text + newline to VM serial console\n\
           vm ssh <id> [-i KEY] [-- CMD]   Open an SSH session directly into the running VM\n\
           vm stop <id>                    Stop a running VM and free its resources/ports\n\
           vm rm <id>                      Remove a stopped VM record"
    );
}

pub fn handle_vm_command(tx: &mut Transport, node_addr: &str, mut args: Vec<String>) -> Result<(), String> {
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
    let eff_cmdline = cmdline.or(file_spec.cmdline).unwrap_or_default();
    let eff_no_net = no_net || file_spec.no_net;

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
        cmdline: eff_cmdline,
        net,
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
                "{:<14} {:<9} {:>6} {:<16} {:<14} {:>8} {:>10}",
                "ID", "STATE", "RAM", "GUEST_IP", "PORTS", "UPTIME", "NET RX/TX"
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
                    "{:<14} {:<9} {:>4}M {:<16} {:<14} {:>6}ms {:>4}/{}",
                    vm.id,
                    vm.status.as_str(),
                    vm.ram_mib,
                    ip,
                    ports,
                    vm.uptime_ms,
                    vm.net_rx_packets,
                    vm.net_tx_packets,
                );
            }
            Ok(())
        }
        Response::Error { message } => Err(message),
        other => Err(format!("unexpected VmList response: {other:?}")),
    }
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
