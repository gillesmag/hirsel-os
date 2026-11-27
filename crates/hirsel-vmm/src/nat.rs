//! L3/L4 IPv4 NAT (MASQUERADE) router for outbound Tier 2 Linux MicroVM networking.
//!
//! Translates outbound TCP, UDP (including DNS), and ICMP Echo traffic from guest
//! MicroVMs (`10.0.3.x`) onto Hirsel-OS's outer network interface (`10.0.2.15`) and
//! reverse-NATs incoming replies directly back into the originating VM's `virtio-net`
//! channel, allowing VMs to SSH out, resolve DNS, and reach external/host networks
//! without interfering with Hirsel-OS's control plane (`:4444`) or inbound port forwards.

use std::collections::HashMap;

pub const NAT_PORT_MIN: u16 = 50_000;
pub const NAT_PORT_MAX: u16 = 59_999;

const ETH_HDR_LEN: usize = 14;
const ETH_TYPE_IPV4: u16 = 0x0800;
const IP_PROTO_ICMP: u8 = 1;
const IP_PROTO_TCP: u8 = 6;
const IP_PROTO_UDP: u8 = 17;

#[derive(Clone, Debug)]
struct NatEntry {
    vm_id: String,
    guest_ip: [u8; 4],
    guest_mac: [u8; 6],
    guest_port: u16, // TCP/UDP src port, or ICMP Echo ID
    last_tick: u64,
}

/// Stateful IPv4 NAPT (NAT/MASQUERADE) table shared across all running MicroVMs.
pub struct NatEngine {
    outer_ip: [u8; 4],
    outer_mac: [u8; 6],
    outer_gw_mac: Option<[u8; 6]>,
    vm_gw_mac: [u8; 6],
    next_port: u16,
    tick: u64,
    /// `(proto, vm_id, guest_ip, guest_port) -> nat_port`
    fwd: HashMap<(u8, String, [u8; 4], u16), u16>,
    /// `(proto, nat_port) -> NatEntry`
    rev: HashMap<(u8, u16), NatEntry>,
}

impl NatEngine {
    pub fn new(outer_ip: [u8; 4], outer_mac: [u8; 6], vm_gw_mac: [u8; 6]) -> Self {
        Self {
            outer_ip,
            outer_mac,
            // Default QEMU SLIRP gateway MAC (52:55:0a:00:02:02); overridden automatically on first RX.
            outer_gw_mac: Some([0x52, 0x55, 0x0a, 0x00, 0x02, 0x02]),
            vm_gw_mac,
            next_port: NAT_PORT_MIN,
            tick: 0,
            fwd: HashMap::new(),
            rev: HashMap::new(),
        }
    }

    /// Learns the upstream router / QEMU SLIRP MAC address from incoming outer Ethernet frames.
    pub fn learn_outer_gw(&mut self, frame: &[u8]) {
        if frame.len() < ETH_HDR_LEN {
            return;
        }
        let src: [u8; 6] = frame[6..12].try_into().unwrap();
        // Ignore broadcast/multicast/zero/self source MACs.
        if (src[0] & 1) == 0 && src != [0; 6] && src != self.outer_mac {
            self.outer_gw_mac = Some(src);
        }
    }

    pub fn remove_vm(&mut self, vm_id: &str) {
        self.fwd.retain(|(_, id, _, _), _| id != vm_id);
        self.rev.retain(|_, entry| entry.vm_id != vm_id);
    }

    fn alloc_nat_port(
        &mut self,
        proto: u8,
        vm_id: &str,
        guest_ip: [u8; 4],
        guest_mac: [u8; 6],
        guest_port: u16,
    ) -> u16 {
        self.tick = self.tick.wrapping_add(1);
        let key = (proto, vm_id.to_owned(), guest_ip, guest_port);
        if let Some(&port) = self.fwd.get(&key) {
            if let Some(entry) = self.rev.get_mut(&(proto, port)) {
                entry.guest_mac = guest_mac;
                entry.last_tick = self.tick;
            }
            return port;
        }

        // Evict oldest entries if table grows large.
        if self.rev.len() >= 4096 {
            if let Some((&(ev_proto, ev_port), _)) = self.rev.iter().min_by_key(|(_, e)| e.last_tick)
            {
                if let Some(old) = self.rev.remove(&(ev_proto, ev_port)) {
                    self.fwd
                        .remove(&(ev_proto, old.vm_id, old.guest_ip, old.guest_port));
                }
            }
        }

        let range = (NAT_PORT_MAX - NAT_PORT_MIN + 1) as usize;
        for _ in 0..range {
            let candidate = self.next_port;
            self.next_port = if self.next_port >= NAT_PORT_MAX {
                NAT_PORT_MIN
            } else {
                self.next_port + 1
            };
            if !self.rev.contains_key(&(proto, candidate)) {
                self.fwd.insert(key, candidate);
                self.rev.insert(
                    (proto, candidate),
                    NatEntry {
                        vm_id: vm_id.to_owned(),
                        guest_ip,
                        guest_mac,
                        guest_port,
                        last_tick: self.tick,
                    },
                );
                return candidate;
            }
        }

        // Fallback overwrite if all 10,000 ports are occupied.
        let port = NAT_PORT_MIN;
        self.fwd.insert(key, port);
        self.rev.insert(
            (proto, port),
            NatEntry {
                vm_id: vm_id.to_owned(),
                guest_ip,
                guest_mac,
                guest_port,
                last_tick: self.tick,
            },
        );
        port
    }

