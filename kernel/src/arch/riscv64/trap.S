/* RISC-V C ABI が求める 16-byte 整列を保つ固定 256-byte フレームである。
 * x0 は不変、x2(sp) は復帰時に 256 を足して回復できるため、x1 と x3..x31 を保存する。 */
.section .text.trap, "ax"
.balign 4
/* stvec に登録する生シンボルで、Direct mode の BASE 要件に必要な 4-byte 整列を満たす。 */
.globl __trap_entry
.type __trap_entry, @function
__trap_entry:
    addi sp, sp, -256
    sd ra,   0(sp)
    sd gp,   8(sp)
    sd tp,  16(sp)
    sd t0,  24(sp)
    sd t1,  32(sp)
    sd t2,  40(sp)
    sd s0,  48(sp)
    sd s1,  56(sp)
    sd a0,  64(sp)
    sd a1,  72(sp)
    sd a2,  80(sp)
    sd a3,  88(sp)
    sd a4,  96(sp)
    sd a5, 104(sp)
    sd a6, 112(sp)
    sd a7, 120(sp)
    sd s2, 128(sp)
    sd s3, 136(sp)
    sd s4, 144(sp)
    sd s5, 152(sp)
    sd s6, 160(sp)
    sd s7, 168(sp)
    sd s8, 176(sp)
    sd s9, 184(sp)
    sd s10, 192(sp)
    sd s11, 200(sp)
    sd t3, 208(sp)
    sd t4, 216(sp)
    sd t5, 224(sp)
    sd t6, 232(sp)

    /* 生シンボル rust_trap_handler は extern "C" で、呼び出し時の sp は 16-byte 整列済みである。 */
    call rust_trap_handler

    ld ra,   0(sp)
    ld gp,   8(sp)
    ld tp,  16(sp)
    ld t0,  24(sp)
    ld t1,  32(sp)
    ld t2,  40(sp)
    ld s0,  48(sp)
    ld s1,  56(sp)
    ld a0,  64(sp)
    ld a1,  72(sp)
    ld a2,  80(sp)
    ld a3,  88(sp)
    ld a4,  96(sp)
    ld a5, 104(sp)
    ld a6, 112(sp)
    ld a7, 120(sp)
    ld s2, 128(sp)
    ld s3, 136(sp)
    ld s4, 144(sp)
    ld s5, 152(sp)
    ld s6, 160(sp)
    ld s7, 168(sp)
    ld s8, 176(sp)
    ld s9, 184(sp)
    ld s10, 192(sp)
    ld s11, 200(sp)
    ld t3, 208(sp)
    ld t4, 216(sp)
    ld t5, 224(sp)
    ld t6, 232(sp)
    /* 割り込まれたspはフレーム幅を戻すだけで完全に復元でき、可変スロットを要しない。 */
    addi sp, sp, 256
    /* Rust ハンドラから戻る経路は sepc/sstatus を書き換えない契約のため、
     * ハードウェアがトラップ入口で保存した再開先と特権状態を sret がそのまま使える。 */
    sret
.size __trap_entry, . - __trap_entry
