# トラップと割り込み

RISC-V では、現在の制御フローから特権ハンドラへ移るイベントをまとめて
トラップと呼びます。同期例外は実行中の命令が原因で、不正命令、ブレークポイント、
ページフォールトなどがあります。一方、非同期割り込みは実行命令とは独立したタイマ、
外部デバイス、ソフトウェア通知から届きます。

`scause` の最上位 bit は割り込みかどうかを表し、残りの bit が cause code です。
MiniOS の `decode_scause` はこの分離だけを行う純粋関数なので、RISC-V 実機を
使わずホストテストで例外と割り込みの両分岐を検証できます。

## 診断に使う CSR

- `scause`: トラップの種類と cause code。
- `sepc`: トラップした命令または再開位置の仮想アドレス。
- `stval`: 例外固有の追加値。ページフォールトでは問題の仮想アドレス、不正命令では
  命令 bit が入る場合があります。情報がない場合は 0 です。

予期しないトラップでは、デコードした `Interrupt(code)` または `Exception(code)` と
3 つの CSR を 16 進数で UART に出し、SystemFailure でリセットします。`sepc` だけでなく
`stval` も並べると、問題の命令と対象アドレスを切り分けやすくなります。

## `stvec`: Direct と Vectored

`stvec` はハンドラの BASE アドレスと mode を持ちます。Direct mode (mode 0) では
例外も割り込みもすべて BASE へ入り、Rust 側が `scause` を読んで分配します。Vectored
mode (mode 1) では割り込みだけが `BASE + 4 * cause` へ入るため初動を短くできますが、
ベクタ表と入口ごとの ABI 管理が必要です。MiniOS は小さく検証しやすい Direct mode を
使い、`__trap_entry` を 4 byte 境界に整列して下位 mode bit を 0 にします。

## トラップフレームと ABI

トラップは通常の関数呼び出しではないため、ハードウェアは C ABI の caller-saved
レジスタを保存してくれません。割り込まれた命令から見れば、すべての整数レジスタが
実行中の状態です。そこで入口は 256 byte のフレームを確保し、x1 と x3〜x31 を
すべて保存してから Rust を呼びます。x0 は常に 0 で、x2 (`sp`) は復帰時にフレーム幅
256 を加えて回復できるため、書き換え可能な別スロットには保存しません。

256 は 16 の倍数なので、割り込まれた `sp` の 16-byte 整列を Rust の `call`
入口まで保てます。ハンドラが戻る場合は同じレジスタを復元し、フレームを解放して
`sret` します。保存対象を callee-saved だけに限ると、任意の命令間で発生する割り込みが
引数や一時値を破壊するため不十分です。

## QEMU での確認と演習

`cargo xtask test trap` は `qemu-test-trap` feature でビルドし、`trap::init` 後に
`ebreak` を実行します。breakpoint 例外の cause code は 3 です。ハンドラが正確に
デコードできたときだけ `[MINIOS_TEST] trap: ok` を出し、成功理由でリセットします。

**演習:** `kernel_main` の `ebreak` を、0 の命令 word を実行する不正命令テストに
置き換えてください。予測すべき変化は `Exception(3)` から `Exception(2)` への変化です。
`scause` の割り込み bit は 0 のままで、code は illegal instruction の 2 になります。
`sepc` は不正命令のアドレスを指し、`stval` は実装が提供する場合はその命令 bit、
そうでなければ 0 になると予測できます。marker 分岐も cause 2 に変えてから実行し、
予測と UART 診断を比較してください。