    /// Returns `true` if `frame` is an IPv4 packet addressed to an external destination
    /// (not the VM's local virtual gateway IP and not broadcast/multicast).
    pub fn should_nat_outbound(frame: &[u8], vm_gw_ip: [u8; 4]) -> bool {
        if frame.len() < ETH_HDR_LEN + 20 {
            return false;
        }
        let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
        if ethertype != ETH_TYPE_IPV4 {
            return false;
        }
        let ver = frame[ETH_HDR_LEN] >> 4;
        if ver != 4 {
            return false;
        }
        let dst_ip: [u8; 4] = frame[ETH_HDR_LEN + 16..ETH_HDR_LEN + 20].try_into().unwrap();
        if dst_ip == vm_gw_ip || dst_ip == [255, 255, 255, 255] || (dst_ip[0] & 0xF0) == 0xE0 {
            return false;
        }
        true
    }

    /// Translates an outbound guest Ethernet/IPv4 frame (TCP/UDP/ICMP Echo) to originate
    /// from `outer_ip`/`outer_mac` destined for `outer_gw_mac`.
    pub fn outbound(
        &mut self,
        vm_id: &str,
        vm_gw_ip: [u8; 4],
        frame: &[u8],
    ) -> Option<Vec<u8>> {
        if !Self::should_nat_outbound(frame, vm_gw_ip) {
            return None;
        }
        let gw_mac = self.outer_gw_mac?;
        let ihl = ((frame[ETH_HDR_LEN] & 0x0F) as usize) * 4;
        if ihl < 20 || frame.len() < ETH_HDR_LEN + ihl {
            return None;
        }
        let total_len =
            u16::from_be_bytes([frame[ETH_HDR_LEN + 2], frame[ETH_HDR_LEN + 3]]) as usize;
        if total_len < ihl || ETH_HDR_LEN + total_len > frame.len() {
            return None;
        }

        let proto = frame[ETH_HDR_LEN + 9];
        let guest_mac: [u8; 6] = frame[6..12].try_into().unwrap();
        let guest_ip: [u8; 4] = frame[ETH_HDR_LEN + 12..ETH_HDR_LEN + 16].try_into().unwrap();
        let l4_off = ETH_HDR_LEN + ihl;
        let l4_len = total_len - ihl;

        let mut out = frame[..ETH_HDR_LEN + total_len].to_vec();

        match proto {
            IP_PROTO_TCP => {
                if l4_len < 20 {
                    return None;
                }
                let guest_port = u16::from_be_bytes([out[l4_off], out[l4_off + 1]]);
                let nat_port =
                    self.alloc_nat_port(IP_PROTO_TCP, vm_id, guest_ip, guest_mac, guest_port);
                out[l4_off..l4_off + 2].copy_from_slice(&nat_port.to_be_bytes());
                out[ETH_HDR_LEN + 12..ETH_HDR_LEN + 16].copy_from_slice(&self.outer_ip);
                recompute_ipv4_checksum(&mut out[ETH_HDR_LEN..ETH_HDR_LEN + ihl]);
                recompute_tcp_checksum(&mut out[ETH_HDR_LEN..ETH_HDR_LEN + total_len], ihl);
            }
            IP_PROTO_UDP => {
                if l4_len < 8 {
                    return None;
                }
                let guest_port = u16::from_be_bytes([out[l4_off], out[l4_off + 1]]);
                let nat_port =
                    self.alloc_nat_port(IP_PROTO_UDP, vm_id, guest_ip, guest_mac, guest_port);
                out[l4_off..l4_off + 2].copy_from_slice(&nat_port.to_be_bytes());
                out[ETH_HDR_LEN + 12..ETH_HDR_LEN + 16].copy_from_slice(&self.outer_ip);
                recompute_ipv4_checksum(&mut out[ETH_HDR_LEN..ETH_HDR_LEN + ihl]);
                recompute_udp_checksum(&mut out[ETH_HDR_LEN..ETH_HDR_LEN + total_len], ihl);
            }
            IP_PROTO_ICMP => {
                if l4_len < 8 || out[l4_off] != 8 {
                    // Only NAT ICMP Echo Requests (type 8).
                    return None;
                }
                let guest_id = u16::from_be_bytes([out[l4_off + 4], out[l4_off + 5]]);
                let nat_id =
                    self.alloc_nat_port(IP_PROTO_ICMP, vm_id, guest_ip, guest_mac, guest_id);
                out[l4_off + 4..l4_off + 6].copy_from_slice(&nat_id.to_be_bytes());
                out[ETH_HDR_LEN + 12..ETH_HDR_LEN + 16].copy_from_slice(&self.outer_ip);
                recompute_ipv4_checksum(&mut out[ETH_HDR_LEN..ETH_HDR_LEN + ihl]);
                recompute_icmp_checksum(&mut out[l4_off..ETH_HDR_LEN + total_len]);
            }
            _ => return None,
        }

        // Rewrite outer Ethernet header.
        out[0..6].copy_from_slice(&gw_mac);
        out[6..12].copy_from_slice(&self.outer_mac);
        Some(out)
    }

