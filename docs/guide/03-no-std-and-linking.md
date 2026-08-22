# 3. `no_std`とlink配置

## 学習目標

`#![no_std]`、`#![no_main]`、panic abortが必要な理由と、linker scriptがELF section、boot stack、
kernel終端をどの物理addressへ置くかを説明できるようになります。

## 背景

通常のRust binaryはhost OSのruntimeとstandard libraryを前提にします。bare-metal kernelには
process entryの`main`もunwind runtimeもありません。またcompilerが生成したsectionをどこへ
loadするかはOS loaderではなくkernel自身のlink契約で決める必要があります。

## 実装

[`kernel/src/main.rs`](../../kernel/src/main.rs)は`#![no_std]`と`#![no_main]`を宣言し、workspace
profileはpanicを`abort`にします。[`kernel/linker.ld`](../../kernel/linker.ld)は`ENTRY(_start)`とし、
QEMU `virt`でOpenSBIの領域を避けた`0x8020_0000`から`.text`、`.rodata`、`.data`、`.bss`を
4 KiB境界に配置します。

`.bss`にはsmall BSSも回収し、`__bss_start`と`__bss_end`を公開します。64 KiB boot stackを
16 byte整列で含め、最後を`__kernel_end`として4 KiBに切り上げます。このsymbolより下はkernel
image自身なのでpage allocatorへ渡せません。build scriptはlinker scriptの絶対pathをkernel
binaryだけへ渡し、host library testにはRISC-V link契約を混ぜません。

## 実行と確認

```console
$ cargo xtask build
$ test -f target/riscv64gc-unknown-none-elf/debug/minios-kernel
```

両commandがstatus 0ならELFが生成されています。`cargo test -p xtask
cargo::tests::linker_places_small_data_and_bss_probes_inside_boundaries`はsmall data/BSSの退行も検査します。

## よくある失敗

- `can't find crate for core`: RISC-V targetが未導入です。第2章のsetupへ戻ります。
- section overlap: start addressやalignmentを変えた可能性があります。linker mapと
  [memory map](../reference/memory-map.md)を照合します。
- host testまでRISC-V linkerで失敗する: `build.rs`のlink argumentがbinary限定か確認します。

## 演習

`kernel/linker.ld`で`.text`から`__kernel_end`までの順を紙に書き、どこがread-only、initialized、
zero-initializedか分類してください。次にboot stackを`.bss`に含めるとentryがzero化する範囲へ
入ることを確認します。

## 次の章

[第2章](02-setup.md)へ戻れます。次は[第4章: OpenSBIからの起動](04-boot-with-opensbi.md)で、
ELF entryからRustへ制御を渡します。
