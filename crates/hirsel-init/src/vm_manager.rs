//! Persistent managed Tier 2 Linux MicroVM manager and `smoltcp` virtual gateway bridge.

#![cfg(target_os = "fuchsia")]

use crate::runner::BootContext;
use hirsel_proto::{PortForward, VmNetSpec, VmSpec, VmStatus, VmSummary};
use hirsel_vmm::ManagedVmControl;
use hirsel_vmm::virtio_net_mmio::{GPA_VIRTIO_NET_MMIO, VIRTIO_NET_IRQ, VirtioNetChannel};
use smoltcp::iface::{Config as IfaceConfig, Interface, SocketHandle, SocketSet};
use smoltcp::phy::{self, Device, DeviceCapabilities, Medium};
use smoltcp::socket::tcp;
use smoltcp::time::Instant as SmolInstant;
use smoltcp::wire::{EthernetAddress, HardwareAddress, IpAddress, IpCidr, Ipv4Address};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::thread;
use zx_sys::*;

struct ManagedVmRecord {
    id: String,
    name: String,
    ram_mib: u32,
    started_mono_ns: i64,
    ended_mono_ns: Option<i64>,
    status: VmStatus,
    exit_code: i32,
    error: String,
    net: Option<VmNetSpec>,
    ctrl: Arc<ManagedVmControl>,
}

/// Thread-safe registry of persistent managed Tier 2 Linux MicroVMs.
pub struct VmManager {
    next_id: u64,
    vms: HashMap<String, Arc<Mutex<ManagedVmRecord>>>,
}

#[derive(Clone)]
pub struct ActiveNetAttachment {
    pub vm_id: String,
    pub net: VmNetSpec,
    pub channel: Arc<VirtioNetChannel>,
}