    /// Inspects an incoming outer Ethernet frame; if it belongs to an active NAT session,
    /// rewrites it back for the guest VM and returns `Some((vm_id, guest_frame))`.
    pub fn inbound(&mut self, frame: &[u8]) -> Option<(String, Vec<u8>)> {
        self.learn_outer_gw(frame);

        if frame.len() < ETH_HDR_LEN + 20 {
            return None;
        }
        let ethertype = u16::from_be_bytes([frame[12], frame[13]]);
        if ethertype != ETH_TYPE_IPV4 {
            return None;
        }
        if (frame[ETH_HDR_LEN] >> 4) != 4 {
            return None;
        }
        let ihl = ((frame[ETH_HDR_LEN] & 0x0F) as usize) * 4;
        if ihl < 20 || frame.len() < ETH_HDR_LEN + ihl {
            return None;
        }
        let total_len =
            u16::from_be_bytes([frame[ETH_HDR_LEN + 2], frame[ETH_HDR_LEN + 3]]) as usize;
        if total_len < ihl || ETH_HDR_LEN + total_len > frame.len() {
            return None;
        }
        let dst_ip: [u8; 4] = frame[ETH_HDR_LEN + 16..ETH_HDR_LEN + 20].try_into().unwrap();
        if dst_ip != self.outer_ip {
            return None;
        }

        let proto = frame[ETH_HDR_LEN + 9];
        let l4_off = ETH_HDR_LEN + ihl;
        let l4_len = total_len - ihl;

        let nat_port = match proto {
            IP_PROTO_TCP => {
                if l4_len < 20 {
                    return None;
                }
                u16::from_be_bytes([frame[l4_off + 2], frame[l4_off + 3]])
            }
            IP_PROTO_UDP => {
                if l4_len < 8 {
                    return None;
                }
                u16::from_be_bytes([frame[l4_off + 2], frame[l4_off + 3]])
            }
            IP_PROTO_ICMP => {
                if l4_len < 8 || frame[l4_off] != 0 {
                    // Only reverse-NAT ICMP Echo Replies (type 0).
                    return None;
                }
                u16::from_be_bytes([frame[l4_off + 4], frame[l4_off + 5]])
            }
            _ => return None,
        };

        if !(NAT_PORT_MIN..=NAT_PORT_MAX).contains(&nat_port) {
            return None;
        }
        self.tick = self.tick.wrapping_add(1);
        let entry = self.rev.get_mut(&(proto, nat_port))?;
        entry.last_tick = self.tick;
        let vm_id = entry.vm_id.clone();
        let guest_ip = entry.guest_ip;
        let guest_mac = entry.guest_mac;
        let guest_port = entry.guest_port;

        let mut out = frame[..ETH_HDR_LEN + total_len].to_vec();
        out[0..6].copy_from_slice(&guest_mac);
        out[6..12].copy_from_slice(&self.vm_gw_mac);
        out[ETH_HDR_LEN + 16..ETH_HDR_LEN + 20].copy_from_slice(&guest_ip);
        recompute_ipv4_checksum(&mut out[ETH_HDR_LEN..ETH_HDR_LEN + ihl]);

        match proto {
            IP_PROTO_TCP => {
                out[l4_off + 2..l4_off + 4].copy_from_slice(&guest_port.to_be_bytes());
                recompute_tcp_checksum(&mut out[ETH_HDR_LEN..ETH_HDR_LEN + total_len], ihl);
            }
            IP_PROTO_UDP => {
                out[l4_off + 2..l4_off + 4].copy_from_slice(&guest_port.to_be_bytes());
                recompute_udp_checksum(&mut out[ETH_HDR_LEN..ETH_HDR_LEN + total_len], ihl);
            }
            IP_PROTO_ICMP => {
                out[l4_off + 4..l4_off + 6].copy_from_slice(&guest_port.to_be_bytes());
                recompute_icmp_checksum(&mut out[l4_off..ETH_HDR_LEN + total_len]);
            }
            _ => unreachable!(),
        }

        Some((vm_id, out))
    }
}

