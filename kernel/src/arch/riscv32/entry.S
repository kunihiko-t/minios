/* NEORV32はMモードのベアメタルで起動する。OpenSBIやDTBは存在しない。 */
/* spだけを設定し、BSSをゼロ化してRustのkernel_main32を呼ぶ。 */
.section .text.entry, "ax"
.globl _start
.type _start, @function
_start:
    la sp, __boot_stack_end
    /* RV32に`sd`はないため`sw`で4バイトずつゼロ化する。 */
    la t0, __bss_start
    la t1, __bss_end
1:
    bgeu t0, t1, 2f
    sw zero, 0(t0)
    addi t0, t0, 4
    j 1b
2:
    /* 予期しないトラップは復帰先のない停止ループへ集める。 */
    la t0, park
    csrw mtvec, t0
    /* kernel_main32はRust側のABI境界である。 */
    call kernel_main32
3:
    wfi
    j 3b
park:
    j park
.size _start, . - _start
