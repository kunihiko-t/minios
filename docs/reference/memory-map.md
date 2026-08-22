# QEMU `virt` memory map

MiniOSはQEMU `virt`を`-m 128M`で起動し、RAMを`0x8000_0000..0x8800_0000`として扱います。
上端はexclusiveです。現在はDevice Treeを解析しないため、次のfixed layoutがruntime契約です。

| 範囲・address | 所有者/用途 | MiniOSの扱い |
| --- | --- | --- |
| `0x8000_0000..0x8020_0000` | OpenSBI reserved RAMとkernel load前の低位領域 | allocator対象外。OpenSBI imageの実占有に加え、layout上の2 MiBを保守的に予約 |
| `0x8020_0000` | kernel start / ELF entry配置域の先頭 | `linker.ld`のsection counter、OpenSBI `Next Address`、QEMU `-kernel`の一致点 |
| `0x8020_0000..__kernel_end` | `.text/.rodata/.data/.bss`、64 KiB boot stack、bitmap | kernel自身が占有。`__kernel_end`はlinkerが4 KiB alignedでexportする可変symbol |
| `align_up(__kernel_end, 0x1000)..0x8800_0000` | allocatable physical RAM | `unsafe`な`FrameAllocator::new`で、このallocatorだけが排他的にclaim。upper boundはexclusive |
| `0x8800_0000` | 128 MiB RAM upper bound | `PHYSICAL_MEMORY_END`。このaddress以上は絶対にallocateしない |
| `0x1000_0000..0x1000_1000` | 16550 compatible UART MMIO | RAM外のdevice window。base `0x1000_0000`へvolatile access |

## OpenSBI reservedとkernel start

QEMUのOpenSBI logではfirmware baseが`0x8000_0000`、MiniOSのnext addressが`0x8020_0000`です。
firmwareの実image sizeだけを推測してfreeにせず、この2 MiB全域をallocatorから外します。これにより
firmware versionで内部heapやscratch配置が変わってもkernelと衝突しません。

## `__kernel_end`とallocatable RAM

kernel sizeはbuildごとに変わるためfree startを固定addressにしません。linkerが全sectionとboot stackの後へ
`__kernel_end`を置き、Rust側はaddressだけを取得して4 KiBへ上向きalignmentします。bitmapも`.bss`内にある
のでこの境界より下です。allocator上端はQEMU `-m 128M`と同じ`0x8800_0000`です。

OpenSBIから渡されるDTBは検証環境でRAM高位に置かれますが、現在のmilestoneは内容を読みません。将来DTBを
保持して解析する段階では、そのblobの実rangeをDevice Treeから確定し、allocator予約へ追加する必要があります。

## UARTはRAMではない

`0x1000_0000`は128 MiB RAM範囲外ですがCPUのphysical address spaceにあります。normal load/store構文を
使えてもdevice side effectを持つため、`read_volatile`/`write_volatile`以外でaccessしません。

[architecture](architecture.md) | [troubleshooting](troubleshooting.md)