fn internet_checksum_sum(bytes: &[u8]) -> u32 {
    let mut sum = 0u32;
    let mut chunks = bytes.chunks_exact(2);
    for c in &mut chunks {
        sum += u32::from(u16::from_be_bytes([c[0], c[1]]));
    }
    if let Some(&last) = chunks.remainder().first() {
        sum += u32::from(last) << 8;
    }
    sum
}

fn finalize_checksum(mut sum: u32) -> u16 {
    while (sum >> 16) != 0 {
        sum = (sum & 0xffff) + (sum >> 16);
    }
    !(sum as u16)
}

fn recompute_ipv4_checksum(hdr: &mut [u8]) {
    hdr[10] = 0;
    hdr[11] = 0;
    let csum = finalize_checksum(internet_checksum_sum(hdr));
    hdr[10..12].copy_from_slice(&csum.to_be_bytes());
}

fn pseudo_header_sum(ip_pkt: &[u8], proto: u8, l4_len: usize) -> u32 {
    let mut sum = internet_checksum_sum(&ip_pkt[12..20]);
    sum += u32::from(proto);
    sum += l4_len as u32;
    sum
}

fn recompute_tcp_checksum(ip_pkt: &mut [u8], ihl: usize) {
    let l4_len = ip_pkt.len() - ihl;
    ip_pkt[ihl + 16] = 0;
    ip_pkt[ihl + 17] = 0;
    let sum = pseudo_header_sum(ip_pkt, IP_PROTO_TCP, l4_len)
        + internet_checksum_sum(&ip_pkt[ihl..]);
    let csum = finalize_checksum(sum);
    ip_pkt[ihl + 16..ihl + 18].copy_from_slice(&csum.to_be_bytes());
}

fn recompute_udp_checksum(ip_pkt: &mut [u8], ihl: usize) {
    let l4_len = ip_pkt.len() - ihl;
    ip_pkt[ihl + 6] = 0;
    ip_pkt[ihl + 7] = 0;
    let sum = pseudo_header_sum(ip_pkt, IP_PROTO_UDP, l4_len)
        + internet_checksum_sum(&ip_pkt[ihl..]);
    let mut csum = finalize_checksum(sum);
    if csum == 0 {
        csum = 0xffff;
    }
    ip_pkt[ihl + 6..ihl + 8].copy_from_slice(&csum.to_be_bytes());
}

