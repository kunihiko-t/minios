# 4. OpenSBIからの起動

## 学習目標

OpenSBIとMiniOSのentry ABI、BSS zero化、boot stack、SBI resetの役割を、起動順に説明できる
ようになります。

## 背景

QEMU `-bios default`はまずM-modeのOpenSBIを起動します。MiniOSはhardwareを直接M-modeから
初期化せず、firmwareが整えた環境からS-modeへ入ります。この境界ではC ABIに似たregister契約を
守らないと、Rustの引数もstatic変数の初期値も信用できません。

## 実装

OpenSBIはkernelを`0x8020_0000`へ配置し、`a0`にhart ID、`a1`にDevice Tree Blob addressを入れて
[`entry.S`](../../kernel/src/arch/riscv64/entry.S)の`_start`へ渡します。entryは割り込みを止め、
`__boot_stack_end`を`sp`へ設定し、`__bss_start..__bss_end`を8 byteずつzero化します。`a0/a1`を
壊さず`kernel_main(hart_id, dtb)`へcallします。

SBI callは`a0..a2`が引数、`a6`がfunction ID、`a7`がextension ID、戻り`a0/a1`がerror/valueです。
終了にはSRST extension `0x53525354`、function 0を使います。resetがunexpectedに戻った場合は
emergency UARTへ数値errorを出し、割り込みを無効にした`wfi` loopへ入ります。

## 実行と確認

```console
$ cargo xtask test boot
MiniOS booting...
hart id: 0
[MINIOS_TEST] boot: ok
```

実際の先頭にはOpenSBIのplatform情報と`[ok] traps/timer/memory`も表示されます。xtaskはstatus 0と
完全一致markerの両方を要求します。

## よくある失敗

- UARTが何も出ない: `sp`、link address、BSS loopのbranch条件を最初に確認します。
- hart IDが不定: BSS clear中に`a0`をtemporaryとして使っていないか調べます。
- QEMUが終了しない: SRSTのextension/function/register配置と`-bios default`を照合します。

## 演習

`entry.S`の各instructionについて、Rustをcallする前に成立させる不変条件を列挙してください。
特に`sp`の16 byte alignmentとBSS全域zero化が崩れた場合、どのRust機能が先に壊れるか予測します。

## 次の章

[第3章](03-no-std-and-linking.md)へ戻れます。次は[第5章: UART](05-uart.md)で、起動後の観測経路を
作ります。
