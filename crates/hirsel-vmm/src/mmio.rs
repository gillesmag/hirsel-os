//! Guest x86-64 page-table walker and MMIO instruction decoder for `ZX_GUEST_TRAP_MEM`.
//!
//! Used by `hirsel-vmm` to decode Linux `virtio_mmio` `readb`/`readw`/`readl` and
//! `writeb`/`writew`/`writel` accesses directly from guest RAM without needing
//! external disassembler crates.

/// Decoded x86-64 register index matching Intel/AMD encoding (`0=RAX .. 15=R15`).
pub const REG_RAX: u8 = 0;
pub const REG_RCX: u8 = 1;
pub const REG_RDX: u8 = 2;
pub const REG_RBX: u8 = 3;
pub const REG_RSP: u8 = 4;
pub const REG_RBP: u8 = 5;
pub const REG_RSI: u8 = 6;
pub const REG_RDI: u8 = 7;

/// Decoded MMIO memory operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MmioAccess {
    /// Load `size` bytes from MMIO into register `reg`.
    /// If `zero_extend` is true (e.g. `movzx` or 32-bit `mov r32, [m]`), upper bits are zeroed.
    Read {
        reg: u8,
        size: u8,
        zero_extend: bool,
    },
    /// Store `size` bytes from register `reg` into MMIO.
    WriteReg {
        reg: u8,
        size: u8,
    },
    /// Store `size` bytes of immediate `value` into MMIO.
    WriteImm {
        value: u64,
        size: u8,
    },
}

fn read_u64_le(ram: &[u8], gpa: u64) -> Option<u64> {
    let off = usize::try_from(gpa).ok()?;
    let slice = ram.get(off..off + 8)?;
    Some(u64::from_le_bytes(slice.try_into().unwrap()))
}

/// Translates a 64-bit guest virtual address `vaddr` to a guest physical address
/// using the guest's 4-level page table rooted at `cr3`.
pub fn guest_vaddr_to_gpa(ram: &[u8], cr3: u64, vaddr: u64) -> Option<u64> {
    const PTE_PRESENT: u64 = 1 << 0;
    const PTE_PS: u64 = 1 << 7;
    const ADDR_MASK: u64 = 0x000f_ffff_ffff_f000;

    let pml4_idx = ((vaddr >> 39) & 0x1ff) * 8;
    let pml4e = read_u64_le(ram, (cr3 & ADDR_MASK) + pml4_idx)?;
    if (pml4e & PTE_PRESENT) == 0 {
        return None;
    }

    let pdpt_idx = ((vaddr >> 30) & 0x1ff) * 8;
    let pdpte = read_u64_le(ram, (pml4e & ADDR_MASK) + pdpt_idx)?;
    if (pdpte & PTE_PRESENT) == 0 {
        return None;
    }
    if (pdpte & PTE_PS) != 0 {
        let base = pdpte & 0x000f_ffff_c000_0000;
        return Some(base | (vaddr & 0x3fff_ffff));
    }

    let pd_idx = ((vaddr >> 21) & 0x1ff) * 8;
    let pde = read_u64_le(ram, (pdpte & ADDR_MASK) + pd_idx)?;
    if (pde & PTE_PRESENT) == 0 {
        return None;
    }
    if (pde & PTE_PS) != 0 {
        let base = pde & 0x000f_ffff_ffe0_0000;
        return Some(base | (vaddr & 0x1f_ffff));
    }

    let pt_idx = ((vaddr >> 12) & 0x1ff) * 8;
    let pte = read_u64_le(ram, (pde & ADDR_MASK) + pt_idx)?;
    if (pte & PTE_PRESENT) == 0 {
        return None;
    }
    Some((pte & ADDR_MASK) | (vaddr & 0xfff))
}

/// Copies up to `out.len()` instruction bytes starting at guest virtual address `rip`.
pub fn fetch_guest_inst_bytes(ram: &[u8], cr3: u64, rip: u64, out: &mut [u8]) -> usize {
    let mut fetched = 0usize;
    while fetched < out.len() {
        let va = rip.wrapping_add(fetched as u64);
        let Some(gpa) = guest_vaddr_to_gpa(ram, cr3, va) else {
            break;
        };
        let off = gpa as usize;
        if off >= ram.len() {
            break;
        }
        let page_rem = 4096 - (off & 0xfff);
        let chunk = (out.len() - fetched).min(page_rem).min(ram.len() - off);
        out[fetched..fetched + chunk].copy_from_slice(&ram[off..off + chunk]);
        fetched += chunk;
    }
    fetched
}

