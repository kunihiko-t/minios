/* entry.S の _start を ELF entry point とする Rust/リンカ間の起動契約。 */
OUTPUT_ARCH(riscv)
ENTRY(_start)

SECTIONS
{
    /* QEMU virt の OpenSBI が S モードカーネルをロードする物理アドレス。 */
    . = 0x80200000;

    .text : ALIGN(4K)
    {
        *(.text.entry)
        *(.text .text.*)
    }

    .rodata : ALIGN(4K)
    {
        *(.rodata .rodata.*)
    }

    .data : ALIGN(4K)
    {
        *(.data .data.*)
    }

    .bss : ALIGN(4K)
    {
        /* entry.S が Rust に入る前にこの範囲をゼロ化する契約。 */
        __bss_start = .;
        *(.bss .bss.*)
        *(COMMON)

        . = ALIGN(16);
        __boot_stack_start = .;
        /* 初期 Rust コード用の 64 KiB スタックを BSS 内へ確保する。 */
        . += 64K;
        __boot_stack_end = .;
        __bss_end = .;
    }

    . = ALIGN(4K);
    /* 以後の物理メモリ管理がカーネル占有末尾として利用する契約。 */
    __kernel_end = .;
}
