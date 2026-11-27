//! Pure-Rust, zero-dependency RFC 7748 X25519 Elliptic-Curve Diffie-Hellman (Curve25519)
//! for Perfect Forward Secrecy (PFS) on Hirsel-OS control-plane sessions.

/// Field element mod `p = 2^255 - 19` in radix-2^51 (5 limbs, each < 2^52).
#[derive(Clone, Copy)]
struct Fe([u64; 5]);

const MASK51: u64 = (1u64 << 51) - 1;

impl Fe {
    const ZERO: Self = Self([0, 0, 0, 0, 0]);
    const ONE: Self = Self([1, 0, 0, 0, 0]);

    fn from_bytes(b: &[u8; 32]) -> Self {
        let l0 = u64::from_le_bytes(b[0..8].try_into().unwrap()) & MASK51;
        let l1 = (u64::from_le_bytes(b[6..14].try_into().unwrap()) >> 3) & MASK51;
        let l2 = (u64::from_le_bytes(b[12..20].try_into().unwrap()) >> 6) & MASK51;
        let l3 = (u64::from_le_bytes(b[19..27].try_into().unwrap()) >> 1) & MASK51;
        let l4 = (u64::from_le_bytes(b[24..32].try_into().unwrap()) >> 12) & MASK51;
        Self([l0, l1, l2, l3, l4])
    }

    fn to_bytes(self) -> [u8; 32] {
        let mut h = self.0;
        // Full carry reduction.
        for _ in 0..2 {
            let mut c = h[0] >> 51;
            h[0] &= MASK51;
            h[1] += c;
            c = h[1] >> 51;
            h[1] &= MASK51;
            h[2] += c;
            c = h[2] >> 51;
            h[2] &= MASK51;
            h[3] += c;
            c = h[3] >> 51;
            h[3] &= MASK51;
            h[4] += c;
            c = h[4] >> 51;
            h[4] &= MASK51;
            h[0] += c * 19;
        }
        // Conditional subtraction of p = 2^255 - 19.
        let mut q = (h[0] + 19) >> 51;
        q = (h[1] + q) >> 51;
        q = (h[2] + q) >> 51;
        q = (h[3] + q) >> 51;
        q = (h[4] + q) >> 51;
        h[0] += 19 * q;
        let mut c = h[0] >> 51;
        h[0] &= MASK51;
        h[1] += c;
        c = h[1] >> 51;
        h[1] &= MASK51;
        h[2] += c;
        c = h[2] >> 51;
        h[2] &= MASK51;
        h[3] += c;
        c = h[3] >> 51;
        h[3] &= MASK51;
        h[4] += c;
        h[4] &= MASK51;

        let mut out = [0u8; 32];
        let w0 = h[0] | (h[1] << 51);
        let w1 = (h[1] >> 13) | (h[2] << 38);
        let w2 = (h[2] >> 26) | (h[3] << 25);
        let w3 = (h[3] >> 39) | (h[4] << 12);
        out[0..8].copy_from_slice(&w0.to_le_bytes());
        out[8..16].copy_from_slice(&w1.to_le_bytes());
        out[16..24].copy_from_slice(&w2.to_le_bytes());
        out[24..32].copy_from_slice(&w3.to_le_bytes());
        out
    }

    #[inline]
    fn add(self, o: Self) -> Self {
        Self([
            self.0[0] + o.0[0],
            self.0[1] + o.0[1],
            self.0[2] + o.0[2],
            self.0[3] + o.0[3],
            self.0[4] + o.0[4],
        ])
    }

    #[inline]
    fn sub(self, o: Self) -> Self {
        // Add 2p to keep all limbs strictly positive.
        const P0: u64 = 0x000f_ffff_ffff_ffda; // 2 * (2^51 - 19)
        const P1: u64 = 0x000f_ffff_ffff_fffe; // 2 * (2^51 - 1)
        Self([
            (self.0[0] + P0) - o.0[0],
            (self.0[1] + P1) - o.0[1],
            (self.0[2] + P1) - o.0[2],
            (self.0[3] + P1) - o.0[3],
            (self.0[4] + P1) - o.0[4],
        ])
    }

