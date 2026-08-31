/* MiniOS U-mode entry and kernel return path.
 *
 * C ABI: __run_user(a0=&UserContext, a1=user_satp, a2=kernel_satp,
 * a3=kernel_stack_top) -> a0=RunExit discriminant.
 *
 * kernel trap stack上位には144バイトの固定headerを置く。`user.S`とRust側が
 * このoffset規約を共有するため、片方だけを変更してはならない。
 *   0(sp)  ra              64(sp)  s4            112(sp) s11
 *   8(sp)  gp              72(sp)  s5            120(sp) 呼び出し側のboot stack sp
 *  16(sp)  tp              80(sp)  s6            128(sp) kernel satp
 *  24(sp)  s0              88(sp)  s7            136(sp) 予約（未使用）
 *  32(sp)  s1              96(sp)  s8
 *  40(sp)  s2             104(sp)  s9
 *  48(sp)  s3                                       header合計 = 144 bytes
 *
 * trap frameのUserContext（34語、272バイト）はheaderの直下
 * [kernel_stack_top-416, kernel_stack_top-144) に置く。
 * register i は 8*i、sepc=256、sstatus=264 が`UserContext`のC ABI layoutである。
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

/* user trap入口。stvecは実行前にここを指す。
 * 最初の命令でsscratchとspを交換し、spをkernel trap stack topへ降ろす。
 * 交換後のkernel stack上にUserContextを組み立て、x1とx3..x31を保存し、
 * 交換前のuser spをsscratchから読んでregisters[2]へ、sepcとsstatusも
 * contextへ保存してからhandlerへ渡す。handlerの戻り値a0がRunExitである。
 * Resume(0)ならcontextから逆順に復元してsretし、
 * ReturnToKernel(1)なら固定header経由でkernelへ戻る。 */
.balign 4
.globl __user_trap_entry
.type __user_trap_entry, @function
__user_trap_entry:
    csrrw sp, sscratch, sp
    addi sp, sp, -416
    sd ra,   8(sp)
    sd gp,  24(sp)
    sd tp,  32(sp)
    sd t0,  40(sp)
    sd t1,  48(sp)
    sd t2,  56(sp)
    sd s0,  64(sp)
    sd s1,  72(sp)
    sd a0,  80(sp)
    sd a1,  88(sp)
    sd a2,  96(sp)
    sd a3, 104(sp)
    sd a4, 112(sp)
    sd a5, 120(sp)
    sd a6, 128(sp)
    sd a7, 136(sp)
    sd s2, 144(sp)
    sd s3, 152(sp)
    sd s4, 160(sp)
    sd s5, 168(sp)
    sd s6, 176(sp)
    sd s7, 184(sp)
    sd s8, 192(sp)
    sd s9, 200(sp)
    sd s10, 208(sp)
    sd s11, 216(sp)
    sd t3, 224(sp)
    sd t4, 232(sp)
    sd t5, 240(sp)
    sd t6, 248(sp)
    /* t0..t2のuser値は保存済み。以降のcsrr退避にt0を使う。 */
    csrr t0, sscratch
    sd t0,  16(sp)
    csrr t0, sepc
    sd t0, 256(sp)
    csrr t0, sstatus
    sd t0, 264(sp)
    mv a0, sp
    /* handlerはcontextの直下に専用frameを確保して呼ぶ。
     * headerとcontextを上書きするとkernel復帰値が壊れるため、frame幅は
     * それらと無関係に保つ。1920 - 416 = 2336 byte使用し、stackは4 KiB page内に収まる。 */
    addi sp, sp, -1920
    call rust_user_trap_handler
    addi sp, sp, 1920
    bnez a0, 2f
    /* Resume: 次のuser trapに備えてsscratchへkernel stack topを戻してから、
     * sstatus・sepcをcontextの値でCSRへ書き戻し、registerを逆順に復元する。 */
    addi t1, sp, 416
    csrw sscratch, t1
    ld t1, 264(sp)
    csrw sstatus, t1
    ld t1, 256(sp)
    csrw sepc, t1
    mv t0, sp
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
2:
    /* spをcontext baseから固定header baseへ戻し、kernelへ復帰する。 */
    addi sp, sp, 272
    j __user_run_leave
.size __user_trap_entry, . - __user_trap_entry

/* kernel address spaceとboot stackを復元し、Rust callerへRunExitを返す。
 * spは固定headerのbase、つまりkernel_stack_top - 144でなければならない。
 * a0にはhandlerが返したRunExitが入ったままなので、ここでは決して書き換えない。 */
.globl __user_run_leave
.type __user_run_leave, @function
__user_run_leave:
    ld t0, 128(sp)
    csrw satp, t0
    sfence.vma zero, zero
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