impl VmManager {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            vms: HashMap::new(),
        }
    }

    pub fn start_vm(
        &mut self,
        ctx: &Arc<BootContext>,
        spec: VmSpec,
        kernel_bytes: Vec<u8>,
        initrd_bytes: Option<Vec<u8>>,
    ) -> Result<VmSummary, String> {
        let id = if spec.name.trim().is_empty() {
            let n = self.next_id;
            self.next_id += 1;
            format!("vm-{n}")
        } else {
            spec.name.trim().to_owned()
        };

        if let Some(existing) = self.vms.get(&id) {
            if existing.lock().unwrap().status == VmStatus::Running {
                return Err(format!("VM `{id}` is already running"));
            }
        }

        // Check for host port conflicts across currently running VMs.
        if let Some(net) = spec.net.as_ref() {
            for pf in &net.ports {
                if pf.host_port == 4444 {
                    return Err("host port 4444 is reserved for Hirsel-OS control plane".into());
                }
                for rec_arc in self.vms.values() {
                    let rec = rec_arc.lock().unwrap();
                    if rec.status == VmStatus::Running {
                        if let Some(other_net) = rec.net.as_ref() {
                            if other_net.ports.iter().any(|p| p.host_port == pf.host_port) {
                                return Err(format!(
                                    "host port {} is already in use by VM `{}`",
                                    pf.host_port, rec.id
                                ));
                            }
                        }
                    }
                }
            }
        }

        let ram_mib = spec.ram_mib.max(64);
        let mut net_spec = spec.net.clone();
        if let Some(ref mut n) = net_spec {
            if n.mac == [0; 6] {
                let seq = (self.next_id as u8).max(1);
                n.mac = [0x02, 0x48, 0x52, 0x53, 0x4c, seq];
            }
            if n.prefix_len == 0 || n.prefix_len > 30 {
                n.prefix_len = 24;
            }
        }

        // Compose final Linux kernel command line with Virtio-MMIO net device & IP parameters.
        let mut cmdline = if spec.cmdline.trim().is_empty() {
            String::from("console=ttyS0 reboot=k panic=1 pci=off nomodules")
        } else {
            spec.cmdline.clone()
        };
        if let Some(ref n) = net_spec {
            if !cmdline.contains("virtio_mmio.device=") {
                cmdline.push_str(&format!(
                    " virtio_mmio.device=4K@{:#x}:{}",
                    GPA_VIRTIO_NET_MMIO, VIRTIO_NET_IRQ
                ));
            }
            if !cmdline.contains("hirsel.ip=") {
                cmdline.push_str(&format!(
                    " hirsel.ip={}.{}.{}.{}/{} hirsel.gw={}.{}.{}.{} hirsel.name={}",
                    n.guest_ip[0],
                    n.guest_ip[1],
                    n.guest_ip[2],
                    n.guest_ip[3],
                    n.prefix_len,
                    n.gateway_ip[0],
                    n.gateway_ip[1],
                    n.gateway_ip[2],
                    n.gateway_ip[3],
                    id
                ));
            }
        }

        let net_channel = net_spec.as_ref().map(|_| VirtioNetChannel::new());
        let mac = net_spec.as_ref().map(|n| n.mac);
        let ctrl = ManagedVmControl::new(net_channel);
        let now = unsafe { zx_clock_get_monotonic() };

        let rec = Arc::new(Mutex::new(ManagedVmRecord {
            id: id.clone(),
            name: id.clone(),
            ram_mib,
            started_mono_ns: now,
            ended_mono_ns: None,
            status: VmStatus::Running,
            exit_code: 0,
            error: String::new(),
            net: net_spec.clone(),
            ctrl: Arc::clone(&ctrl),
        }));

        self.vms.insert(id.clone(), Arc::clone(&rec));
        let summary = summarize_record(&rec.lock().unwrap());

        println!(
            "[hirsel-vm] starting managed Linux VM `{id}` ({ram_mib} MiB, net={})",
            net_spec
                .as_ref()
                .map(|n| format!(
                    "{}.{}.{}.{}/{} gw={}.{}.{}.{} ports={:?}",
                    n.guest_ip[0],
                    n.guest_ip[1],
                    n.guest_ip[2],
                    n.guest_ip[3],
                    n.prefix_len,
                    n.gateway_ip[0],
                    n.gateway_ip[1],
                    n.gateway_ip[2],
                    n.gateway_ip[3],
                    n.ports
                        .iter()
                        .map(|p| format!("{}:{}", p.host_port, p.guest_port))
                        .collect::<Vec<_>>()
                ))
                .unwrap_or_else(|| "none".into())
        );

        let hypervisor_res = ctx.hypervisor_resource;
        let vmex_res = ctx.vmex_resource;
        let vm_id_bg = id;
        let ram_bytes = (ram_mib as usize) * 1024 * 1024;
        thread::spawn(move || {
            let res = hirsel_vmm::run_managed_linux_vm(
                hypervisor_res,
                vmex_res,
                &kernel_bytes,
                initrd_bytes.as_deref(),
                &cmdline,
                ram_bytes,
                mac,
                ctrl,
            );
            let ended = unsafe { zx_clock_get_monotonic() };
            let mut r = rec.lock().unwrap();
            r.ended_mono_ns = Some(ended);
            match res {
                Ok(report) => {
                    r.status = VmStatus::Exited;
                    r.exit_code = report.exit_code;
                    println!(
                        "[hirsel-vm] managed VM `{vm_id_bg}` exited cleanly code={} (io={}, mmio={})",
                        report.exit_code, report.io_exits, report.mmio_exits
                    );
                }
                Err(e) => {
                    r.status = VmStatus::Failed;
                    r.exit_code = -1;
                    r.error = e.clone();
                    println!("[hirsel-vm] managed VM `{vm_id_bg}` failed: {e}");
                }
            }
        });

        Ok(summary)
    }

    pub fn list_vms(&self) -> Vec<VmSummary> {
        let mut out: Vec<VmSummary> = self
            .vms
            .values()
            .map(|r| summarize_record(&r.lock().unwrap()))
            .collect();
        out.sort_by(|a, b| a.id.cmp(&b.id));
        out
    }

    pub fn inspect_vm(&self, id: &str) -> Result<VmSummary, String> {
        let rec = self
            .vms
            .get(id)
            .ok_or_else(|| format!("VM `{id}` not found"))?;
        Ok(summarize_record(&rec.lock().unwrap()))
    }

    pub fn read_logs(
        &self,
        id: &str,
        offset: u64,
        max_bytes: u32,
    ) -> Result<(Vec<u8>, u64, VmStatus), String> {
        let rec = self
            .vms
            .get(id)
            .ok_or_else(|| format!("VM `{id}` not found"))?;
        let r = rec.lock().unwrap();
        let (data, next_offset) = r
            .ctrl
            .read_console_logs(offset, (max_bytes as usize).min(64 * 1024));
        Ok((data, next_offset, r.status))
    }

    pub fn write_console(&self, id: &str, data: &[u8]) -> Result<(), String> {
        let rec = self
            .vms
            .get(id)
            .ok_or_else(|| format!("VM `{id}` not found"))?;
        let r = rec.lock().unwrap();
        if r.status != VmStatus::Running {
            return Err(format!("VM `{id}` is not running"));
        }
        r.ctrl.write_console_in(data);
        Ok(())
    }

    pub fn stop_vm(&self, id: &str) -> Result<VmSummary, String> {
        let rec = self
            .vms
            .get(id)
            .ok_or_else(|| format!("VM `{id}` not found"))?;
        {
            let r = rec.lock().unwrap();
            if r.status == VmStatus::Running {
                r.ctrl.request_stop();
            }
        }
        // Wait briefly (up to 1s) for VCPU thread teardown so the caller sees `Exited`.
        for _ in 0..50 {
            let r = rec.lock().unwrap();
            if r.status != VmStatus::Running {
                return Ok(summarize_record(&r));
            }
            drop(r);
            let next = unsafe { zx_clock_get_monotonic() }.saturating_add(20_000_000);
            unsafe { zx_nanosleep(next) };
        }
        Ok(summarize_record(&rec.lock().unwrap()))
    }

    pub fn remove_vm(&mut self, id: &str) -> Result<(), String> {
        let running = self
            .vms
            .get(id)
            .ok_or_else(|| format!("VM `{id}` not found"))?
            .lock()
            .unwrap()
            .status
            == VmStatus::Running;
        if running {
            return Err(format!("VM `{id}` is still running; stop it first"));
        }
        self.vms.remove(id);
        Ok(())
    }

    pub fn active_net_attachments(&self) -> Vec<ActiveNetAttachment> {
        let mut out = Vec::new();
        for rec_arc in self.vms.values() {
            let rec = rec_arc.lock().unwrap();
            if rec.status == VmStatus::Running {
                if let (Some(net), Some(ch)) = (rec.net.as_ref(), rec.ctrl.net_channel.as_ref()) {
                    out.push(ActiveNetAttachment {
                        vm_id: rec.id.clone(),
                        net: net.clone(),
                        channel: Arc::clone(ch),
                    });
                }
            }
        }
        out
    }
}