    fn mul(self, o: Self) -> Self {
        let a = self.0;
        let b = o.0;
        let b1_19 = (b[1] as u128) * 19;
        let b2_19 = (b[2] as u128) * 19;
        let b3_19 = (b[3] as u128) * 19;
        let b4_19 = (b[4] as u128) * 19;

        let t0 = (a[0] as u128) * (b[0] as u128)
            + (a[1] as u128) * b4_19
            + (a[2] as u128) * b3_19
            + (a[3] as u128) * b2_19
            + (a[4] as u128) * b1_19;
        let t1 = (a[0] as u128) * (b[1] as u128)
            + (a[1] as u128) * (b[0] as u128)
            + (a[2] as u128) * b4_19
            + (a[3] as u128) * b3_19
            + (a[4] as u128) * b2_19;
        let t2 = (a[0] as u128) * (b[2] as u128)
            + (a[1] as u128) * (b[1] as u128)
            + (a[2] as u128) * (b[0] as u128)
            + (a[3] as u128) * b4_19
            + (a[4] as u128) * b3_19;
        let t3 = (a[0] as u128) * (b[3] as u128)
            + (a[1] as u128) * (b[2] as u128)
            + (a[2] as u128) * (b[1] as u128)
            + (a[3] as u128) * (b[0] as u128)
            + (a[4] as u128) * b4_19;
        let t4 = (a[0] as u128) * (b[4] as u128)
            + (a[1] as u128) * (b[3] as u128)
            + (a[2] as u128) * (b[2] as u128)
            + (a[3] as u128) * (b[1] as u128)
            + (a[4] as u128) * (b[0] as u128);

        let mut r0 = (t0 as u64) & MASK51;
        let t1 = t1 + (t0 >> 51);
        let mut r1 = (t1 as u64) & MASK51;
        let t2 = t2 + (t1 >> 51);
        let r2 = (t2 as u64) & MASK51;
        let t3 = t3 + (t2 >> 51);
        let r3 = (t3 as u64) & MASK51;
        let t4 = t4 + (t3 >> 51);
        let r4 = (t4 as u64) & MASK51;

        r0 += ((t4 >> 51) as u64) * 19;
        r1 += r0 >> 51;
        r0 &= MASK51;
        Self([r0, r1, r2, r3, r4])
    }

    #[inline]
    fn sq(self) -> Self {
        self.mul(self)
    }

    fn mul_a24(self) -> Self {
        // a24 = 121665
        const A24: u128 = 121_665;
        let t0 = (self.0[0] as u128) * A24;
        let t1 = (self.0[1] as u128) * A24 + (t0 >> 51);
        let t2 = (self.0[2] as u128) * A24 + (t1 >> 51);
        let t3 = (self.0[3] as u128) * A24 + (t2 >> 51);
        let t4 = (self.0[4] as u128) * A24 + (t3 >> 51);
        let mut r0 = (t0 as u64) & MASK51;
        let mut r1 = (t1 as u64) & MASK51;
        let r2 = (t2 as u64) & MASK51;
        let r3 = (t3 as u64) & MASK51;
        let r4 = (t4 as u64) & MASK51;
        r0 += ((t4 >> 51) as u64) * 19;
        r1 += r0 >> 51;
        r0 &= MASK51;
        Self([r0, r1, r2, r3, r4])
    }

