//! Minimal Zircon vDSO syscall ABI bindings (`libzircon.so` + `libc.so` startup helpers).
//!
//! Kept `#![no_std]` so `hirsel-init`, `hirsel-vmm`, and `hirsel-core-rt`'s
//! `host/fuchsia` backend can share one tiny, dependency-free syscall layer.

#![no_std]
#![allow(non_camel_case_types)]

pub type zx_status_t = i32;
pub type zx_handle_t = u32;
pub type zx_rights_t = u32;
pub type zx_signals_t = u32;
pub type zx_time_t = i64;
pub type zx_duration_t = i64;
pub type zx_vaddr_t = usize;
pub type zx_paddr_t = usize;
pub type zx_gpaddr_t = usize;
pub type zx_koid_t = u64;
pub type zx_vm_option_t = u32;

pub const ZX_HANDLE_INVALID: zx_handle_t = 0;
pub const ZX_TIME_INFINITE: zx_time_t = i64::MAX;

// Common zx_status_t codes.
pub const ZX_OK: zx_status_t = 0;
pub const ZX_ERR_INTERNAL: zx_status_t = -1;
pub const ZX_ERR_NOT_SUPPORTED: zx_status_t = -2;
pub const ZX_ERR_NO_RESOURCES: zx_status_t = -3;
pub const ZX_ERR_NO_MEMORY: zx_status_t = -4;
pub const ZX_ERR_INVALID_ARGS: zx_status_t = -10;
pub const ZX_ERR_BAD_HANDLE: zx_status_t = -11;
pub const ZX_ERR_WRONG_TYPE: zx_status_t = -12;
pub const ZX_ERR_BAD_STATE: zx_status_t = -20;
pub const ZX_ERR_TIMED_OUT: zx_status_t = -21;
pub const ZX_ERR_SHOULD_WAIT: zx_status_t = -22;
pub const ZX_ERR_CANCELED: zx_status_t = -23;
pub const ZX_ERR_PEER_CLOSED: zx_status_t = -24;
pub const ZX_ERR_NOT_FOUND: zx_status_t = -25;
pub const ZX_ERR_ACCESS_DENIED: zx_status_t = -30;

// Standard Zircon handle rights.
pub const ZX_RIGHT_NONE: zx_rights_t = 0;
pub const ZX_RIGHT_DUPLICATE: zx_rights_t = 1 << 0;
pub const ZX_RIGHT_TRANSFER: zx_rights_t = 1 << 1;
pub const ZX_RIGHT_READ: zx_rights_t = 1 << 2;
pub const ZX_RIGHT_WRITE: zx_rights_t = 1 << 3;
pub const ZX_RIGHT_EXECUTE: zx_rights_t = 1 << 4;
pub const ZX_RIGHT_MAP: zx_rights_t = 1 << 5;
pub const ZX_RIGHT_SAME_RIGHTS: zx_rights_t = 1 << 31;

// Common object signals.
pub const ZX_CHANNEL_READABLE: zx_signals_t = 1 << 0;
pub const ZX_CHANNEL_WRITABLE: zx_signals_t = 1 << 1;
pub const ZX_CHANNEL_PEER_CLOSED: zx_signals_t = 1 << 2;
pub const ZX_SOCKET_READABLE: zx_signals_t = 1 << 0;
pub const ZX_SOCKET_WRITABLE: zx_signals_t = 1 << 1;
pub const ZX_SOCKET_PEER_CLOSED: zx_signals_t = 1 << 2;
pub const ZX_PROCESS_TERMINATED: zx_signals_t = 1 << 3;
pub const ZX_TASK_TERMINATED: zx_signals_t = 1 << 3;

// Socket creation options.
pub const ZX_SOCKET_STREAM: u32 = 0;

// VMO child options.
pub const ZX_VMO_CHILD_SNAPSHOT: u32 = 1 << 0;
pub const ZX_VMO_CHILD_SNAPSHOT_AT_LEAST_ON_WRITE: u32 = 1 << 4;
pub const ZX_VMO_CHILD_NO_WRITE: u32 = 1 << 5;

// VMAR options.
pub const ZX_VM_PERM_READ: zx_vm_option_t = 1 << 0;
pub const ZX_VM_PERM_WRITE: zx_vm_option_t = 1 << 1;
pub const ZX_VM_PERM_EXECUTE: zx_vm_option_t = 1 << 2;
pub const ZX_VM_SPECIFIC: zx_vm_option_t = 1 << 4;
pub const ZX_VM_CAN_MAP_SPECIFIC: zx_vm_option_t = 1 << 6;
pub const ZX_VM_CAN_MAP_READ: zx_vm_option_t = 1 << 7;
pub const ZX_VM_CAN_MAP_WRITE: zx_vm_option_t = 1 << 8;
pub const ZX_VM_CAN_MAP_EXECUTE: zx_vm_option_t = 1 << 9;