/// Decodes a 64-bit x86-64 MMIO load/store instruction (`mov`, `movzx`).
pub fn decode_mmio_inst(bytes: &[u8]) -> Result<MmioAccess, String> {
    let mut i = 0usize;
    let mut op_override = false;

    while i < bytes.len() {
        match bytes[i] {
            0x66 => op_override = true,
            0x67 | 0x26 | 0x2E | 0x36 | 0x3E | 0x64 | 0x65 | 0xF0 | 0xF2 | 0xF3 => {}
            _ => break,
        }
        i += 1;
    }

    let mut rex = 0u8;
    if i < bytes.len() && (bytes[i] & 0xF0) == 0x40 {
        rex = bytes[i];
        i += 1;
    }
    let rex_w = (rex & 0x08) != 0;
    let rex_r = if (rex & 0x04) != 0 { 8u8 } else { 0u8 };

    if i >= bytes.len() {
        return Err("empty MMIO instruction".into());
    }
    let op = bytes[i];
    i += 1;

    let mut two_byte = false;
    let mut op2 = 0u8;
    if op == 0x0F {
        if i >= bytes.len() {
            return Err("truncated 0x0F MMIO instruction".into());
        }
        two_byte = true;
        op2 = bytes[i];
        i += 1;
    }

    if i >= bytes.len() {
        return Err(format!("missing ModR/M byte for opcode {op:#04x}"));
    }
    let modrm = bytes[i];
    i += 1;

    let mod_bits = modrm >> 6;
    let reg = ((modrm >> 3) & 7) | rex_r;
    let rm = modrm & 7;

    // Skip SIB + displacement so `i` points directly at any immediate operand.
    if mod_bits != 3 {
        if rm == 4 {
            if i >= bytes.len() {
                return Err("truncated SIB byte".into());
            }
            let sib = bytes[i];
            i += 1;
            if mod_bits == 0 && (sib & 7) == 5 {
                i += 4;
            }
        } else if mod_bits == 0 && rm == 5 {
            i += 4;
        }
        if mod_bits == 1 {
            i += 1;
        } else if mod_bits == 2 {
            i += 4;
        }
    }

    let word_size = if rex_w {
        8
    } else if op_override {
        2
    } else {
        4
    };

    if two_byte {
        return match op2 {
            0xB6 => Ok(MmioAccess::Read {
                reg,
                size: 1,
                zero_extend: true,
            }),
            0xB7 => Ok(MmioAccess::Read {
                reg,
                size: 2,
                zero_extend: true,
            }),
            _ => Err(format!("unsupported 2-byte MMIO opcode 0x0F {op2:#04x}")),
        };
    }

    match op {
        // mov r/m8, r8
        0x88 => Ok(MmioAccess::WriteReg { reg, size: 1 }),
        // mov r/m{16,32,64}, r{16,32,64}
        0x89 => Ok(MmioAccess::WriteReg {
            reg,
            size: word_size,
        }),
        // mov r8, r/m8
        0x8A => Ok(MmioAccess::Read {
            reg,
            size: 1,
            zero_extend: false,
        }),
        // mov r{16,32,64}, r/m{16,32,64}
        0x8B => Ok(MmioAccess::Read {
            reg,
            size: word_size,
            zero_extend: word_size >= 4,
        }),
        // mov r/m8, imm8
        0xC6 => {
            let imm = *bytes
                .get(i)
                .ok_or_else(|| "truncated imm8 in C6".to_string())?;
            Ok(MmioAccess::WriteImm {
                value: u64::from(imm),
                size: 1,
            })
        }
        // mov r/m{16,32,64}, imm{16,32}
        0xC7 => {
            if op_override && !rex_w {
                let s = bytes
                    .get(i..i + 2)
                    .ok_or_else(|| "truncated imm16 in C7".to_string())?;
                let v = u16::from_le_bytes(s.try_into().unwrap());
                Ok(MmioAccess::WriteImm {
                    value: u64::from(v),
                    size: 2,
                })
            } else {
                let s = bytes
                    .get(i..i + 4)
                    .ok_or_else(|| "truncated imm32 in C7".to_string())?;
                let v = i32::from_le_bytes(s.try_into().unwrap());
                let val = if rex_w { v as i64 as u64 } else { u64::from(v as u32) };
                Ok(MmioAccess::WriteImm {
                    value: val,
                    size: word_size,
                })
            }
        }
        _ => Err(format!("unsupported MMIO opcode {op:#04x}")),
    }
}

