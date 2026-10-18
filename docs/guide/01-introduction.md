# 1. MiniOSで学ぶこと

## 学習目標

MiniOSの到達点と学習範囲を把握します。
読み終えると、QEMU上で動く小さなカーネルを題材に、起動、UART、トラップ、タイマー、物理ページ管理、シェルをどの順に組み立てるのか説明できるようになります。

## 背景

通常のRustプログラムは、プロセス、仮想メモリー、標準入出力などをOSから提供してもらいます。
カーネルを作るときは、その提供元を自分で用意します。
MiniOSの主教材では、基本構成をRISC-V 64、QEMU `virt`、OpenSBI、1ハート、128 MiB RAMに固定し、一つずつ観察できる規模に保っています。
multi-hartの検証経路だけは`smp-*`のQEMUテストで2ハートを使います。
同じコンソールとシェルの境界を使い、NEORV32上でRISC-V 32のM-mode起動も試せます。
Rustの基本構文、Gitの基本操作、CPUが命令を順に実行することを知っていれば読み始められます。

## 実装

最初の区切りまでに作る機能は次のとおりです。

- OpenSBIからSupervisorモードへ入る起動コードと`no_std`のRustコード
- 16550互換UARTによる出力、入力、パニック診断
- 例外と割り込みの入口、SBIタイマー、100 Hzのティック
- ビットマップ方式の4 KiB物理ページアロケーター
- activeなSv39カーネルアドレス空間と`satp`切り替え後の写像検査
- 静的RISC-V 64 ELFを検証し、inactiveな`LoadedImage`として保持する実行前loader
- U-mode遷移、user trap context、`write`と`exit`のsystem call、実行用frameの回収
- 予約済みMiniBundleを検証し、boot payloadをU-modeで実行する経路
- ヒープを使わない固定長入力を持つ対話シェル
- NEORV32のIMEMとDMEMへ配置するRV32IMカーネルと、UARTシェル
- ホストテスト、RISC-Vクロスビルド、QEMUテストを統合する`cargo xtask`

ここまでの実装に、動的ヒープ、scheduler、virtio-blkとFAT32、file descriptor、process間の連携、network、SMPの一部が続きます。
第13章以降で機能を追加し、その先の計画は[発展ロードマップ](../reference/roadmap.md)にあります。

ソースコードを読む入口も確認しておきましょう。
OpenSBIから最初に実行される[`entry.S`](../../kernel/src/arch/riscv64/entry.S)は、スタックとBSSを準備します。
続いて[`kernel_main`](../../kernel/src/main.rs)が、トラップ、タイマー、アロケーター、シェルを順に初期化します。
ホスト側では[`xtask`の実行管理](../../xtask/src/lib.rs)がビルドとテストの順序を持ち、[`QEMUハーネス`](../../xtask/src/qemu.rs)がゲストのマーカーと対話記録を検証します。
各章では、この四つの入口から担当モジュールへ進み、ハードウェアやABIに依存する境界と、ホストでもテストできる純粋なロジックを分けて読みます。

## 実行と確認

リポジトリのルートで、開発コマンドの一覧を表示します。

```console
$ cargo xtask
missing xtask command

MiniOS development commands:
cargo xtask setup
cargo xtask build
cargo xtask run
cargo xtask bundle [--name <name>] [--arg <value>]... [--output <path>]
cargo xtask bundle --image <guest-bin> [--image <guest-bin>]... [--output <path>]
cargo xtask test [all|boot|trap|timer|memory|vm|elf|user-entry|user-trap|user-syscall|user-exit|kernel-stack|fdt|heap|virtio|payload|payload-args|payload-stdin|file|file-fd|file-write|file-unlink|file-seek|file-rename|file-mkdir|file-spawn|file-waitpid|file-stat|file-readdir|file-exec|file-fdinherit|file-pipe|user-heap|user-sleep|user-dup|spawn-args|user-shell|sched|sched-io|sched-io-partial|irq-uart|irq-blk|net-arp|net-ping|net-udp|net-sock|proc-fault|proc-kill|shell|smp-boot|smp-timer|smp-ipi]
cargo xtask check
cargo xtask fuzz --target <fat32|elf|boot-payload> [--seed <n>] [--iters <n>] [--max-bytes <n>] [--input-timeout <secs>] [--time-limit <secs>] [--alloc-cap <n>] [--corpus <dir>] [--output <dir>]
cargo xtask fuzz --target <fat32|elf|boot-payload> --input <file>
```

この操作では、終了ステータス1が正しい結果です。
コマンドが省略されたという診断と、開発コマンドの入口が表示されれば、ホスト側の教材を実行できています。

## よくある失敗

- 最初からプロセスやファイルシステムまで設計すると、起動失敗の原因と上位機能の不具合を切り分けにくくなります。
  章の順に検証マーカーを増やしてください。
- ホストとゲストを混同すると、macOS向けのバイナリーをQEMUへ渡したり、RISC-V向けのバイナリーをホストテストで実行したりします。
  ターゲットトリプルと実行場所を確認してください。
- GoやDockerはMiniOSのビルドに必要ありません。
  ビルドに使うのはRust、Cargo、QEMUです。

## 演習

上の機能一覧を「ホストで単体テストできる処理」と「QEMUでなければ確認できない処理」に分類してください。
コマンドパーサーは前者、UARTのMMIOとSBIリセットは後者です。
分類できない項目については、ハードウェアに依存する境界がどこにあるかをメモしてください。

## 次の章

[ガイド索引](README.md)へ戻ることもできます。
次は[第2章「開発環境とQEMU」](02-setup.md)で、RustターゲットとQEMUを同じコマンドから診断します。
