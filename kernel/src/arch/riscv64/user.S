/* MiniOS U-mode entry and kernel return path.
 *
 * C ABI: __run_user(a0=&UserContext, a1=user_satp, a2=kernel_satp,
 * a3=kernel_stack_top) -> a0=RunExit discriminant.
 *
 * kernel trap stack上部には144バイトの固定headerを置く。`user.S`とRust側が
 * このoffset規約を共有するため、片方だけを変更してはならない。
 *   0(sp)  ra              64(sp)  s4            112(sp) s11
 *   8(sp)  gp              72(sp)  s5            120(sp) 呼び出し側のboot stack sp
 *  16(sp)  tp              80(sp)  s6            128(sp) kernel satp
 *  24(sp)  s0              88(sp)  s7            136(sp) RunExit結果slot
 *  32(sp)  s1              96(sp)  s8
 *  40(sp)  s2             104(sp)  s9
 *  48(sp)  s3                                       header合計 = 144 bytes
 */
.section .text.user, "ax"
.balign 4
.globl __run_user
.type __run_user, @function
__run_user:
    /* boot stackの呼び出し側状態を、kernel trap stack上部の固定headerへ保存する。
     * spはまだ呼び出し側のboot stackを指すため、scratchのt0でheaderを組み立てる。 */
    addi t0, a3, -144
    sd ra,   0(t0)
    sd gp,   8(t0)
    sd tp,  16(t0)
    sd s0,  24(t0)
    sd s1,  32(t0)
    sd s2,  40(t0)
    sd s3,  48(t0)
    sd s4,  56(t0)
    sd s5,  64(t0)
    sd s6,  72(t0)
    sd s7,  80(t0)
    sd s8,  88(t0)
    sd s9,  96(t0)
    sd s10, 104(t0)
    sd s11, 112(t0)
    sd sp, 120(t0)
    sd a2, 128(t0)
    sd zero, 136(t0)
    mv sp, t0
    /* user address spaceからもこの命令列とstackを参照できるよう、
     * 交換前のtrap入口sscratchにkernel stack topを置いてからsatpを切り替える。 */
    csrw sscratch, a3
    csrw satp, a1
    sfence.vma zero, zero
    /* user PCとsstatusをCSRへ復元する。sstatusはSPP=0、SPIE=1なので、
     * sretはU-modeへ遷移しSIEへSPIEをコピーする。 */
    ld t1, 256(a0)
    csrw sepc, t1
    ld t1, 264(a0)
    csrw sstatus, t1
    /* x1..x31を復元する。t0はcontext pointerであり、最後に自分のslotから読む。 */
    mv t0, a0
    ld ra,   8(t0)
    ld sp,  16(t0)
    ld gp,  24(t0)
    ld tp,  32(t0)
    ld t1,  48(t0)
    ld t2,  56(t0)
    ld s0,  64(t0)
    ld s1,  72(t0)
    ld a0,  80(t0)
    ld a1,  88(t0)
    ld a2,  96(t0)
    ld a3, 104(t0)
    ld a4, 112(t0)
    ld a5, 120(t0)
    ld a6, 128(t0)
    ld a7, 136(t0)
    ld s2, 144(t0)
    ld s3, 152(t0)
    ld s4, 160(t0)
    ld s5, 168(t0)
    ld s6, 176(t0)
    ld s7, 184(t0)
    ld s8, 192(t0)
    ld s9, 200(t0)
    ld s10, 208(t0)
    ld s11, 216(t0)
    ld t3, 224(t0)
    ld t4, 232(t0)
    ld t5, 240(t0)
    ld t6, 248(t0)
    ld t0, 40(t0)
    sret
.size __run_user, . - __run_user

/* user trap入口。stvecは実行前にここを指す。sscratchとの交換で
 * kernel trap stack topへ降り、user spはsscratchへ退避する。
 * handlerは固定headerの下に専用frameを確保してから呼ぶ。headerを
 * 上書きするとkernel復帰値が壊れるため、frame幅はheaderと無関係に保つ。
 * a0には固定header内のRunExit結果slot地址を渡す。 */
.globl __user_trap_entry
.type __user_trap_entry, @function
__user_trap_entry:
    csrrw sp, sscratch, sp
    addi a0, sp, -8
    addi sp, sp, -1920
    call rust_user_trap_handler
    addi sp, sp, 1776
    j __user_run_leave
.size __user_trap_entry, . - __user_trap_entry

/* kernel address spaceとboot stackを復元し、Rust callerへRunExitを返す。
 * spは固定headerのbase、つまりkernel_stack_top - 144でなければならない。 */
.globl __user_run_leave
.type __user_run_leave, @function
__user_run_leave:
    ld t0, 128(sp)
    csrw satp, t0
    sfence.vma zero, zero
    ld a0, 136(sp)
    ld ra,   0(sp)
    ld gp,   8(sp)
    ld tp,  16(sp)
    ld s0,  24(sp)
    ld s1,  32(sp)
    ld s2,  40(sp)
    ld s3,  48(sp)
    ld s4,  56(sp)
    ld s5,  64(sp)
    ld s6,  72(sp)
    ld s7,  80(sp)
    ld s8,  88(sp)
    ld s9,  96(sp)
    ld s10, 104(sp)
    ld s11, 112(sp)
    ld sp, 120(sp)
    ret
.size __user_run_leave, . - __user_run_leave
