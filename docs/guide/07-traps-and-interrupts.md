# 7. RISC-Vの例外・割り込み

## 学習目標

同期exceptionと非同期interrupt、`scause/sepc/stval/stvec`、Direct mode、trap frame、`sret`の
関係を説明できるようになります。

## 背景

trapは現在のcontrol flowから特権handlerへ移るeventの総称です。illegal instructionやbreakpointは
実行命令に同期したexception、timerやdevice通知は非同期interruptです。hardwareは普通の関数callの
C ABIに従って全registerを保存してくれないため、entry assemblyが中断位置を守る必要があります。

## 実装

[`decode_scause`](../../kernel/src/arch/riscv64/trap.rs)は最上位bitをinterrupt flag、残りをcause codeへ
分ける純粋関数です。unexpected trapは種類と`scause`、`sepc`、`stval`を16進数で出しfailure reset
します。`sepc`は再開候補、`stval`はfault addressやinstruction bitsなどcause固有の追加値です。

`stvec`はDirect mode 0で`__trap_entry`を指します。entryは256 byte、16 byte alignedのframeへx1と
x3..x31を保存します。x0は常に0、x2 (`sp`)はframe幅を加えて回復できるため別slotに保存しません。
Rust handlerから戻る経路は同じregisterを復元し`sret`します。callee-savedだけでは任意のinstruction
間に入るinterruptからtemporaryやargumentを守れません。

## 実行と確認

```console
$ cargo xtask test trap
...
[MINIOS_TEST] trap: ok
```

test featureは`trap::init`後に`ebreak`を実行します。breakpoint exceptionのcause 3だけがmarkerへ
進み、status 0でresetします。marker不足やunexpected diagnosticsは失敗です。

## よくある失敗

- immediate retrap: `stvec` alignment、saved register offset、frame size、`sret`前の`sp`を調べます。
- `sepc`が同じまま繰り返す: 同期exceptionを通常復帰させるならinstruction幅だけ進める設計が必要です。
  MiniOSはtestのbreakpointを成功resetするため通常継続しません。
- Rust handlerだけ直してもregister破損する: assemblyのsave/restore対称性を照合します。
- Vectoredと誤解する: MiniOSは全trapが一つのBASEへ入るDirect modeです。

## 演習

`ebreak`を一時的にillegal instructionへ替えると、`Exception(3)`から`Exception(2)`へ変わると予測
できます。`scause`のinterrupt bitは0、`sepc`はそのinstruction、`stval`は実装依存でinstruction bits
または0です。予測を書いてから実験し、必ず変更を戻してください。

## 次の章

[第6章](06-panic-and-diagnostics.md)へ戻れます。次は
[第8章: timer割り込み](08-timer-interrupts.md)で、通常復帰するinterrupt code 5を扱います。