fn summarize_record(r: &ManagedVmRecord) -> VmSummary {
    let end = r
        .ended_mono_ns
        .unwrap_or_else(|| unsafe { zx_clock_get_monotonic() });
    let uptime_ms = end.saturating_sub(r.started_mono_ns).max(0) as u64 / 1_000_000;
    let (net_rx_packets, net_tx_packets) = match r.ctrl.net_channel.as_ref() {
        Some(ch) => (
            ch.rx_packets.load(std::sync::atomic::Ordering::Relaxed),
            ch.tx_packets.load(std::sync::atomic::Ordering::Relaxed),
        ),
        None => (0, 0),
    };
    VmSummary {
        id: r.id.clone(),
        name: r.name.clone(),
        status: r.status,
        exit_code: r.exit_code,
        error: r.error.clone(),
        ram_mib: r.ram_mib,
        uptime_ms,
        io_exits: r.ctrl.io_exits.load(std::sync::atomic::Ordering::Relaxed),
        mmio_exits: r.ctrl.mmio_exits.load(std::sync::atomic::Ordering::Relaxed),
        net_rx_packets,
        net_tx_packets,
        net: r.net.clone(),
    }
}

/// `smoltcp::phy::Device` implementation bridged directly to a guest's `VirtioNetChannel`.
pub struct VmNetDevice {
    channel: Arc<VirtioNetChannel>,
}

