# UART コンソール

QEMU `virt` の 16550 互換 UART は MMIO ベース `0x1000_0000` にあります。通常の
メモリではないため、MiniOS は `read_volatile` と `write_volatile` を使います。
volatile によりコンパイラはデバイスレジスタへのアクセスを削除・統合・キャッシュ
できません。

Line Status Register はベースから offset 5 です。bit 5 は送信保持レジスタが空で
送信可能であること、bit 0 は受信バイトが存在することを表します。`write_byte` は
bit 5 を待って offset 0 へ書き込み、`has_byte` は bit 0 を確認し、`read_byte` は
bit 0 を待って offset 0 から読みます。

`Uart` は `core::fmt::Write` を実装しているので、`console::_print` と
`print!`/`println!` は割り当てなしで整形済み文字列を UART へ送れます。

```text
cargo xtask test boot
```

このテストは feature `qemu-test-boot` を付けてカーネルをビルドし、UART と OpenSBI
の出力を捕捉します。5 秒以内に QEMU が終了し、終了 status が 0 で、かつ正確な
`[MINIOS_TEST] boot: ok` が含まれる場合だけ成功です。非ゼロ終了はクラッシュまたは
QEMU 起動失敗として出力を添えて失敗し、marker がない正常終了も失敗です。期限切れ
では QEMU を kill して wait した後、捕捉済み stdout と stderr を返すため、無限待機と
起動直後のクラッシュを区別できます。
