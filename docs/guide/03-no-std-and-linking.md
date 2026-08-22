# `no_std` とリンク配置

MiniOS のカーネルバイナリは OS の上で動かないため、標準ライブラリを使わない
`#![no_std]` と `#![no_main]` を指定します。通常の `main`、ファイル入出力、動的
アロケータはこの段階ではありません。panic は workspace の dev/release profile で
`abort` にし、unwind 用のランタイムを要求しない構成です。

`kernel/linker.ld` は `ENTRY(_start)` とし、QEMU `virt` における OpenSBI の次の
ロード先 `0x8020_0000` から配置します。`.text`、`.rodata`、`.data`、`.bss` を
4 KiB 境界にそろえ、`__bss_start`、`__bss_end`、`__kernel_end` を公開します。
`.bss` の末尾には初期起動用の 64 KiB スタックを置き、`__boot_stack_end` を
アセンブリの初期 `sp` とします。

ビルドスクリプトは絶対パスの `linker.ld` を `minios-kernel` **バイナリだけ**へ
渡します。この分離により、ホスト側で実行する `minios-kernel` ライブラリテストへ
RISC-V 用リンカ契約を混ぜません。

次のコマンドで ELF を生成できます。

```text
cargo xtask build
# target/riscv64gc-unknown-none-elf/debug/minios-kernel
```
