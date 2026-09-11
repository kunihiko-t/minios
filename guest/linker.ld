/* MiniOS guestの配置規約:
 *   .text/.rodata は USER_START (0x0010_0000) からR+Xで一つのsegmentへ、
 *   .data/.bss    は次pageからR+Wのsegmentへ置く。
 * kernel ELF loaderが受理する範囲は USER_START..USER_GUARD_BOTTOM である。
 */
ENTRY(_start)

PHDRS
{
    text PT_LOAD FLAGS(5); /* R+X */
    data PT_LOAD FLAGS(6); /* R+W */
}

SECTIONS
{
    . = 0x00100000;
    .text : {
        KEEP(*(.text.entry))
        *(.text .text.*)
    } :text
    .rodata : ALIGN(8) {
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
    } :text
    . = ALIGN(0x1000);
    .data : ALIGN(8) {
        *(.data .data.*)
        *(.sdata .sdata.*)
    } :data
    .bss (NOLOAD) : ALIGN(8) {
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        *(COMMON)
        . = ALIGN(8);
    } :data
    /DISCARD/ : {
        *(.eh_frame .eh_frame_hdr)
        *(.note .note.*)
        *(.comment)
    }
}
