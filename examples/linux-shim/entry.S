/* Ring-0 <-> Ring-3 x86-64 SYSCALL/SYSRET and IRETQ trampoline for Hirsel-OS Tier 2 Linux shim. */

.section .text
.global load_gdt_and_tss
.global enter_ring3
.global syscall_entry

.extern g_kernel_rsp
.extern g_user_rsp
.extern linux_syscall_dispatch

/* void load_gdt_and_tss(const void *gdtr, uint16_t tss_sel) */
load_gdt_and_tss:
    lgdt (%rdi)
    mov $0x10, %ax
    mov %ax, %ds
    mov %ax, %es
    mov %ax, %fs
    mov %ax, %gs
    mov %ax, %ss
    /* Far return to reload CS = 0x08 (64-bit Kernel CS). */
    pushq $0x08
    leaq 1f(%rip), %rax
    pushq %rax
    lretq
1:
    ltr %si
    ret

/* void enter_ring3(uint64_t user_rip, uint64_t user_rsp) __attribute__((noreturn)) */
enter_ring3:
    /* Push Ring-3 IRETQ frame: SS, RSP, RFLAGS, CS, RIP. */
    pushq $0x1B          /* User DS | RPL=3 */
    pushq %rsi           /* User RSP */
    pushq $0x202         /* RFLAGS (IF=1, reserved bit 1=1) */
    pushq $0x23          /* User CS | RPL=3 */
    pushq %rdi           /* User RIP */
    xor %rax, %rax
    xor %rbx, %rbx
    xor %rcx, %rcx
    xor %rdx, %rdx
    xor %rsi, %rsi
    xor %rdi, %rdi
    xor %rbp, %rbp
    xor %r8, %r8
    xor %r9, %r9
    xor %r10, %r10
    xor %r11, %r11
    xor %r12, %r12
    xor %r13, %r13
    xor %r14, %r14
    xor %r15, %r15
    iretq

/* Native x86-64 SYSCALL entry (zero VM exits via AMD SVM MSRPM passthrough!).
 * On entry: RAX=nr, RDI=a1, RSI=a2, RDX=a3, R10=a4, R8=a5, R9=a6, RCX=user RIP, R11=user RFLAGS. */
syscall_entry:
    movq %rsp, g_user_rsp(%rip)
    movq g_kernel_rsp(%rip), %rsp

    /* Build struct SyscallFrame on the kernel stack. */
    pushq %r11           /* user_rflags */
    pushq %rcx           /* user_rip */
    pushq %r15
    pushq %r14
    pushq %r13
    pushq %r12
    pushq %rbp
    pushq %rbx
    pushq %r9            /* arg6 */
    pushq %r8            /* arg5 */
    pushq %r10           /* arg4 */
    pushq %rdx           /* arg3 */
    pushq %rsi           /* arg2 */
    pushq %rdi           /* arg1 */
    pushq %rax           /* nr */

    movq %rsp, %rdi
    call linux_syscall_dispatch
    /* Return value is in RAX; store into saved frame slot. */
    movq %rax, 0(%rsp)

    popq %rax
    popq %rdi
    popq %rsi
    popq %rdx
    popq %r10
    popq %r8
    popq %r9
    popq %rbx
    popq %rbp
    popq %r12
    popq %r13
    popq %r14
    popq %r15
    popq %rcx            /* user_rip */
    popq %r11            /* user_rflags */

    movq g_user_rsp(%rip), %rsp
    sysretq