/// Reads register `reg` (`0..=15`) from a `zx_vcpu_state_t`.
#[cfg(target_os = "fuchsia")]
pub fn get_gpr(state: &zx_sys::zx_vcpu_state_t, reg: u8) -> u64 {
    match reg {
        0 => state.rax,
        1 => state.rcx,
        2 => state.rdx,
        3 => state.rbx,
        4 => state.rsp,
        5 => state.rbp,
        6 => state.rsi,
        7 => state.rdi,
        8 => state.r8,
        9 => state.r9,
        10 => state.r10,
        11 => state.r11,
        12 => state.r12,
        13 => state.r13,
        14 => state.r14,
        15 => state.r15,
        _ => 0,
    }
}

/// Writes `value` into register `reg` (`0..=15`) honoring x86-64 partial-register semantics.
#[cfg(target_os = "fuchsia")]
pub fn set_gpr(state: &mut zx_sys::zx_vcpu_state_t, reg: u8, size: u8, zero_extend: bool, val: u64) {
    let slot = match reg {
        0 => &mut state.rax,
        1 => &mut state.rcx,
        2 => &mut state.rdx,
        3 => &mut state.rbx,
        4 => &mut state.rsp,
        5 => &mut state.rbp,
        6 => &mut state.rsi,
        7 => &mut state.rdi,
        8 => &mut state.r8,
        9 => &mut state.r9,
        10 => &mut state.r10,
        11 => &mut state.r11,
        12 => &mut state.r12,
        13 => &mut state.r13,
        14 => &mut state.r14,
        15 => &mut state.r15,
        _ => return,
    };
    if zero_extend || size >= 4 {
        let mask = match size {
            1 => 0xffu64,
            2 => 0xffffu64,
            4 => 0xffff_ffffu64,
            _ => u64::MAX,
        };
        *slot = val & mask;
    } else if size == 1 {
        *slot = (*slot & !0xff) | (val & 0xff);
    } else if size == 2 {
        *slot = (*slot & !0xffff) | (val & 0xffff);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_common_virtio_mmio_instructions() {
        // mov eax, [rdi]  (8B 07)
        assert_eq!(
            decode_mmio_inst(&[0x8B, 0x07]).unwrap(),
            MmioAccess::Read {
                reg: REG_RAX,
                size: 4,
                zero_extend: true
            }
        );
        // mov [rdi+0x50], esi  (89 77 50)
        assert_eq!(
            decode_mmio_inst(&[0x89, 0x77, 0x50]).unwrap(),
            MmioAccess::WriteReg {
                reg: REG_RSI,
                size: 4
            }
        );
        // mov r12d, [rax+0x10]  (44 8B 60 10)
        assert_eq!(
            decode_mmio_inst(&[0x44, 0x8B, 0x60, 0x10]).unwrap(),
            MmioAccess::Read {
                reg: 12,
                size: 4,
                zero_extend: true
            }
        );
        // movzx eax, byte ptr [rdi+0x100]  (0F B6 87 00 01 00 00)
        assert_eq!(
            decode_mmio_inst(&[0x0F, 0xB6, 0x87, 0x00, 0x01, 0x00, 0x00]).unwrap(),
            MmioAccess::Read {
                reg: REG_RAX,
                size: 1,
                zero_extend: true
            }
        );
        // movzx edx, word ptr [rax+0x106]  (0F B7 90 06 01 00 00)
        assert_eq!(
            decode_mmio_inst(&[0x0F, 0xB7, 0x90, 0x06, 0x01, 0x00, 0x00]).unwrap(),
            MmioAccess::Read {
                reg: REG_RDX,
                size: 2,
                zero_extend: true
            }
        );
        // mov dword ptr [rdi+0x70], 0x0f  (C7 47 70 0F 00 00 00)
        assert_eq!(
            decode_mmio_inst(&[0xC7, 0x47, 0x70, 0x0F, 0x00, 0x00, 0x00]).unwrap(),
            MmioAccess::WriteImm {
                value: 0x0f,
                size: 4
            }
        );
        // mov word ptr [rcx], ax  (66 89 01)
        assert_eq!(
            decode_mmio_inst(&[0x66, 0x89, 0x01]).unwrap(),
            MmioAccess::WriteReg {
                reg: REG_RAX,
                size: 2
            }
        );
    }
}
