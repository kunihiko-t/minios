# 4. OpenSBIからの起動

## 学習目標

OpenSBIとMiniOSの入口で使うABI、BSSのゼロ初期化、起動用スタック、SBIリセットの役割を、起動順に説明できるようになります。

## 背景

QEMUの`-bios default`は、最初にM-modeのOpenSBIを起動します。
MiniOSはハードウェアをM-modeから直接初期化せず、ファームウェアが整えた環境からS-modeへ入ります。
この境界ではC ABIに似たレジスター規約を守らなければ、Rustの引数と静的変数の初期値を信用できません。

## 実装

OpenSBIはカーネルを`0x8020_0000`へ配置し、`a0`にハートID、`a1`にDevice Tree Blobのアドレスを入れて[`entry.S`](../../kernel/src/arch/riscv64/entry.S)の`_start`へ渡します。
`entry.S`は割り込みを止め、`__boot_stack_end`を`sp`へ設定し、`__bss_start..__bss_end`を8バイトずつゼロ化します。
その間も`a0`と`a1`を保ち、`kernel_main(hart_id, dtb)`を呼び出します。

SBI呼び出しでは、`a0..a2`が引数、`a6`が関数ID、`a7`が拡張ID、戻り値の`a0/a1`がエラーと値です。
終了にはSRST拡張`0x53525354`の関数0を使います。
リセット処理が予期せず戻った場合は、緊急用UARTへ数値エラーを出し、割り込みを無効にした`wfi`ループへ入ります。

## 実行と確認

```console
$ cargo xtask test boot
MiniOS booting...
hart id: 0
[MINIOS_TEST] boot: ok
```

実際の出力では、先頭にOpenSBIのプラットフォーム情報があり、`[ok] traps`、`[ok] timer`、`[ok] memory`も表示されます。
`xtask`は終了ステータス0と完全一致するマーカーの両方を要求します。

## よくある失敗

- UARTに何も出ない：`sp`、リンクアドレス、BSSループの分岐条件を最初に確認します。
- ハートIDが不定になる：BSSのゼロ化中に`a0`を一時レジスターとして使っていないか調べます。
- QEMUが終了しない：SRSTの拡張ID、関数ID、レジスター配置と`-bios default`を照合します。

## 演習

`entry.S`の各命令について、Rustを呼び出す前に成立させる不変条件を列挙してください。
特に、`sp`の16バイトアラインメントとBSS全域のゼロ化が崩れた場合に、どのRust機能が先に壊れるか予測します。

## 次の章

[第3章](03-no-std-and-linking.md)へ戻れます。
次は[第5章「UART」](05-uart.md)で、起動後の状態を観測する経路を作ります。
