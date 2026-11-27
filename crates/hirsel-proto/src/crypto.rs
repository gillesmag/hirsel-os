//! Zero-dependency RFC 8439 ChaCha20-Poly1305 AEAD, HMAC-SHA256, and HKDF-SHA256
//! for mutually authenticated, encrypted Hirsel-OS control-plane sessions.

use crate::sha256;

/// Default development cluster key (overridden in production via `--key` / `HIRSEL_OS_KEY`).
pub const DEFAULT_DEV_KEY_STR: &str = "hirsel-os-dev-cluster-key-v1";

pub fn derive_cluster_key(secret: &str) -> [u8; 32] {
    let mut input = Vec::with_capacity(24 + secret.len());
    input.extend_from_slice(b"hirsel-os.cluster-key.v1:");
    input.extend_from_slice(secret.as_bytes());
    sha256(&input)
}

/// Constant-time 32-byte equality check.
pub fn ct_eq_32(a: &[u8; 32], b: &[u8; 32]) -> bool {
    let mut diff = 0u8;
    for i in 0..32 {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

pub use ct_eq_32 as ct_eq;

/// RFC 2104 HMAC-SHA256.
pub fn hmac_sha256(key: &[u8], parts: &[&[u8]]) -> [u8; 32] {
    let mut k_block = [0u8; 64];
    if key.len() > 64 {
        let h = sha256(key);
        k_block[..32].copy_from_slice(&h);
    } else {
        k_block[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0u8; 64];
    let mut opad = [0u8; 64];
    for i in 0..64 {
        ipad[i] = k_block[i] ^ 0x36;
        opad[i] = k_block[i] ^ 0x5c;
    }

    let total_msg: usize = parts.iter().map(|p| p.len()).sum();
    let mut inner = Vec::with_capacity(64 + total_msg);
    inner.extend_from_slice(&ipad);
    for p in parts {
        inner.extend_from_slice(p);
    }
    let inner_hash = sha256(&inner);

    let mut outer = [0u8; 96];
    outer[..64].copy_from_slice(&opad);
    outer[64..96].copy_from_slice(&inner_hash);
    sha256(&outer)
}

/// Derives `(client_to_server_key, server_to_client_key)` from cluster key + both nonces.
pub fn derive_session_keys(
    cluster_key: &[u8; 32],
    client_nonce: &[u8; 32],
    server_nonce: &[u8; 32],
) -> ([u8; 32], [u8; 32]) {
    let prk = hmac_sha256(cluster_key, &[b"hirsel-os.hkdf.v1", client_nonce, server_nonce]);
    let c2s = hmac_sha256(&prk, &[b"c2s", &[1u8]]);
    let s2c = hmac_sha256(&prk, &[b"s2c", &[1u8]]);
    (c2s, s2c)
}

pub fn server_auth_mac(
    cluster_key: &[u8; 32],
    client_nonce: &[u8; 32],
    server_nonce: &[u8; 32],
) -> [u8; 32] {
    hmac_sha256(cluster_key, &[b"hirsel-os-srv-v1", client_nonce, server_nonce])
}

pub fn client_auth_mac(
    cluster_key: &[u8; 32],
    client_nonce: &[u8; 32],
    server_nonce: &[u8; 32],
) -> [u8; 32] {
    hmac_sha256(cluster_key, &[b"hirsel-os-cli-v1", client_nonce, server_nonce])
}

pub use client_auth_mac as compute_client_auth_mac;
pub use server_auth_mac as compute_server_auth_mac;

/// Perfect Forward Secrecy (PFS) session key derivation combining static cluster key
/// with an ephemeral RFC 7748 X25519 ECDH shared secret and both session nonces.
pub fn derive_session_keys_pfs(
    cluster_key: &[u8; 32],
    ecdh_shared: &[u8; 32],
    client_nonce: &[u8; 32],
    server_nonce: &[u8; 32],
) -> ([u8; 32], [u8; 32]) {
    let prk = hmac_sha256(
        cluster_key,
        &[b"hirsel-os.hkdf.pfs.v2", ecdh_shared, client_nonce, server_nonce],
    );
    let c2s = hmac_sha256(&prk, &[b"c2s", &[1u8]]);
    let s2c = hmac_sha256(&prk, &[b"s2c", &[1u8]]);
    (c2s, s2c)
}

/// Server authentication MAC binding the ephemeral X25519 public keys and nonces
/// to the pre-shared cluster key (prevents MitM on the ephemeral key exchange).
pub fn server_auth_mac_pfs(
    cluster_key: &[u8; 32],
    client_nonce: &[u8; 32],
    client_pubkey: &[u8; 32],
    server_nonce: &[u8; 32],
    server_pubkey: &[u8; 32],
) -> [u8; 32] {
    hmac_sha256(
        cluster_key,
        &[
            b"hirsel-os-srv-pfs-v2",
            client_nonce,
            client_pubkey,
            server_nonce,
            server_pubkey,
        ],
    )
}

/// Client authentication MAC binding both ephemeral X25519 public keys, nonces, and shared secret.
pub fn client_auth_mac_pfs(
    cluster_key: &[u8; 32],
    ecdh_shared: &[u8; 32],
    client_nonce: &[u8; 32],
    client_pubkey: &[u8; 32],
    server_nonce: &[u8; 32],
    server_pubkey: &[u8; 32],
) -> [u8; 32] {
    hmac_sha256(
        cluster_key,
        &[
            b"hirsel-os-cli-pfs-v2",
            ecdh_shared,
            client_nonce,
            client_pubkey,
            server_nonce,
            server_pubkey,
        ],
    )
}

// --- RFC 8439 ChaCha20 ---

#[inline]
fn quarter_round(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] ^= s[a];
    s[d] = s[d].rotate_left(16);

    s[c] = s[c].wrapping_add(s[d]);
    s[b] ^= s[c];
    s[b] = s[b].rotate_left(12);

    s[a] = s[a].wrapping_add(s[b]);
    s[d] ^= s[a];
    s[d] = s[d].rotate_left(8);

    s[c] = s[c].wrapping_add(s[d]);
    s[b] ^= s[c];
    s[b] = s[b].rotate_left(7);
}

fn chacha20_block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut state = [0u32; 16];
    state[0] = 0x6170_7865;
    state[1] = 0x3320_646e;
    state[2] = 0x7962_2d32;
    state[3] = 0x6b20_6574;
    for i in 0..8 {
        state[4 + i] = u32::from_le_bytes(key[i * 4..(i + 1) * 4].try_into().unwrap());
    }
    state[12] = counter;
    for i in 0..3 {
        state[13 + i] = u32::from_le_bytes(nonce[i * 4..(i + 1) * 4].try_into().unwrap());
    }

    let mut working = state;
    for _ in 0..10 {
        quarter_round(&mut working, 0, 4, 8, 12);
        quarter_round(&mut working, 1, 5, 9, 13);
        quarter_round(&mut working, 2, 6, 10, 14);
        quarter_round(&mut working, 3, 7, 11, 15);
        quarter_round(&mut working, 0, 5, 10, 15);
        quarter_round(&mut working, 1, 6, 11, 12);
        quarter_round(&mut working, 2, 7, 8, 13);
        quarter_round(&mut working, 3, 4, 9, 14);
    }

    let mut out = [0u8; 64];
    for i in 0..16 {
        let w = working[i].wrapping_add(state[i]);
        out[i * 4..(i + 1) * 4].copy_from_slice(&w.to_le_bytes());
    }
    out
}

fn chacha20_xor(key: &[u8; 32], mut counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
    for chunk in data.chunks_mut(64) {
        let block = chacha20_block(key, counter, nonce);
        counter = counter.wrapping_add(1);
        for (b, k) in chunk.iter_mut().zip(block.iter()) {
            *b ^= *k;
        }
    }
}

// --- RFC 8439 Poly1305 (radix-2^26 5-limb arithmetic) ---

struct Poly1305 {
    r: [u64; 5],
    s: [u32; 4],
    h: [u64; 5],
}

impl Poly1305 {
    fn new(key: &[u8; 32]) -> Self {
        let mut r_bytes = [0u8; 16];
        r_bytes.copy_from_slice(&key[0..16]);
        // Clamp r per RFC 8439 §2.5.
        r_bytes[3] &= 15;
        r_bytes[7] &= 15;
        r_bytes[11] &= 15;
        r_bytes[15] &= 15;
        r_bytes[4] &= 252;
        r_bytes[8] &= 252;
        r_bytes[12] &= 252;

        let t0 = u32::from_le_bytes(r_bytes[0..4].try_into().unwrap()) as u64;
        let t1 = u32::from_le_bytes(r_bytes[4..8].try_into().unwrap()) as u64;
        let t2 = u32::from_le_bytes(r_bytes[8..12].try_into().unwrap()) as u64;
        let t3 = u32::from_le_bytes(r_bytes[12..16].try_into().unwrap()) as u64;

        let r = [
            t0 & 0x3ff_ffff,
            ((t0 >> 26) | (t1 << 6)) & 0x3ff_ffff,
            ((t1 >> 20) | (t2 << 12)) & 0x3ff_ffff,
            ((t2 >> 14) | (t3 << 18)) & 0x3ff_ffff,
            t3 >> 8,
        ];
        let s = [
            u32::from_le_bytes(key[16..20].try_into().unwrap()),
            u32::from_le_bytes(key[20..24].try_into().unwrap()),
            u32::from_le_bytes(key[24..28].try_into().unwrap()),
            u32::from_le_bytes(key[28..32].try_into().unwrap()),
        ];
        Self { r, s, h: [0; 5] }
    }

    fn update_block(&mut self, block: &[u8], hibit: u64) {
        let mut buf = [0u8; 16];
        buf[..block.len()].copy_from_slice(block);
        if block.len() < 16 {
            buf[block.len()] = 1;
        }
        let t0 = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as u64;
        let t1 = u32::from_le_bytes(buf[4..8].try_into().unwrap()) as u64;
        let t2 = u32::from_le_bytes(buf[8..12].try_into().unwrap()) as u64;
        let t3 = u32::from_le_bytes(buf[12..16].try_into().unwrap()) as u64;

        let mut h0 = self.h[0] + (t0 & 0x3ff_ffff);
        let mut h1 = self.h[1] + (((t0 >> 26) | (t1 << 6)) & 0x3ff_ffff);
        let mut h2 = self.h[2] + (((t1 >> 20) | (t2 << 12)) & 0x3ff_ffff);
        let mut h3 = self.h[3] + (((t2 >> 14) | (t3 << 18)) & 0x3ff_ffff);
        let mut h4 = self.h[4] + (t3 >> 8) + hibit;

        let [r0, r1, r2, r3, r4] = self.r;
        let s1 = r1 * 5;
        let s2 = r2 * 5;
        let s3 = r3 * 5;
        let s4 = r4 * 5;

        let d0 = h0 * r0 + h1 * s4 + h2 * s3 + h3 * s2 + h4 * s1;
        let mut d1 = h0 * r1 + h1 * r0 + h2 * s4 + h3 * s3 + h4 * s2;
        let mut d2 = h0 * r2 + h1 * r1 + h2 * r0 + h3 * s4 + h4 * s3;
        let mut d3 = h0 * r3 + h1 * r2 + h2 * r1 + h3 * r0 + h4 * s4;
        let mut d4 = h0 * r4 + h1 * r3 + h2 * r2 + h3 * r1 + h4 * r0;

        let mut c = d0 >> 26;
        h0 = d0 & 0x3ff_ffff;
        d1 += c;
        c = d1 >> 26;
        h1 = d1 & 0x3ff_ffff;
        d2 += c;
        c = d2 >> 26;
        h2 = d2 & 0x3ff_ffff;
        d3 += c;
        c = d3 >> 26;
        h3 = d3 & 0x3ff_ffff;
        d4 += c;
        c = d4 >> 26;
        h4 = d4 & 0x3ff_ffff;
        h0 += c * 5;
        c = h0 >> 26;
        h0 &= 0x3ff_ffff;
        h1 += c;

        self.h = [h0, h1, h2, h3, h4];
    }

    fn update_padded(&mut self, data: &[u8]) {
        for chunk in data.chunks(16) {
            if chunk.len() == 16 {
                self.update_block(chunk, 1 << 24);
            } else {
                // Zero-pad to 16 bytes for RFC 8439 §2.8 AEAD construction (hibit=1<<24).
                let mut padded = [0u8; 16];
                padded[..chunk.len()].copy_from_slice(chunk);
                self.update_block(&padded, 1 << 24);
            }
        }
    }

    fn finish(mut self) -> [u8; 16] {
        let [mut h0, mut h1, mut h2, mut h3, mut h4] = self.h;
        let mut c = h1 >> 26;
        h1 &= 0x3ff_ffff;
        h2 += c;
        c = h2 >> 26;
        h2 &= 0x3ff_ffff;
        h3 += c;
        c = h3 >> 26;
        h3 &= 0x3ff_ffff;
        h4 += c;
        c = h4 >> 26;
        h4 &= 0x3ff_ffff;
        h0 += c * 5;
        c = h0 >> 26;
        h0 &= 0x3ff_ffff;
        h1 += c;

        // Compute h - p = h + 5 - 2^130.
        let mut g0 = h0 + 5;
        c = g0 >> 26;
        g0 &= 0x3ff_ffff;
        let mut g1 = h1 + c;
        c = g1 >> 26;
        g1 &= 0x3ff_ffff;
        let mut g2 = h2 + c;
        c = g2 >> 26;
        g2 &= 0x3ff_ffff;
        let mut g3 = h3 + c;
        c = g3 >> 26;
        g3 &= 0x3ff_ffff;
        let g4 = h4.wrapping_add(c).wrapping_sub(1 << 26);

        let mask = (g4 >> 63).wrapping_sub(1); // all-1s if h >= p else 0
        h0 = (h0 & !mask) | (g0 & mask);
        h1 = (h1 & !mask) | (g1 & mask);
        h2 = (h2 & !mask) | (g2 & mask);
        h3 = (h3 & !mask) | (g3 & mask);
        h4 = (h4 & !mask) | (g4 & mask);

        let f0 = ((h0) | (h1 << 26)) as u32;
        let f1 = ((h1 >> 6) | (h2 << 20)) as u32;
        let f2 = ((h2 >> 12) | (h3 << 14)) as u32;
        let f3 = ((h3 >> 18) | (h4 << 8)) as u32;

        let mut acc = (f0 as u64) + (self.s[0] as u64);
        let o0 = acc as u32;
        acc = (f1 as u64) + (self.s[1] as u64) + (acc >> 32);
        let o1 = acc as u32;
        acc = (f2 as u64) + (self.s[2] as u64) + (acc >> 32);
        let o2 = acc as u32;
        acc = (f3 as u64) + (self.s[3] as u64) + (acc >> 32);
        let o3 = acc as u32;

        self.h = [0; 5];
        let mut tag = [0u8; 16];
        tag[0..4].copy_from_slice(&o0.to_le_bytes());
        tag[4..8].copy_from_slice(&o1.to_le_bytes());
        tag[8..12].copy_from_slice(&o2.to_le_bytes());
        tag[12..16].copy_from_slice(&o3.to_le_bytes());
        tag
    }
}

fn poly1305_aead_tag(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], ciphertext: &[u8]) -> [u8; 16] {
    let otk_block = chacha20_block(key, 0, nonce);
    let mut otk = [0u8; 32];
    otk.copy_from_slice(&otk_block[0..32]);
    let mut mac = Poly1305::new(&otk);
    mac.update_padded(aad);
    mac.update_padded(ciphertext);
    let mut lens = [0u8; 16];
    lens[0..8].copy_from_slice(&(aad.len() as u64).to_le_bytes());
    lens[8..16].copy_from_slice(&(ciphertext.len() as u64).to_le_bytes());
    mac.update_block(&lens, 1 << 24);
    mac.finish()
}

