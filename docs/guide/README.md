# MiniOS学習ガイド

このガイドは前章の成果を次章の前提にするhands-on教材です。初回は1から12まで順に読み、実装中は
各章の「実行と確認」「よくある失敗」へ戻ってください。

## 全12章

1. [MiniOSで学ぶこと](01-introduction.md) — milestoneの範囲とhost/guest境界を分類できる。
2. [開発環境とQEMU](02-setup.md) — Rust targetとQEMUを再現可能なcommandで診断できる。
3. [`no_std`とlink配置](03-no-std-and-linking.md) — ELF sectionとkernel memory境界を説明できる。
4. [OpenSBIからの起動](04-boot-with-opensbi.md) — firmware ABIからRust entryまでを追跡できる。
5. [UARTで文字を出す・受け取る](05-uart.md) — volatile MMIOとconsole層の責務を区別できる。
6. [panicと緊急診断](06-panic-and-diagnostics.md) — lock不要のfailure pathを設計できる。
7. [RISC-Vの例外・割り込み](07-traps-and-interrupts.md) — CSRとtrap frameから原因を読める。
8. [supervisor timer割り込み](08-timer-interrupts.md) — SBI deadlineと100 Hz tickを説明できる。
9. [物理memoryとpage管理](09-physical-memory.md) — bitmap ownershipとstats不変条件を検証できる。
10. [UART対話shell](10-shell.md) — bounded inputとcommand作用を分離できる。
11. [test harnessの仕組み](11-test-harness.md) — host/QEMU/CIを同じ14 phaseで検証できる。
12. [次に作るもの](12-next-steps.md) — 11個の発展項目をprerequisite順に計画できる。

## navigationの約束

各章末の「次の章」には前章、次章、必要なreferenceへのlinkがあります。初回はnext linkをたどり、
途中で用語やaddressを確認したらbrowserの戻る操作で同じ位置へ戻ります。第1章のpreviousと第12章の
nextはこの索引です。章番号付きfile `01-...md`から`12-...md`はすべて、`学習目標`、`背景`、`実装`、
`実行と確認`、`よくある失敗`、`演習`、`次の章`の7 sectionを持ち、`cargo xtask check`が構造を検査します。

[repository README](../../README.md) | [architecture](../reference/architecture.md) |
[troubleshooting](../reference/troubleshooting.md)
