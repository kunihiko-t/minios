/* OpenSBI は a0 に hart ID、a1 に DTB ポインタを渡す。t0/t1/sp のみを使い、
 * Rust の kernel_main を呼ぶまで a0/a1 を変更しない。 */
.section .text.entry, "ax"
.globl _start
.type _start, @function
_start:
    /* 割り込み処理系を初期化するまで S-mode 割り込みを許可しない。 */
    csrw sie, zero
    la sp, __boot_stack_end
    /* リンカが BSS を 8 バイト境界に配置するため sd により安全にゼロ化できる。 */
    la t0, __bss_start
    la t1, __bss_end
1:
    bgeu t0, t1, 2f
    sd zero, 0(t0)
    addi t0, t0, 8
    j 1b
2:
    /* kernel_main は extern "C" で公開した Rust の ABI 境界である。 */
    call kernel_main
3:
    wfi
    j 3b
.size _start, . - _start
