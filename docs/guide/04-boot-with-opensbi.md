# OpenSBI からの起動

QEMU `virt` は `-bios default` により OpenSBI を起動し、S モードの MiniOS を
`0x8020_0000` にロードします。OpenSBI との最初の ABI 契約は、`a0` が hart ID、
`a1` が Device Tree Blob (DTB) ポインタであることです。`entry.S` は BSS を消去
する間に `a0` と `a1` を変更せず、`kernel_main(hart_id, dtb)` へ渡します。

Rust のグローバル/静的領域はロード時に必ずしもゼロではありません。そのため Rust
コードへ入る前に `__bss_start..__bss_end` を 8 バイトずつゼロ化します。リンカは
この範囲を十分に整列し、エントリコードはその後でスタックを使って Rust を呼びます。

リセットには SBI の SRST 拡張（extension ID `0x53525354`、function ID 0）を使います。
SBI 呼び出しは `a0..a2` に引数、`a6` に function ID、`a7` に extension ID を置き、
戻りの `a0/a1` を error/value として読む ABI です。テストモードでは成功した
マーカーの後に Shutdown を要求するので、QEMU は正常終了します。

期待する起動末尾は次のとおりです。

```text
MiniOS booting...
hart id: 0
[MINIOS_TEST] boot: ok
```

通常モードはシェルを実装する次章以降まで `wfi` で待機します。SBI リセットが
エラーを返した異常経路では、UART に数値エラーを出して割り込みを無効化した `wfi`
ループへ入ります。
