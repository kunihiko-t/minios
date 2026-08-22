# 1. MiniOSで学ぶこと

## 学習目標

この章では、MiniOSの完成地点と学習範囲を把握します。読み終えると、QEMU上で動く小さな
カーネルを題材に、起動、UART、トラップ、タイマー、物理ページ管理、シェルをどの順に
組み立てるのかを説明できます。

## 背景

通常のRustプログラムはOSからプロセス、仮想メモリ、標準入出力を提供してもらいます。
カーネルを作るときは、その「提供される側」を自分で用意します。そこでMiniOSは対象を
RISC-V 64、QEMU `virt`、OpenSBI、1 hart、128 MiB RAMに固定し、一つずつ観察できる規模に
保ちます。Rustの基本構文、Gitの基本操作、CPUが命令を順に実行することを知っていれば
読み始められます。

## 実装

最初のマイルストーンに含めるのは次の機能です。

- OpenSBIからSupervisor modeへ入る起動コードと`no_std` Rust
- 16550互換UARTによる出力、入力、panic診断
- 例外・割り込み入口、SBIタイマー、100 Hzのtick
- bitmap方式の4 KiB物理ページallocator
- heapを使わない固定長入力と6 commandの対話shell
- host test、RISC-V cross build、QEMU testを統合する`cargo xtask`

dynamic heap、Sv39仮想memory、user mode、process、filesystem、network、multi-hart、実機対応は
現在の実装に先行して抽象化せず、[第12章](12-next-steps.md)の発展課題にします。

読み始める前に、現在の境界をソースでも確認しておきます。OpenSBIから最初に実行される
[`entry.S`](../../kernel/src/arch/riscv64/entry.S)がstackとBSSを準備し、
[`kernel_main`](../../kernel/src/main.rs)がtrap、timer、allocator、shellを順に初期化します。
host側では[`xtask`のphase runner](../../xtask/src/lib.rs)がbuildとtestの順序を持ち、
[`QEMU harness`](../../xtask/src/qemu.rs)がguestのmarkerと対話transcriptを検証します。各章では
この4つの入口から担当moduleへ降り、hardware/ABI境界とpure logicを分けて読みます。

## 実行と確認

まずrepository rootで開発command一覧を表示します。

```console
$ cargo xtask
missing xtask command

MiniOS development commands:
  cargo xtask setup
  cargo xtask build
  cargo xtask run
  cargo xtask test [all|boot|trap|timer|memory|shell]
  cargo xtask check
```

この時点では終了status 1で構いません。commandを省略したという診断と、以後使う五つの入口が
表示されれば、host側の教材入口を確認できています。

## よくある失敗

- 最初からprocessやfilesystemまで設計すると、起動失敗の原因と上位機能のbugを分離できません。
  章順に検証markerを増やしてください。
- hostとguestを混同すると、macOS向けbinaryをQEMUへ渡したり、RISC-V binaryをhost testで
  実行したりします。target tripleと実行場所を常に確認します。
- GoやDockerを必須だと思うことがありますが、MiniOSのbuild graphはRust、Cargo、QEMUだけです。

## 演習

上の機能一覧を「hostで単体testできる処理」と「QEMUでなければ確認できない処理」に分類して
ください。command parserは前者、UART MMIOとSBI resetは後者です。分類できない項目は、どこに
hardware境界があるかをメモしておきます。

## 次の章

[ガイド索引](README.md)へ戻ることもできます。次は[第2章: 開発環境とQEMU](02-setup.md)で、
Rust targetとQEMUを同じcommandから診断します。
