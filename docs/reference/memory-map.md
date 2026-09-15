# QEMU `virt`のメモリーマップ

MiniOSはQEMU `virt`を`-m 128M`で起動し、OpenSBIが`a1`へ渡すDTBから物理RAMを`0x8000_0000..0x8800_0000`として発見します。
範囲の上端は含みません。
以下の表は`-m 128M`のときDTBが導く参照配置であり、実際の値は実行時のmachine記述 (`MachineSpec`) が決めます。

## 物理アドレス

| 範囲またはアドレス | 所有者と用途 | MiniOSでの扱い |
| --- | --- | --- |
| `0x8000_0000..0x8020_0000` | OpenSBI予約RAMとカーネル読み込み位置より下の領域 | アロケーターの対象外とし、配置上の2 MiBを保守的に予約 |
| `0x8020_0000` | カーネルの先頭とELF entryの配置位置 | `linker.ld`のlocation counter、OpenSBIの`Next Address`、QEMUの`-kernel`が一致する位置 |
| `0x8020_0000..__kernel_end` | `.text`、`.rodata`、`.data`、`.bss`、64 KiBの起動用stack、bitmap | カーネル自身が占有し、sectionごとの最小権限でS-modeへ恒等写像 |
| `align_up(__kernel_end, 0x1000)..0x8770_0000` | 割り当て可能な物理RAM | 一つの`FrameAllocator`だけが排他的に取得し、S-modeの`R+W`で恒等写像。上端からヒープ成長分を供給する |
| `0x8770_0000..0x8780_0000` | 汎用ヒープの初期領域 (1 MiB) | 16バイト粒度のfree-listが16バイト未満のkernel objectを分割・併合する。`#[global_allocator]`経由で`alloc` crateへ供給。OOM時は`allocate_at`で直上のframeを取り込み下端を下へ伸ばす |
| `0x8780_0000..0x87e0_0000` | MiniBundle boot payloadの予約領域 | allocatorの対象外であり、payloadがあるときは使用pageだけをS-mode read-onlyでmap |
| `0x87e0_0000..0x8800_0000` | FDTの予約領域 | QEMUがRAM上端の2 MiB整列境界へ置くDTBを守るため、allocatorの対象外で未写像 |
| `0x8800_0000` | 128 MiB RAMの排他的な上端 | RAMの上端であり、allocatorの上端ではない |
| `0x1000_0000..0x1000_1000` | 16550互換UARTのMMIO | RAM外の機器領域としてS-modeの`R+W`で恒等写像し、volatile accessだけを使用 |

`PHYSICAL_MEMORY_END`という実装定数は、物理RAM全体の上端ではなく、payload窓の開始位置`0x8780_0000`を表します。
実行時のmanaged RAMの上端は`MachineSpec::managed_end`が導き、`ram_end - FDT予約 - BUNDLE_MAX_LEN`で計算します。
ヒープの初期領域はこの上端から`KERNEL_HEAP_LEN`分を下へ切り出した範囲であり、`FrameAllocator`の管理上端はヒープ初期位置です。
ヒープが成長するとallocatorの上端からpageを取り込むため、ヒープの下端は実行時に下へ伸びます。
この上端をpayload開始位置と一致させることで、ELF loaderが確保するpage table、user page、stack pageと後続のMiniBundleが同じ物理ページを所有しません。

payloadを検証した後は、使用lengthを4 KiBへ切り上げたpageだけをS-mode read-onlyかつ`U=0`でidentity mapします。
payloadがない通常bootでは、この予約windowは未写像です。

## ユーザー仮想アドレス

ELF loaderは、次の下位Sv39範囲だけをuser imageに使います。

| 仮想範囲 | 用途 | 写像 |
| --- | --- | --- |
| `0x0010_0000..0x4000_0000` | user address range全体 | 上端を含まない固定契約 |
| `0x0010_0000..0x3ffe_f000` | `PT_LOAD`を配置できる範囲 | segmentを4 KiB pageへ丸め、ELF flags由来の最小権限と`U=1`で写像 |
| `0x3ffe_f000..0x3fff_0000` | 4 KiBのguard page | 意図して未写像 |
| `0x3fff_0000..0x4000_0000` | 64 KiBのuser stack | ゼロ化した16 pageを`R+W+U`で写像 |

`PT_LOAD`の正確なbyte範囲がguardより下にあっても、4 KiBへ丸めたpage範囲がguardまたはstackへ重なる場合は拒否します。
entry pointは実行可能な`PT_LOAD`のmemory range内に必要です。
loaderが返す`LoadedImage`はこの仮想配置を所有し、実行時にはkernel mappingとkernel trap stackを加えたaddress spaceで`satp`へ設定します。

## OpenSBIとカーネルの境界

QEMUのOpenSBI logでは、firmwareのbase addressは`0x8000_0000`、MiniOSのnext addressは`0x8020_0000`です。
firmwareの実際のimage sizeだけを推測して未使用と見なさず、この2 MiB全域をallocatorから外します。

カーネルの大きさはbuildごとに変わるため、未使用領域の開始位置を固定addressにはしません。
linkerは全sectionと起動用stackの後ろへ`__kernel_end`を置き、Rust側はそのaddressを4 KiB境界へ上向きにそろえます。
bitmapも`.bss`内にあるため、この境界より下にあります。

OpenSBIが`a1`で渡すDTBは、QEMU `virt`ではRAM上端の2 MiB整列境界 (`-m 128M`では`0x87e0_0000`) に置かれます。
`kernel_main`はbare modeでheaderの`totalsize`だけを先に読み、`fdt::MachineSpec`が構造と必須node (`/memory`の`reg`、16550 UART、`/cpus`の`timebase-frequency`) を検証します。
DTBがRAM最後の2 MiB予約領域の外側にあるmachine、あるいはpayload窓とFDT予約を収められないRAM構成は起動時に拒否します。
`cargo xtask test fdt`は、発見したRAM範囲、UARTベース、timebaseをhost側のmarkerと照合します。

[全体構成](architecture.md) | [U-modeの学習章](../guide/15-user-mode.md) | [payloadの学習章](../guide/16-boot-payload.md) | [問題の切り分け方](troubleshooting.md)
