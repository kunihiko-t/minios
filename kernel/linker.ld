/* entry.S の _start を ELF entry point とする Rust/リンカ間の起動契約。 */
OUTPUT_ARCH(riscv)
ENTRY(_start)

SECTIONS
{
    /* QEMU virt の OpenSBI が S モードカーネルをロードする物理アドレス。 */
    . = 0x80200000;

    . = ALIGN(4K);
    __text_start = .;
    .text :
    {
        *(.text.entry)
        *(.text .text.*)
    }
    . = ALIGN(4K);
    __text_end = .;

    . = ALIGN(4K);
    __rodata_start = .;
    .rodata :
    {
        *(.rodata .rodata.*)
    }
    . = ALIGN(4K);
    __rodata_end = .;

    . = ALIGN(4K);
    __data_start = .;
    .data :
    {
        *(.data .data.*)
        /* RISC-V の small initialized data も data 境界内へ回収する。 */
        *(.sdata .sdata.*)
    }
    . = ALIGN(4K);
    __data_end = .;

    . = ALIGN(4K);
    __bss_start = .;
    .bss :
    {
        *(.bss .bss.*)
        /* RISC-V の small zero-initialized data も entry.S のゼロ化範囲へ回収する。 */
        *(.sbss .sbss.*)
        *(COMMON)
    }
    . = ALIGN(4K);
    __bss_end = .;

    . = ALIGN(4K);
    __boot_stack_start = .;
    .boot_stack (NOLOAD) :
    {
        /* 初期 Rust コード用の 64 KiB スタックを確保する。 */
        . += 64K;
    }
    . = ALIGN(4K);
    __boot_stack_end = .;

    . = ALIGN(4K);
    /* 以後の物理メモリ管理がカーネル占有末尾として利用する契約。 */
    __kernel_end = .;
}