/// RFC 8439 AEAD_CHACHA20_POLY1305 seal: returns `ciphertext || tag[16]`.
pub fn aead_seal(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(plaintext.len() + 16);
    out.extend_from_slice(plaintext);
    chacha20_xor(key, 1, nonce, &mut out);
    let tag = poly1305_aead_tag(key, nonce, aad, &out);
    out.extend_from_slice(&tag);
    out
}

/// RFC 8439 AEAD_CHACHA20_POLY1305 open: verifies tag in constant time and decrypts.
pub fn aead_open(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    sealed: &[u8],
) -> Result<Vec<u8>, String> {
    if sealed.len() < 16 {
        return Err("AEAD ciphertext shorter than 16-byte tag".into());
    }
    let ct_len = sealed.len() - 16;
    let (ct, tag) = sealed.split_at(ct_len);
    let expected = poly1305_aead_tag(key, nonce, aad, ct);
    let mut diff = 0u8;
    for i in 0..16 {
        diff |= tag[i] ^ expected[i];
    }
    if diff != 0 {
        return Err("AEAD authentication tag verification failed".into());
    }
    let mut pt = ct.to_vec();
    chacha20_xor(key, 1, nonce, &mut pt);
    Ok(pt)
}

/// Stateful bidirectional AEAD session cipher with monotonic 64-bit nonces.
pub struct SessionCipher {
    tx_key: [u8; 32],
    rx_key: [u8; 32],
    tx_seq: u64,
    rx_seq: u64,
}