    fn invert(self) -> Self {
        // Compute self^(p-2) = self^(2^255 - 21) via standard addition chain.
        let z2 = self.sq();
        let z9 = z2.sq().sq().mul(self);
        let z11 = z9.mul(z2);
        let t0 = z11.sq().mul(z9); // 2^5 - 1
        let mut t1 = t0;
        for _ in 0..5 {
            t1 = t1.sq();
        }
        let z2_10_0 = t1.mul(t0); // 2^10 - 1
        let mut t2 = z2_10_0;
        for _ in 0..10 {
            t2 = t2.sq();
        }
        let z2_20_0 = t2.mul(z2_10_0);
        let mut t3 = z2_20_0;
        for _ in 0..20 {
            t3 = t3.sq();
        }
        let z2_40_0 = t3.mul(z2_20_0);
        let mut t4 = z2_40_0;
        for _ in 0..10 {
            t4 = t4.sq();
        }
        let z2_50_0 = t4.mul(z2_10_0);
        let mut t5 = z2_50_0;
        for _ in 0..50 {
            t5 = t5.sq();
        }
        let z2_100_0 = t5.mul(z2_50_0);
        let mut t6 = z2_100_0;
        for _ in 0..100 {
            t6 = t6.sq();
        }
        let z2_200_0 = t6.mul(z2_100_0);
        let mut t7 = z2_200_0;
        for _ in 0..50 {
            t7 = t7.sq();
        }
        let z2_250_0 = t7.mul(z2_50_0);
        let mut t8 = z2_250_0;
        for _ in 0..5 {
            t8 = t8.sq();
        }
        t8.mul(z11)
    }

    fn cswap(swap: u64, a: &mut Self, b: &mut Self) {
        let mask = 0u64.wrapping_sub(swap & 1);
        for i in 0..5 {
            let t = mask & (a.0[i] ^ b.0[i]);
            a.0[i] ^= t;
            b.0[i] ^= t;
        }
    }
}

/// RFC 7748 X25519 scalar multiplication: `scalar * u_coord`.
pub fn x25519(scalar: &[u8; 32], u_coord: &[u8; 32]) -> [u8; 32] {
    let mut k = *scalar;
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;

    let mut u_bytes = *u_coord;
    u_bytes[31] &= 127;
    let x_1 = Fe::from_bytes(&u_bytes);

    let mut x_2 = Fe::ONE;
    let mut z_2 = Fe::ZERO;
    let mut x_3 = x_1;
    let mut z_3 = Fe::ONE;
    let mut swap = 0u64;

    for t in (0..255).rev() {
        let k_t = u64::from((k[t >> 3] >> (t & 7)) & 1);
        swap ^= k_t;
        Fe::cswap(swap, &mut x_2, &mut x_3);
        Fe::cswap(swap, &mut z_2, &mut z_3);
        swap = k_t;

        let a = x_2.add(z_2);
        let aa = a.sq();
        let b = x_2.sub(z_2);
        let bb = b.sq();
        let e = aa.sub(bb);
        let c = x_3.add(z_3);
        let d = x_3.sub(z_3);
        let da = d.mul(a);
        let cb = c.mul(b);
        x_3 = da.add(cb).sq();
        z_3 = x_1.mul(da.sub(cb).sq());
        x_2 = aa.mul(bb);
        z_2 = e.mul(aa.add(e.mul_a24()));
    }

    Fe::cswap(swap, &mut x_2, &mut x_3);
    Fe::cswap(swap, &mut z_2, &mut z_3);
    x_2.mul(z_2.invert()).to_bytes()
}

/// Derives the 32-byte X25519 public key from a 32-byte secret scalar.
pub fn x25519_public_key(secret: &[u8; 32]) -> [u8; 32] {
    let mut basepoint = [0u8; 32];
    basepoint[0] = 9;
    x25519(secret, &basepoint)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hex_encode;

    fn hex_decode_32(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for i in 0..32 {
            out[i] = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).unwrap();
        }
        out
    }

    #[test]
    fn rfc7748_section_6_1_vectors() {
        // RFC 7748 Section 6.1 test vector
        let alice_priv = hex_decode_32(
            "77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a",
        );
        let alice_pub = x25519_public_key(&alice_priv);
        assert_eq!(
            hex_encode(&alice_pub),
            "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"
        );

        let bob_priv = hex_decode_32(
            "5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb",
        );
        let bob_pub = x25519_public_key(&bob_priv);
        assert_eq!(
            hex_encode(&bob_pub),
            "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f"
        );

        let ss_a = x25519(&alice_priv, &bob_pub);
        let ss_b = x25519(&bob_priv, &alice_pub);
        assert_eq!(ss_a, ss_b);
        assert_eq!(
            hex_encode(&ss_a),
            "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742"
        );
    }
}