// System resource creation constants (<zircon/syscalls/resource.h>).
pub const ZX_RSRC_KIND_SYSTEM: u32 = 5;
pub const ZX_RSRC_SYSTEM_HYPERVISOR_BASE: u64 = 0;
pub const ZX_RSRC_SYSTEM_VMEX_BASE: u64 = 1;
pub const ZX_RSRC_SYSTEM_IOMMU_BASE: u64 = 8;

pub const ZX_IOMMU_TYPE_STUB: u32 = 0;

pub const ZX_BTI_PERM_READ: u32 = 1 << 0;
pub const ZX_BTI_PERM_WRITE: u32 = 1 << 1;
pub const ZX_BTI_PERM_EXECUTE: u32 = 1 << 2;
pub const ZX_BTI_CONTIGUOUS: u32 = 1 << 4;

pub const ZX_CACHE_POLICY_CACHED: u32 = 0;
pub const ZX_CACHE_POLICY_UNCACHED: u32 = 1;
pub const ZX_CACHE_POLICY_UNCACHED_DEVICE: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct zx_iommu_desc_stub_t {
    pub reserved: u8,
}

// Object properties & info topics (<zircon/syscalls/object.h>).
pub const ZX_PROP_NAME: u32 = 3;
pub const ZX_INFO_PROCESS: u32 = 3 | (1 << 28);
pub const ZX_INFO_VMAR: u32 = 7;

// Job basic policy (<zircon/syscalls/policy.h>).
pub const ZX_JOB_POL_RELATIVE: u32 = 0;
pub const ZX_JOB_POL_ABSOLUTE: u32 = 1;
pub const ZX_JOB_POL_BASIC_V2: u32 = 0x0100_0000;
pub const ZX_POL_VMAR_WX: u32 = 2;
pub const ZX_POL_NEW_PROCESS: u32 = 12;
pub const ZX_POL_AMBIENT_MARK_VMO_EXEC: u32 = 15;
pub const ZX_POL_ACTION_DENY: u32 = 1;
pub const ZX_POL_OVERRIDE_DENY: u32 = 1;

// Hypervisor trap kinds (zx_guest_set_trap).
pub const ZX_GUEST_TRAP_BELL: u32 = 0;
pub const ZX_GUEST_TRAP_MEM: u32 = 1;
pub const ZX_GUEST_TRAP_IO: u32 = 2;

pub const ZX_VCPU_STATE: u32 = 0;
pub const ZX_VCPU_IO: u32 = 1;

pub const ZX_PKT_TYPE_GUEST_BELL: u32 = 3;
pub const ZX_PKT_TYPE_GUEST_MEM: u32 = 4;
pub const ZX_PKT_TYPE_GUEST_IO: u32 = 5;
pub const ZX_PKT_TYPE_GUEST_VCPU: u32 = 6;

// Zircon processargs protocol constants (<zircon/processargs.h>).
pub const ZX_PROCARGS_PROTOCOL: u32 = 0x4150_585d; // MXPA (legacy Magenta/Zircon ABI)
pub const ZX_PROCARGS_VERSION: u32 = 0x0000_1000;

pub const PA_PROC_SELF: u32 = 0x01;
pub const PA_THREAD_SELF: u32 = 0x02;
pub const PA_JOB_DEFAULT: u32 = 0x03;
pub const PA_VMAR_ROOT: u32 = 0x04;
pub const PA_VMAR_LOADED: u32 = 0x05;
pub const PA_LDSVC_LOADER: u32 = 0x10;
pub const PA_VMO_VDSO: u32 = 0x11;
pub const PA_VMO_STACK: u32 = 0x13;
pub const PA_VMO_EXECUTABLE: u32 = 0x14;
pub const PA_VMO_BOOTDATA: u32 = 0x1a;
pub const PA_VMO_BOOTFS: u32 = 0x1b;
pub const PA_VMO_KERNEL_FILE: u32 = 0x1c;
pub const PA_RESOURCE: u32 = 0x23;
pub const PA_MMIO_RESOURCE: u32 = 0x24;
pub const PA_IRQ_RESOURCE: u32 = 0x25;
pub const PA_IOPORT_RESOURCE: u32 = 0x26;
pub const PA_SYSTEM_RESOURCE: u32 = 0x28;
pub const PA_FD: u32 = 0x30;
pub const PA_USER0: u32 = 0xF0;