impl SessionCipher {
    pub fn new(tx_key: [u8; 32], rx_key: [u8; 32]) -> Self {
        Self {
            tx_key,
            rx_key,
            tx_seq: 0,
            rx_seq: 0,
        }
    }

    pub fn new_client(
        cluster_key: &[u8; 32],
        client_nonce: &[u8; 32],
        server_nonce: &[u8; 32],
    ) -> Self {
        let (c2s, s2c) = derive_session_keys(cluster_key, client_nonce, server_nonce);
        Self::new(c2s, s2c)
    }

    pub fn new_server(
        cluster_key: &[u8; 32],
        client_nonce: &[u8; 32],
        server_nonce: &[u8; 32],
    ) -> Self {
        let (c2s, s2c) = derive_session_keys(cluster_key, client_nonce, server_nonce);
        Self::new(s2c, c2s)
    }

    pub fn new_client_pfs(
        cluster_key: &[u8; 32],
        ecdh_shared: &[u8; 32],
        client_nonce: &[u8; 32],
        server_nonce: &[u8; 32],
    ) -> Self {
        let (c2s, s2c) =
            derive_session_keys_pfs(cluster_key, ecdh_shared, client_nonce, server_nonce);
        Self::new(c2s, s2c)
    }

    pub fn new_server_pfs(
        cluster_key: &[u8; 32],
        ecdh_shared: &[u8; 32],
        client_nonce: &[u8; 32],
        server_nonce: &[u8; 32],
    ) -> Self {
        let (c2s, s2c) =
            derive_session_keys_pfs(cluster_key, ecdh_shared, client_nonce, server_nonce);
        Self::new(s2c, c2s)
    }

