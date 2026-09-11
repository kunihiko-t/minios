# MiniOS

MiniOSは、RustとRISC-VでOSの基礎を段階的に学ぶための小さな`no_std`カーネルです。
主教材はQEMU `virt`上のRISC-V 64環境です。
OpenSBIからS-modeで起動し、UARTシェル、トラップ、100 Hzのタイマー、ビットマップ方式の物理ページアロケーター、Sv39のカーネルアドレス空間を備えています。
静的なRISC-V 64 ELFを検証し、U-modeで`write`と`exit`を実行するloaderも備えています。
MiniBundle boot payloadはQEMU loaderから予約物理windowへ渡せます。
NEORV32向けには、RISC-V 32のM-modeで起動してUARTシェルを動かす小さな実機経路があります。
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
| ホスト | Apple Silicon搭載macOS、Ubuntu 24.04 | macOSはQEMU 11.1.0、UbuntuはGitHub ActionsとQEMU 8.2系で検証 |
| Rust | 安定版1.98.0 | rustfmt、Clippy、RV64GCとRV32IMのベアメタルターゲットを固定 |
| QEMUゲスト | RISC-V RV64GCおよびQEMU `virt` | OpenSBI、S-mode、1ハート、128 MiB RAM |
| NEORV32 | RISC-V RV32IM | M-mode、内蔵IMEM 24,288バイト、内蔵DMEM 16,192バイト |
| QEMUコンソール | 16550互換UART | MMIOベース`0x1000_0000`、シリアル標準入出力 |
| NEORV32コンソール | UART0 | MMIOベース`0xfff5_0000`、96 MHz、19,200 baud |

Windowsホスト、別のQEMUマシン、マルチハート、NEORV32以外の実機は保証しません。

## NEORV32向けRV32ビルド

NEORV32用カーネルは、次のコマンドでreleaseビルドします。

```sh
cargo build -p minios-kernel --bin minios-kernel --target riscv32im-unknown-none-elf --release --locked
```

出力は`target/riscv32im-unknown-none-elf/release/minios-kernel`です。
このELFは、`pc=0`からM-modeで始まり、IMEMに続けて格納した`.data`の初期値をDMEMへコピーしてからBSSをゼロ化します。
実機へ書き込む形式と手順は、NEORV32を組み込んだFPGA構成に合わせて選んでください。

起動後のRV32シェルは`help`、`info`、`echo`を実行できます。
RV64側の`uptime`、`memory`、`clear`、`shutdown`を入力すると、`command unavailable on RV32`を返します。
`cargo xtask check`はRV32のClippyとreleaseビルドまで検査しますが、実機UARTの動作確認は開発者が行います。

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
13. [Sv39と単一アドレス空間](docs/guide/13-sv39.md)
14. [ELFを実行前アドレス空間へ配置する](docs/guide/14-elf-loading.md)
15. [U-modeでELFを実行する](docs/guide/15-user-mode.md)
16. [boot payloadを実行する](docs/guide/16-boot-payload.md)

## 設計資料

- [全体構成と起動段階](docs/reference/architecture.md)
- [QEMU `virt`のメモリーマップ](docs/reference/memory-map.md)
- [MiniContainer Guest ABI](docs/reference/minicontainer-abi.md)
- [用語集](docs/reference/glossary.md)
- [問題の切り分け方](docs/reference/troubleshooting.md)
- [発展ロードマップ](docs/reference/roadmap.md)

## テスト

対象を絞るときは`cargo xtask test [all|boot|trap|timer|memory|vm|elf|user-entry|user-trap|user-syscall|user-exit|payload|payload-args|shell]`を使います。
リリース前の全検査は次のコマンドで実行します。

```sh
cargo xtask check
```

このコマンドは、書式、Markdownリンク、ガイドの構造、公開文書、RV64とRV32のClippyおよびクロスビルド、ホストテスト、QEMUの13経路を28段階で検査します。

## 現在の制約

MiniOSが実行対象にするのは、MiniBundleへ格納した静的RISC-V 64 ELFだけです。
OCI image、network、volume、Linux binary互換、multi-tenant isolation、Windowsは保証しません。
動的ヒープ、プロセス管理、VirtIO、ファイルシステム、network、マルチハート、Device Tree解析、NEORV32以外の実機driverは未実装です。
`write`はstdoutとstderrだけを扱い、`exit`は一つのU-mode実行をkernelへ戻します。
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
