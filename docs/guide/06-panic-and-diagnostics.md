# 6. panicと緊急診断

## 学習目標

`no_std` kernelのpanic handlerが残す情報と、通常console lockを避けるemergency UART経路、
診断後にfailure resetする理由を説明できるようになります。

## 背景

panicはformat処理中や割り込み中にも起こります。通常consoleの内部状態やlockがすでに壊れている
可能性があるため、同じ経路を再利用するとdeadlockし、最も必要な診断が消えます。回復不能な
状態で処理を続ければ、元の原因とは別の二次障害も生じます。

## 実装

[`panic`](../../kernel/src/main.rs)はmessage、取得できる場合のsource fileとline、初期化済みなら
boot hart IDを出します。hart IDは他の初期化より前にatomicへ記録します。

[`emergency_print`](../../kernel/src/console.rs)は局所的な`Uart`を作り、共有formatterやconsole lockを
取りません。出力後はSBI System Resetへ`Shutdown`と`SystemFailure`を渡します。SBI call自体が
失敗した場合も数値errorを直接UARTへ残し、S-mode割り込みを止めた`wfi` loopへ入ります。

## 実行と確認

通常の回帰確認は次です。

```sh
cargo xtask test trap
```

panicを一時的に観察するときは`kernel_main`のtrap初期化後へ`panic!("exercise")`を置き、
`cargo xtask run`で`MiniOS panic`、file、line、hart IDが揃うことを確認します。終了statusはfailureに
なるのが正しいため、観察後は変更を戻します。

## よくある失敗

- panic後に無出力で停止: panic handlerが通常`println!`を通していないか確認します。
- file/lineが出ない: `PanicInfo::location()`は常に存在するとは限りません。`None`分岐も必要です。
- panic後も実行が続く: recovery規約がないのでSBI failure resetへ必ず到達させます。
- 診断中にallocationする: heapもallocatorの整合性も信用できません。固定bufferすら不要な経路を
  小さく保ちます。

## 演習

通常consoleがlock保持中にpanicした場合のwait graphを書いてください。次にemergency pathが
参照する状態を列挙し、UART baseとformat arguments以外の共有mutable stateがないことを確認します。

## 次の章

[第5章](05-uart.md)へ戻れます。次は[第7章: 例外・割り込み](07-traps-and-interrupts.md)で、
CPUが通常control flowを離れる入口を作ります。