    fn nonce(seq: u64) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[4..12].copy_from_slice(&seq.to_le_bytes());
        n
    }

    pub fn seal(&mut self, aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let n = Self::nonce(self.tx_seq);
        self.tx_seq = self.tx_seq.wrapping_add(1);
        aead_seal(&self.tx_key, &n, aad, plaintext)
    }

    pub fn open(&mut self, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, String> {
        let n = Self::nonce(self.rx_seq);
        let pt = aead_open(&self.rx_key, &n, aad, sealed)?;
        self.rx_seq = self.rx_seq.wrapping_add(1);
        Ok(pt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex_encode;

    #[test]
    fn rfc4231_hmac_sha256_vector() {
        let mac = hmac_sha256(b"Jefe", &[b"what do ya want for nothing?"]);
        assert_eq!(
            hex_encode(&mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn rfc8439_section_2_8_2_aead_vector() {
        let key: [u8; 32] = [
            0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8d,
            0x8e, 0x8f, 0x90, 0x91, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0x9b,
            0x9c, 0x9d, 0x9e, 0x9f,
        ];
        let nonce: [u8; 12] = [
            0x07, 0x00, 0x00, 0x00, 0x40, 0x41, 0x42, 0x43, 0x44, 0x45, 0x46, 0x47,
        ];
        let aad: [u8; 12] = [
            0x50, 0x51, 0x52, 0x53, 0xc0, 0xc1, 0xc2, 0xc3, 0xc4, 0xc5, 0xc6, 0xc7,
        ];
        let pt = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let sealed = aead_seal(&key, &nonce, &aad, pt);
        let expected_tag = "1ae10b594f09e26a7e902ecbd0600691";
        assert_eq!(hex_encode(&sealed[sealed.len() - 16..]), expected_tag);
        assert_eq!(aead_open(&key, &nonce, &aad, &sealed).unwrap(), pt);

        // Flip 1 bit in ciphertext -> must fail authentication!
        let mut tampered = sealed.clone();
        tampered[5] ^= 0x01;
        assert!(aead_open(&key, &nonce, &aad, &tampered).is_err());
    }
}
