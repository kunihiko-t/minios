# MiniOS

MiniOSは、RustとRISC-V 64でOSの基礎を段階的に学ぶための小さな`no_std`カーネルです。
QEMU `virt`上のOpenSBIからS-modeで起動し、UARTシェル、トラップ、100 Hzのタイマー、ビットマップ方式の物理ページアロケーターを備えています。
日本語の学習ガイドと、同じ結果を繰り返し確認できるテストハーネスも用意しています。

## 五つのコマンドで試す

Rust 1.98.0、RISC-Vターゲット、QEMU 8.2.0以上を用意し、リポジトリのルートで次のコマンドを順に実行します。

```sh
cargo xtask setup
cargo xtask build
cargo xtask run
cargo xtask test
cargo xtask check
```

`cargo xtask run`はQEMUを対話モードで起動します。
プロンプトが表示されたら次のように動作を確かめ、最後に`shutdown`で正常終了してください。

```text
MiniOS booting...
hart id: 0
minios> help
help      Show available commands
info      Show system information
uptime    Show elapsed time
memory    Show physical memory statistics
clear     Clear the terminal
shutdown  Shut down MiniOS
minios> info
MiniOS 0.1.0 on RISC-V 64
hart id: 0
minios> uptime
uptime: 120 ms
ticks: 12
minios> shutdown
shutting down
```

`uptime`の数値は実行時点で変わりますが、`uptime: <n> ms`の直後に`ticks: <n>`が1行ずつ表示されます。
シングルハート構成の`info`は、バナーに続けて`hart id: 0`を表示します。

## 対応環境

| 区分 | 対応範囲 | 検証条件と制約 |
| --- | --- | --- |
| ホスト | Apple Silicon搭載macOS | 主な開発環境としてQEMU 11.1.0で検証 |
| ホスト | LinuxおよびUbuntu 24.04 | GitHub Actionsで検証し、QEMU 8.2系を互換性の下限として使用 |
| Rust | 安定版1.98.0 | rustfmt、Clippy、`riscv64gc-unknown-none-elf`を固定 |
| ゲスト | RISC-V RV64GCおよびQEMU `virt` | OpenSBI、S-mode、1ハート、128 MiB RAM |
| コンソール | 16550互換UART | MMIOベース`0x1000_0000`、QEMUのシリアル標準入出力 |

Windowsホスト、別のQEMUマシン、マルチハート、実機は、現在の受け入れテストに含まれません。

## 学習ガイド

全体の索引と各章の到達目標は[学習ガイド](docs/guide/README.md)にあります。

1. [MiniOSで学ぶこと](docs/guide/01-introduction.md)
2. [開発環境とQEMU](docs/guide/02-setup.md)
3. [`no_std`とリンク配置](docs/guide/03-no-std-and-linking.md)
4. [OpenSBIからの起動](docs/guide/04-boot-with-opensbi.md)
5. [UART](docs/guide/05-uart.md)
6. [パニックと緊急診断](docs/guide/06-panic-and-diagnostics.md)
7. [例外と割り込み](docs/guide/07-traps-and-interrupts.md)
8. [タイマー割り込み](docs/guide/08-timer-interrupts.md)
9. [物理ページ管理](docs/guide/09-physical-memory.md)
10. [UARTシェル](docs/guide/10-shell.md)
11. [テストハーネス](docs/guide/11-test-harness.md)
12. [次に作るもの](docs/guide/12-next-steps.md)

## 設計資料

- [全体構成と八つの起動段階](docs/reference/architecture.md)
- [QEMU `virt`のメモリーマップ](docs/reference/memory-map.md)
- [MiniContainer Guest ABI](docs/reference/minicontainer-abi.md)
- [用語集](docs/reference/glossary.md)
- [問題の切り分け方](docs/reference/troubleshooting.md)
- [発展ロードマップ](docs/reference/roadmap.md)

## テスト

対象を絞るときは`cargo xtask test [all|boot|trap|timer|memory|shell]`を使います。
リリース前の全検査は次のコマンドで実行します。

```sh
cargo xtask check
```

このコマンドは、書式、Markdownリンク、ガイドの構造、公開文書、Clippy、クロスビルド、ホストテスト、QEMUの5経路を17段階で検査します。

## 現在の制約

動的ヒープ、Sv39仮想メモリー、ユーザーモード、システムコール、プロセス、VirtIO、ファイルシステム、ネットワーク、マルチハート、Device Tree解析、実機ドライバーは未実装です。
ハードウェアアドレス、10 MHzのタイムベース、128 MiBの上端はQEMU `virt`に固定しています。
シェルが受け付ける入力は印字可能なASCIIで最大128バイトです。
永続ストレージとセキュリティー境界は提供しません。

## セキュリティー上の位置づけ

MiniOSはOSの仕組みを学ぶための実装であり、本番用のセキュリティー境界ではありません。
未信頼コードの隔離には使用せず、脆弱性の報告方法は[Security Policy](SECURITY.md)を参照してください。

## ライセンス

MiniOSはMIT LicenseまたはApache License 2.0の条件で利用できます。
詳細は[LICENSE-MIT](LICENSE-MIT)と[LICENSE-APACHE](LICENSE-APACHE)を参照してください。
SPDX表記は`MIT OR Apache-2.0`です。
