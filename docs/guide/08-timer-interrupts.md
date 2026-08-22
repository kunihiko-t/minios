# supervisor timer 割り込み

MiniOS は一定間隔の supervisor timer 割り込みを、将来のスケジューラやタイムアウトの
基準となる tick に変換します。QEMU `virt` の timebase は 10 MHz なので、`time` CSR は
1 秒に 10,000,000 増えます。100 Hz、つまり 1 tick を 10 ms とすると、次の割り込みまでの
間隔は `10,000,000 / 100 = 100,000` cycle です。

## `time` CSR と OpenSBI TIME

S-mode は読み取り専用の `time` CSR から現在の 64-bit 時刻を取得できます。一方、次の
timer 割り込み時刻を設定する機械モードのレジスタへ直接は書きません。MiniOS は SBI
TIME 拡張 (`0x54494D45`) の `set_timer` function 0 を呼び、OpenSBI に絶対時刻を渡します。

初期化時と各割り込みの処理時に `time` を読み直し、`now + 100,000` を次の絶対 deadline
として設定します。相対時間だけを SBI に渡すのではありません。割り込みを処理しただけでは
次回のイベントは予約されないため、handler は tick を増やすたびに `set_timer` で rearm
します。この方式では handler の実行が遅れた場合も、すでに過去となった deadline を連続して
消化せず、処理時点から次の 10 ms を予約します。

SBI の timer 設定が失敗すると、MiniOS には tick を継続する別経路も回復規約もありません。
初回設定と handler 内の再設定のどちらでも、数値の SBI error を緊急 UART 経路へ出し、
`SystemFailure` で停止します。失敗を無視して不規則な時刻を公開することはありません。

## STIE と SIE の順序

割り込み許可は局所から大域の順に行います。

1. `stvec` に初期化済み trap entry を登録する。
2. SBI で最初の絶対 deadline を予約する。
3. `sie.STIE` (bit 5) を設定し、supervisor timer を許可する。
4. 最後に `sstatus.SIE` (bit 1) を設定し、S-mode の割り込みを大域的に許可する。

先に `sstatus.SIE` を立てると、対応 handler や deadline の準備が終わる前に trap へ入る
可能性があります。MiniOS は既存の CSR bit を read-modify-write で保存し、この順序を崩しません。
trap dispatcher が通常復帰させるのは `Interrupt(5)` だけです。ほかの割り込みと例外は従来どおり
`scause`、`sepc`、`stval` を診断して失敗停止します。

## tick の観測とミリ秒変換

処理済み tick 数は `AtomicU64` に置き、handler が `fetch_add`、通常コードが `load` します。
現在は単一 hart かつ writer は handler 一つで、この値はほかのメモリ状態を公開する同期旗では
ありません。そのため両操作は `Relaxed` ordering で十分です。multi-hart 化や tick と別状態の
公開を結び付ける場合は、この前提と ordering を再検討する必要があります。

`ticks_to_millis` は `ticks * 1,000 / 100` で変換し、乗算は飽和させて wraparound を避けます。
したがって 1 tick は 10 ms、250 tick は 2,500 ms です。`uptime_millis` は atomic counter の
現在値に同じ変換を適用します。

## QEMU での確認

`cargo xtask test timer` は `qemu-test-timer` feature を付け、5 秒の期限で QEMU を起動します。
kernel は少なくとも 3 回の supervisor timer 割り込みが処理されるまで待ち、
`[MINIOS_TEST] timer: ok` を出して成功理由でリセットします。xtask は終了 status 0 とこの
完全一致 marker の両方を要求するため、単なる起動、誤った cause の分配、rearm 忘れは成功に
なりません。