fn recompute_icmp_checksum(icmp: &mut [u8]) {
    icmp[2] = 0;
    icmp[3] = 0;
    let csum = finalize_checksum(internet_checksum_sum(icmp));
    icmp[2..4].copy_from_slice(&csum.to_be_bytes());
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_eth_ipv4(
        dst_mac: [u8; 6],
        src_mac: [u8; 6],
        src_ip: [u8; 4],
        dst_ip: [u8; 4],
        proto: u8,
        l4: &[u8],
    ) -> Vec<u8> {
        let total_len = (20 + l4.len()) as u16;
        let mut frame = vec![0u8; ETH_HDR_LEN + total_len as usize];
        frame[0..6].copy_from_slice(&dst_mac);
        frame[6..12].copy_from_slice(&src_mac);
        frame[12..14].copy_from_slice(&ETH_TYPE_IPV4.to_be_bytes());
        frame[14] = 0x45;
        frame[16..18].copy_from_slice(&total_len.to_be_bytes());
        frame[22] = 64;
        frame[23] = proto;
        frame[26..30].copy_from_slice(&src_ip);
        frame[30..34].copy_from_slice(&dst_ip);
        frame[34..].copy_from_slice(l4);
        recompute_ipv4_checksum(&mut frame[14..34]);
        frame
    }

    #[test]
    fn tcp_udp_and_icmp_nat_roundtrip() {
        let outer_ip = [10, 0, 2, 15];
        let outer_mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let vm_gw_mac = [0x02, 0x48, 0x52, 0x53, 0x00, 0x01];
        let guest_mac = [0x02, 0x48, 0x52, 0x53, 0x4c, 0x01];
        let guest_ip = [10, 0, 3, 2];
        let vm_gw_ip = [10, 0, 3, 1];
        let remote_ip = [10, 0, 2, 2];

        let mut nat = NatEngine::new(outer_ip, outer_mac, vm_gw_mac);

        // 1. TCP outbound SSH SYN (guest:41234 -> 10.0.2.2:22)
        let mut tcp = vec![0u8; 20];
        tcp[0..2].copy_from_slice(&41234u16.to_be_bytes());
        tcp[2..4].copy_from_slice(&22u16.to_be_bytes());
        tcp[12] = 0x50;
        tcp[13] = 0x02; // SYN
        let out_frame = build_eth_ipv4(vm_gw_mac, guest_mac, guest_ip, remote_ip, IP_PROTO_TCP, &tcp);
        let nat_out = nat.outbound("dev-1", vm_gw_ip, &out_frame).unwrap();
        assert_eq!(&nat_out[26..30], &outer_ip);
        let nat_port = u16::from_be_bytes([nat_out[34], nat_out[35]]);
        assert!((NAT_PORT_MIN..=NAT_PORT_MAX).contains(&nat_port));
        // Verify IPv4 & TCP checksums are valid (sum folds to 0).
        assert_eq!(finalize_checksum(internet_checksum_sum(&nat_out[14..34])), 0);
        assert_eq!(
            finalize_checksum(
                pseudo_header_sum(&nat_out[14..], IP_PROTO_TCP, 20)
                    + internet_checksum_sum(&nat_out[34..])
            ),
            0
        );

        // 2. TCP inbound SYN-ACK (10.0.2.2:22 -> outer:nat_port)
        let mut reply_tcp = vec![0u8; 20];
        reply_tcp[0..2].copy_from_slice(&22u16.to_be_bytes());
        reply_tcp[2..4].copy_from_slice(&nat_port.to_be_bytes());
        reply_tcp[12] = 0x50;
        reply_tcp[13] = 0x12; // SYN-ACK
        let in_frame = build_eth_ipv4(
            outer_mac,
            [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02],
            remote_ip,
            outer_ip,
            IP_PROTO_TCP,
            &reply_tcp,
        );
        let (vm_id, guest_in) = nat.inbound(&in_frame).unwrap();
        assert_eq!(vm_id, "dev-1");
        assert_eq!(&guest_in[0..6], &guest_mac);
        assert_eq!(&guest_in[30..34], &guest_ip);
        assert_eq!(u16::from_be_bytes([guest_in[36], guest_in[37]]), 41234);

        // 3. ICMP Echo Request -> Reply roundtrip
        let mut icmp_req = vec![8, 0, 0, 0, 0x12, 0x34, 0, 1, b'h', b'i'];
        recompute_icmp_checksum(&mut icmp_req);
        let ping_out = build_eth_ipv4(
            vm_gw_mac,
            guest_mac,
            guest_ip,
            [1, 1, 1, 1],
            IP_PROTO_ICMP,
            &icmp_req,
        );
        let nat_ping = nat.outbound("dev-1", vm_gw_ip, &ping_out).unwrap();
        let nat_id = u16::from_be_bytes([nat_ping[38], nat_ping[39]]);
        let mut icmp_rep = vec![0, 0, 0, 0, (nat_id >> 8) as u8, nat_id as u8, 0, 1, b'h', b'i'];
        recompute_icmp_checksum(&mut icmp_rep);
        let ping_in = build_eth_ipv4(
            outer_mac,
            [0x52, 0x55, 0x0a, 0x00, 0x02, 0x02],
            [1, 1, 1, 1],
            outer_ip,
            IP_PROTO_ICMP,
            &icmp_rep,
        );
        let (vm_id2, guest_pong) = nat.inbound(&ping_in).unwrap();
        assert_eq!(vm_id2, "dev-1");
        assert_eq!(u16::from_be_bytes([guest_pong[38], guest_pong[39]]), 0x1234);
    }
}
