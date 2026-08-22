# 3. `no_std`とリンク配置

## 学習目標

`#![no_std]`、`#![no_main]`、パニック時の即時終了が必要な理由を学びます。
リンカースクリプトがELFセクション、起動用スタック、カーネル終端をどの物理アドレスへ置くかも説明できるようになります。

## 背景

通常のRustバイナリーは、ホストOSのランタイムと標準ライブラリーを前提にします。
ベアメタルカーネルには、プロセスの入口となる`main`も、スタックを巻き戻すランタイムもありません。
コンパイラーが生成したセクションの読み込み先も、OSのローダーではなくカーネル自身のリンク規約で決めます。

## 実装

[`kernel/src/main.rs`](../../kernel/src/main.rs)は`#![no_std]`と`#![no_main]`を宣言し、ワークスペースのプロファイルはパニック時の動作を`abort`に設定しています。
[`kernel/linker.ld`](../../kernel/linker.ld)は`ENTRY(_start)`を指定し、QEMU `virt`上のOpenSBI領域を避けた`0x8020_0000`から`.text`、`.rodata`、`.data`、`.bss`を4 KiB境界に配置します。

`.bss`には小さなBSSセクションもまとめ、`__bss_start`と`__bss_end`を公開します。
64 KiBの起動用スタックは16バイト境界にそろえ、最後の位置を`__kernel_end`として4 KiB境界に切り上げます。
このシンボルより下はカーネルイメージ自身が使うため、ページアロケーターへ渡せません。
ビルドスクリプトはリンカースクリプトの絶対パスをカーネルバイナリーだけに渡し、ホスト用ライブラリーテストにはRISC-V固有のリンク規約を混ぜません。

## 実行と確認

```console
$ cargo xtask build
$ test -f target/riscv64gc-unknown-none-elf/debug/minios-kernel
```

二つのコマンドが終了ステータス0なら、ELFが生成されています。
`cargo test -p xtask cargo::tests::linker_places_small_data_and_bss_probes_inside_boundaries`は、小さなデータとBSSが境界内に配置されることも検査します。

## よくある失敗

- `can't find crate for core`：RISC-Vターゲットが導入されていません。
  第2章の`setup`へ戻ります。
- セクションの重複：開始アドレスかアラインメントを変えた可能性があります。
  リンカーマップと[メモリーマップ](../reference/memory-map.md)を照合します。
- ホストテストまでRISC-Vリンカーで失敗する：`build.rs`のリンク引数がバイナリーだけに適用されているか確認します。

## 演習

`kernel/linker.ld`を読み、`.text`から`__kernel_end`までの順序を紙に書いてください。
それぞれを、読み取り専用、初期値あり、ゼロ初期化の三種類に分類します。
起動用スタックを`.bss`に含めると、`entry.S`がゼロ化する範囲に入ることも確認してください。

## 次の章

[第2章](02-setup.md)へ戻れます。
次は[第4章「OpenSBIからの起動」](04-boot-with-opensbi.md)で、ELFの入口からRustへ制御を渡します。