pub const FDIO_FLAG_USE_FOR_STDIO: u16 = 0x8000;

#[inline]
pub const fn pa_hnd(kind: u32, arg: u16) -> u32 {
    (kind & 0xff) | ((arg as u32) << 16)
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct zx_proc_args_t {
    pub protocol: u32,
    pub version: u32,
    pub handle_info_off: u32,
    pub args_off: u32,
    pub args_num: u32,
    pub environ_off: u32,
    pub environ_num: u32,
    pub names_off: u32,
    pub names_num: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct zx_info_process_t {
    pub return_code: i64,
    pub start_time: zx_time_t,
    pub flags: u32,
    pub _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct zx_info_vmar_t {
    pub base: usize,
    pub len: usize,
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct zx_policy_basic_v2_t {
    pub condition: u32,
    pub action: u32,
    pub flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct zx_port_packet_t {
    pub key: u64,
    pub packet_type: u32,
    pub status: zx_status_t,
    pub payload: [u8; 32],
}

impl Default for zx_port_packet_t {
    fn default() -> Self {
        Self {
            key: 0,
            packet_type: 0,
            status: 0,
            payload: [0; 32],
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct zx_vcpu_state_t {
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rbx: u64,
    pub rsp: u64,
    pub rbp: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8: u64,
    pub r9: u64,
    pub r10: u64,
    pub r11: u64,
    pub r12: u64,
    pub r13: u64,
    pub r14: u64,
    pub r15: u64,
    pub rflags: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct zx_vcpu_io_t {
    pub access_size: u8,
    pub _pad: [u8; 3],
    pub data: [u8; 4],
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct zx_packet_guest_bell_t {
    pub addr: u64,
    pub reserved0: u64,
    pub reserved1: u64,
    pub reserved2: u64,
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct zx_packet_guest_mem_t {
    pub addr: u64,
    pub cr3: u64,
    pub rip: u64,
    pub instruction_size: u8,
    pub default_operand_size: u8,
    pub reserved: [u8; 6],
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct zx_packet_guest_io_t {
    pub port: u16,
    pub access_size: u8,
    pub input: u8,
    pub data: [u8; 4],
    pub reserved0: u64,
    pub reserved1: u64,
    pub reserved2: u64,
}

#[cfg(target_os = "fuchsia")]
#[link(name = "c")]
unsafe extern "C" {
    pub fn zx_take_startup_handle(id: u32) -> zx_handle_t;
    pub fn zx_job_default() -> zx_handle_t;
    pub fn zx_vmar_root_self() -> zx_handle_t;
}

#[cfg(target_os = "fuchsia")]
#[link(name = "zircon")]
unsafe extern "C" {
    pub fn zx_handle_close(handle: zx_handle_t) -> zx_status_t;
    pub fn zx_handle_duplicate(
        handle: zx_handle_t,
        rights: zx_rights_t,
        out: *mut zx_handle_t,
    ) -> zx_status_t;

    pub fn zx_clock_get_monotonic() -> zx_time_t;
    pub fn zx_clock_get_boot() -> zx_time_t;
    pub fn zx_nanosleep(deadline: zx_time_t) -> zx_status_t;
    pub fn zx_cprng_draw(buffer: *mut u8, buffer_size: usize);

    pub fn zx_debuglog_create(
        resource: zx_handle_t,
        options: u32,
        out: *mut zx_handle_t,
    ) -> zx_status_t;
    pub fn zx_debuglog_write(
        handle: zx_handle_t,
        options: u32,
        buffer: *const u8,
        buffer_size: usize,
    ) -> zx_status_t;
    pub fn zx_debug_write(buffer: *const u8, buffer_size: usize) -> zx_status_t;

    pub fn zx_channel_create(
        options: u32,
        out0: *mut zx_handle_t,
        out1: *mut zx_handle_t,
    ) -> zx_status_t;
    pub fn zx_channel_read(
        handle: zx_handle_t,
        options: u32,
        bytes: *mut u8,
        handles: *mut zx_handle_t,
        num_bytes: u32,
        num_handles: u32,
        actual_bytes: *mut u32,
        actual_handles: *mut u32,
    ) -> zx_status_t;
    pub fn zx_channel_write(
        handle: zx_handle_t,
        options: u32,
        bytes: *const u8,
        num_bytes: u32,
        handles: *const zx_handle_t,
        num_handles: u32,
    ) -> zx_status_t;

    pub fn zx_socket_create(
        options: u32,
        out0: *mut zx_handle_t,
        out1: *mut zx_handle_t,
    ) -> zx_status_t;
    pub fn zx_socket_read(
        handle: zx_handle_t,
        options: u32,
        buffer: *mut u8,
        buffer_size: usize,
        actual: *mut usize,
    ) -> zx_status_t;

    pub fn zx_port_create(options: u32, out: *mut zx_handle_t) -> zx_status_t;
    pub fn zx_port_wait(
        handle: zx_handle_t,
        deadline: zx_time_t,
        packet: *mut zx_port_packet_t,
    ) -> zx_status_t;
    pub fn zx_object_wait_one(
        handle: zx_handle_t,
        signals: zx_signals_t,
        deadline: zx_time_t,
        observed: *mut zx_signals_t,
    ) -> zx_status_t;
    pub fn zx_object_wait_async(
        handle: zx_handle_t,
        port: zx_handle_t,
        key: u64,
        signals: zx_signals_t,
        options: u32,
    ) -> zx_status_t;
    pub fn zx_object_get_property(
        handle: zx_handle_t,
        property: u32,
        value: *mut u8,
        value_size: usize,
    ) -> zx_status_t;
    pub fn zx_object_set_property(
        handle: zx_handle_t,
        property: u32,
        value: *const u8,
        value_size: usize,
    ) -> zx_status_t;
    pub fn zx_object_get_info(
        handle: zx_handle_t,
        topic: u32,
        buffer: *mut u8,
        buffer_size: usize,
        actual: *mut usize,
        avail: *mut usize,
    ) -> zx_status_t;

    pub fn zx_resource_create(
        parent_rsrc: zx_handle_t,
        options: u32,
        base: u64,
        size: usize,
        name: *const u8,
        name_size: usize,
        resource_out: *mut zx_handle_t,
    ) -> zx_status_t;
    pub fn zx_ioports_request(resource: zx_handle_t, io_addr: u16, len: u32) -> zx_status_t;
    pub fn zx_handle_replace(
        handle: zx_handle_t,
        rights: zx_rights_t,
        out: *mut zx_handle_t,
    ) -> zx_status_t;

    pub fn zx_job_create(
        parent_job: zx_handle_t,
        options: u32,
        out: *mut zx_handle_t,
    ) -> zx_status_t;
    pub fn zx_job_set_policy(
        handle: zx_handle_t,
        options: u32,
        topic: u32,
        policy: *const u8,
        policy_size: u32,
    ) -> zx_status_t;
    pub fn zx_process_create(
        job: zx_handle_t,
        name: *const u8,
        name_size: usize,
        options: u32,
        proc_handle: *mut zx_handle_t,
        vmar_handle: *mut zx_handle_t,
    ) -> zx_status_t;
    pub fn zx_process_start(
        handle: zx_handle_t,
        thread: zx_handle_t,
        entry: zx_vaddr_t,
        stack: zx_vaddr_t,
        arg1: zx_handle_t,
        arg2: usize,
    ) -> zx_status_t;
    pub fn zx_thread_create(
        process: zx_handle_t,
        name: *const u8,
        name_size: usize,
        options: u32,
        out: *mut zx_handle_t,
    ) -> zx_status_t;
    pub fn zx_task_kill(handle: zx_handle_t) -> zx_status_t;
    pub fn zx_process_exit(retcode: i64) -> !;

    pub fn zx_vmo_create(size: u64, options: u32, out: *mut zx_handle_t) -> zx_status_t;
    pub fn zx_vmo_get_size(handle: zx_handle_t, size: *mut u64) -> zx_status_t;
    pub fn zx_vmo_read(
        handle: zx_handle_t,
        buffer: *mut u8,
        offset: u64,
        buffer_size: usize,
    ) -> zx_status_t;
    pub fn zx_vmo_write(
        handle: zx_handle_t,
        buffer: *const u8,
        offset: u64,
        buffer_size: usize,
    ) -> zx_status_t;
    pub fn zx_vmo_create_child(
        handle: zx_handle_t,
        options: u32,
        offset: u64,
        size: u64,
        out: *mut zx_handle_t,
    ) -> zx_status_t;
    pub fn zx_vmo_replace_as_executable(
        handle: zx_handle_t,
        vmex: zx_handle_t,
        out: *mut zx_handle_t,
    ) -> zx_status_t;
    pub fn zx_vmo_create_contiguous(
        bti: zx_handle_t,
        size: usize,
        alignment_log2: u32,
        out: *mut zx_handle_t,
    ) -> zx_status_t;
    pub fn zx_vmo_create_physical(
        resource: zx_handle_t,
        paddr: zx_paddr_t,
        size: usize,
        out: *mut zx_handle_t,
    ) -> zx_status_t;
    pub fn zx_vmo_set_cache_policy(handle: zx_handle_t, cache_policy: u32) -> zx_status_t;

    // Zircon IOMMU / BTI / PMT syscalls (PCI DMA for virtio-net & virtio-blk).
    pub fn zx_iommu_create(
        resource: zx_handle_t,
        r#type: u32,
        desc: *const u8,
        desc_size: usize,
        out: *mut zx_handle_t,
    ) -> zx_status_t;
    pub fn zx_bti_create(
        iommu: zx_handle_t,
        options: u32,
        bti_id: u64,
        out: *mut zx_handle_t,
    ) -> zx_status_t;
    pub fn zx_bti_pin(
        handle: zx_handle_t,
        options: u32,
        vmo: zx_handle_t,
        offset: u64,
        size: u64,
        addrs: *mut zx_paddr_t,
        num_addrs: usize,
        pmt: *mut zx_handle_t,
    ) -> zx_status_t;
    pub fn zx_pmt_unpin(handle: zx_handle_t) -> zx_status_t;

    pub fn zx_vmar_allocate(
        parent_vmar: zx_handle_t,
        options: zx_vm_option_t,
        offset: usize,
        size: usize,
        child_vmar: *mut zx_handle_t,
        child_addr: *mut zx_vaddr_t,
    ) -> zx_status_t;
    pub fn zx_vmar_map(
        handle: zx_handle_t,
        options: zx_vm_option_t,
        vmar_offset: usize,
        vmo: zx_handle_t,
        vmo_offset: u64,
        len: usize,
        mapped_addr: *mut zx_vaddr_t,
    ) -> zx_status_t;
    pub fn zx_vmar_unmap(handle: zx_handle_t, addr: zx_vaddr_t, len: usize) -> zx_status_t;
    pub fn zx_vmar_destroy(handle: zx_handle_t) -> zx_status_t;

    // Zircon Hypervisor syscalls (Tier 2).
    pub fn zx_guest_create(
        resource: zx_handle_t,
        options: u32,
        guest_handle: *mut zx_handle_t,
        vmar_handle: *mut zx_handle_t,
    ) -> zx_status_t;
    pub fn zx_guest_set_trap(
        handle: zx_handle_t,
        kind: u32,
        addr: zx_vaddr_t,
        size: usize,
        port_handle: zx_handle_t,
        key: u64,
    ) -> zx_status_t;
    pub fn zx_vcpu_create(
        guest: zx_handle_t,
        options: u32,
        entry: zx_vaddr_t,
        out: *mut zx_handle_t,
    ) -> zx_status_t;
    pub fn zx_vcpu_enter(
        handle: zx_handle_t,
        packet: *mut zx_port_packet_t,
    ) -> zx_status_t;
    pub fn zx_vcpu_kick(handle: zx_handle_t) -> zx_status_t;
    pub fn zx_vcpu_interrupt(handle: zx_handle_t, vector: u32) -> zx_status_t;
    pub fn zx_vcpu_read_state(
        handle: zx_handle_t,
        kind: u32,
        buffer: *mut u8,
        buffer_size: usize,
    ) -> zx_status_t;
    pub fn zx_vcpu_write_state(
        handle: zx_handle_t,
        kind: u32,
        buffer: *const u8,
        buffer_size: usize,
    ) -> zx_status_t;
}

#[cfg(target_os = "fuchsia")]
unsafe extern "C" {
    /// `<zircon/dlfcn.h>` in Fuchsia `libc.so`: swaps the dynamic linker loader service channel.
    pub fn dl_set_loader_service(new_svc: zx_handle_t) -> zx_handle_t;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn processargs_header_size() {
        assert_eq!(core::mem::size_of::<zx_proc_args_t>(), 36);
        assert_eq!(core::mem::size_of::<zx_port_packet_t>(), 48);
        assert_eq!(core::mem::size_of::<zx_info_process_t>(), 24);
        assert_eq!(core::mem::size_of::<zx_policy_basic_v2_t>(), 12);
        assert_eq!(core::mem::size_of::<zx_vcpu_state_t>(), 136);
        assert_eq!(core::mem::size_of::<zx_vcpu_io_t>(), 8);
        assert_eq!(core::mem::size_of::<zx_packet_guest_bell_t>(), 32);
        assert_eq!(core::mem::size_of::<zx_packet_guest_mem_t>(), 32);
        assert_eq!(core::mem::size_of::<zx_packet_guest_io_t>(), 32);
    }
}
