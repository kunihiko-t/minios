# 5. UARTで文字を出す・受け取る

## 学習目標

QEMU `virt`の16550互換UARTについて、MMIO、volatile access、Line Status Registerのbit、
format出力とbyte入出力の責務分離を説明できるようになります。

## 背景

kernel起動直後にはfileやterminal driverがありません。UART MMIOはCPUからaddressとして見えても
通常RAMではなく、read/write自体がdeviceへの作用です。compilerに値の再利用やaccessの削除を
許すとhardwareへ届かないため、volatile境界が必要です。

## 実装

[`Uart`](../../kernel/src/drivers/uart.rs)はQEMU `virt`固有のbase `0x1000_0000`を持ちます。
base + 5のLine Status Registerでbit 5が送信可能、bit 0が受信可能です。`write_byte`はbit 5を
待ってoffset 0へ`write_volatile`し、`has_byte`と`read_byte`はbit 0を見てoffset 0から
`read_volatile`します。

[`console`](../../kernel/src/console.rs)はdevice registerを隠し、`core::fmt::Write`経由の
`print!`/`println!`、byte入力、緊急出力を提供します。formatとdevice accessを分けることで、
shellはUART register配置を知りません。

## 実行と確認

```console
$ cargo xtask test boot
...
MiniOS booting...
hart id: 0
[MINIOS_TEST] boot: ok
```

5秒以内のstatus 0だけでなくmarkerが必要です。timeout時はxtaskがQEMUをkillしてwaitし、捕捉した
stdout/stderrと実際のcommand lineを省略せず返します。

## よくある失敗

- 文字化け・欠落: transmit-ready bit 5を待たず書いていないか確認します。
- 入力が永遠に待つ: receive-ready bit 0とtransmit bitを取り違えていないか確認します。
- release buildだけ出力が消える: MMIOに通常のpointer read/writeを使っていないか監査します。
- timeoutとcrashを混同する: xtaskのstatus、timeout文、最後のUART行を別々に読みます。

## 演習

Line Status Registerの値が`0b0010_0001`のとき、送信と受信のどちらが可能か答えてください。
次に`Uart`、`console`、`shell`の各層が知るべき情報を一行ずつ書き、register offsetがshellへ
漏れていないことを確認します。

## 次の章

[第4章](04-boot-with-opensbi.md)へ戻れます。次は
[第6章: panicと診断](06-panic-and-diagnostics.md)で、通常経路が壊れた後にも使える出力を作ります。