pub struct VmNetRxToken(Vec<u8>);

impl phy::RxToken for VmNetRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.0)
    }
}

pub struct VmNetTxToken {
    channel: Arc<VirtioNetChannel>,
}

impl phy::TxToken for VmNetTxToken {
    fn consume<R, F>(self, len: usize, f: F) -> R
    where
        F: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = vec![0u8; len];
        let r = f(&mut buf);
        self.channel.host_push_tx(buf);
        r
    }
}

impl Device for VmNetDevice {
    type RxToken<'a> = VmNetRxToken where Self: 'a;
    type TxToken<'a> = VmNetTxToken where Self: 'a;

    fn receive(&mut self, _timestamp: SmolInstant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let frame = self.channel.host_pop_rx()?;
        Some((
            VmNetRxToken(frame),
            VmNetTxToken {
                channel: Arc::clone(&self.channel),
            },
        ))
    }

    fn transmit(&mut self, _timestamp: SmolInstant) -> Option<Self::TxToken<'_>> {
        Some(VmNetTxToken {
            channel: Arc::clone(&self.channel),
        })
    }

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps.max_burst_size = Some(16);
        caps
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum TunnelState {
    Listening,
    Connecting,
    Active,
}

struct PortTunnel {
    pf: PortForward,
    outer_handle: SocketHandle,
    inner_handle: SocketHandle,
    state: TunnelState,
}

/// Per-VM virtual Ethernet gateway (`10.0.3.1/24`) + L4 TCP port forwarder.
pub struct ActiveVmGateway {
    dev: VmNetDevice,
    inner_iface: Interface,
    inner_sockets: SocketSet<'static>,
    guest_ip: Ipv4Address,
    tunnels: Vec<PortTunnel>,
    next_ephemeral: u16,
}

impl ActiveVmGateway {
    pub fn new(
        att: &ActiveNetAttachment,
        outer_sockets: &mut SocketSet<'static>,
        now: SmolInstant,
    ) -> Self {
        let mut dev = VmNetDevice {
            channel: Arc::clone(&att.channel),
        };
        let gw_mac = EthernetAddress([0x02, 0x48, 0x52, 0x53, 0x00, 0x01]);
        let mut cfg = IfaceConfig::new(HardwareAddress::Ethernet(gw_mac));
        let mut seed = [0u8; 8];
        unsafe { zx_cprng_draw(seed.as_mut_ptr(), seed.len()) };
        cfg.random_seed = u64::from_le_bytes(seed);

        let mut inner_iface = Interface::new(cfg, &mut dev, now);
        let gw = att.net.gateway_ip;
        let prefix = att.net.prefix_len;
        inner_iface.update_ip_addrs(|addrs| {
            let _ = addrs.push(IpCidr::new(IpAddress::v4(gw[0], gw[1], gw[2], gw[3]), prefix));
        });

        let mut inner_sockets = SocketSet::new(vec![]);
        let mut tunnels = Vec::new();
        const SLOTS_PER_PORT: usize = 2;

        for &pf in &att.net.ports {
            for _ in 0..SLOTS_PER_PORT {
                let mut outer = tcp::Socket::new(
                    tcp::SocketBuffer::new(vec![0u8; 64 * 1024]),
                    tcp::SocketBuffer::new(vec![0u8; 64 * 1024]),
                );
                outer.set_nagle_enabled(false);
                outer.set_ack_delay(None);
                let _ = outer.listen(pf.host_port);
                let outer_handle = outer_sockets.add(outer);

                let mut inner = tcp::Socket::new(
                    tcp::SocketBuffer::new(vec![0u8; 64 * 1024]),
                    tcp::SocketBuffer::new(vec![0u8; 64 * 1024]),
                );
                inner.set_nagle_enabled(false);
                inner.set_ack_delay(None);
                let inner_handle = inner_sockets.add(inner);

                tunnels.push(PortTunnel {
                    pf,
                    outer_handle,
                    inner_handle,
                    state: TunnelState::Listening,
                });
            }
            println!(
                "[hirsel-netd] forwarding node :{} -> VM `{}` {}.{}.{}.{}:{}",
                pf.host_port,
                att.vm_id,
                att.net.guest_ip[0],
                att.net.guest_ip[1],
                att.net.guest_ip[2],
                att.net.guest_ip[3],
                pf.guest_port
            );
        }

        let g = att.net.guest_ip;
        Self {
            dev,
            inner_iface,
            inner_sockets,
            guest_ip: Ipv4Address::new(g[0], g[1], g[2], g[3]),
            tunnels,
            next_ephemeral: 40_000,
        }
    }

