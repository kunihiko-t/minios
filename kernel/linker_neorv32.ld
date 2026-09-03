/* NEORV32の内蔵IMEM/DMEMへ直接配置する、RV32実機用の起動契約。 */
/* OpenOCDのload_imageで書き込み、`pc=0`から実行する。 */
OUTPUT_ARCH(riscv)
ENTRY(_start)

MEMORY
{
  /* NEORV32の`MEM_INT_IMEM_SIZE`と一致させる。 */
  IMEM (rx)  : ORIGIN = 0x00000000, LENGTH = 24288
  /* NEORV32の`MEM_INT_DMEM_SIZE`と一致させる。 */
  DMEM (rwx) : ORIGIN = 0x80000000, LENGTH = 16192
}

SECTIONS
{
  .text : ALIGN(4)
  {
    KEEP(*(.text.entry))
    *(.text .text.*)
    *(.rodata .rodata.*)
    . = ALIGN(4);
  } > IMEM

  .data : ALIGN(4)
  {
    *(.data .data.*)
    *(.sdata .sdata.*)
    . = ALIGN(4);
  } > DMEM AT > IMEM

  .bss (NOLOAD) : ALIGN(4)
  {
    __bss_start = .;
    *(.bss .bss.*)
    *(.sbss .sbss.*)
    *(COMMON)
    . = ALIGN(4);
    __bss_end = .;
  } > DMEM

  /* DMEM上端の4 KiBを初期Rustコード用のブートスタックにする。 */
  __boot_stack_top = ORIGIN(DMEM) + LENGTH(DMEM);
  __boot_stack_end = __boot_stack_top;
  /* `main.rs`がカーネル占有末尾として利用できる境界。 */
  __kernel_end = .;

  /DISCARD/ :
  {
    *(.eh_frame*)
    *(.comment*)
  }
}