    /// Removes this gateway's outer listening sockets when the VM stops.
    pub fn teardown(self, outer_sockets: &mut SocketSet<'static>) {
        for t in self.tunnels {
            outer_sockets.remove(t.outer_handle);
        }
    }

    /// Polls the VM's virtual L2 link (ARP/ICMP/TCP) and pumps active port-forward tunnels.
    /// Returns `true` if any network activity occurred.
    pub fn poll(&mut self, outer_sockets: &mut SocketSet<'static>, now: SmolInstant) -> bool {
        let mut active = !matches!(
            self.inner_iface
                .poll(now, &mut self.dev, &mut self.inner_sockets),
            smoltcp::iface::PollResult::None
        );

        for t in &mut self.tunnels {
            let outer = outer_sockets.get_mut::<tcp::Socket>(t.outer_handle);
            let inner = self.inner_sockets.get_mut::<tcp::Socket>(t.inner_handle);

            match t.state {
                TunnelState::Listening => {
                    if !outer.is_open() {
                        let _ = outer.listen(t.pf.host_port);
                    } else if outer.is_active() {
                        let local_port = self.next_ephemeral;
                        self.next_ephemeral = if self.next_ephemeral >= 60_000 {
                            40_000
                        } else {
                            self.next_ephemeral + 1
                        };
                        if inner
                            .connect(
                                self.inner_iface.context(),
                                (self.guest_ip, t.pf.guest_port),
                                local_port,
                            )
                            .is_ok()
                        {
                            t.state = TunnelState::Connecting;
                            active = true;
                        } else {
                            outer.abort();
                        }
                    }
                }
                TunnelState::Connecting => {
                    if !outer.is_open() || !inner.is_open() {
                        outer.abort();
                        inner.abort();
                        let _ = outer.listen(t.pf.host_port);
                        t.state = TunnelState::Listening;
                        active = true;
                    } else if inner.may_send() {
                        t.state = TunnelState::Active;
                        active = true;
                    }
                }
                TunnelState::Active => {
                    // Pump outer -> inner (zero-copy backpressure-preserving closure).
                    while outer.can_recv() && inner.can_send() {
                        let moved = outer
                            .recv(|data| {
                                let n = inner.send_slice(data).unwrap_or(0);
                                (n, n)
                            })
                            .unwrap_or(0);
                        if moved == 0 {
                            break;
                        }
                        active = true;
                    }
                    // Pump inner -> outer.
                    while inner.can_recv() && outer.can_send() {
                        let moved = inner
                            .recv(|data| {
                                let n = outer.send_slice(data).unwrap_or(0);
                                (n, n)
                            })
                            .unwrap_or(0);
                        if moved == 0 {
                            break;
                        }
                        active = true;
                    }

                    if !outer.may_recv() && inner.may_send() {
                        inner.close();
                        active = true;
                    }
                    if !inner.may_recv() && outer.may_send() {
                        outer.close();
                        active = true;
                    }
                    if !outer.is_open() && !inner.is_open() {
                        let _ = outer.listen(t.pf.host_port);
                        t.state = TunnelState::Listening;
                        active = true;
                    }
                }
            }
        }

        if !matches!(
            self.inner_iface
                .poll(now, &mut self.dev, &mut self.inner_sockets),
            smoltcp::iface::PollResult::None
        ) {
            active = true;
        }
        active
    }
}
