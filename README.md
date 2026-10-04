# MiniOS

MiniOSは、RustとRISC-VでOSの基礎を段階的に学ぶための小さな`no_std`カーネルです。
主教材はQEMU `virt`上のRISC-V 64環境です。
OpenSBIからS-modeで起動し、UARTシェル、トラップ、100 Hzのタイマー、ビットマップ方式の物理ページアロケーター、Sv39のカーネルアドレス空間を備えています。
静的なRISC-V 64 ELFを検証してU-modeで実行するloaderと、複数のprocessをタイマー割り込みで切り替えるround-robin schedulerを備えています。
virtio-blk上のFAT32 volumeを読み書きでき、user programはfile descriptor、process生成、pipeなどのsystem callを使えます。
system callの一覧は[MiniContainer Guest ABI](docs/reference/minicontainer-abi.md#syscall-abi-v1)にあります。
最初のuser programはMiniBundle boot payloadとしてQEMU loaderから渡し、そのprogramが`spawn`でFAT32上のELFを別のprocessとして起動できます。
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
ls        List a directory
cat       Read a file
rm        Remove a file
mkdir     Create a directory
rmdir     Remove an empty directory
mv        Rename a file or directory
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
`cargo xtask run`は起動ごとに検査用のFAT32 disk imageを生成して接続するため、`ls`や`cat`でその中身を確かめられます。

## 対応環境

| 区分 | 対応範囲 | 検証条件と制約 |
| --- | --- | --- |
| ホスト | Apple Silicon搭載macOS、Ubuntu 24.04 | macOSはQEMU 11.1.0、UbuntuはGitHub ActionsとQEMU 8.2系で検証 |
| Rust | 安定版1.98.0 | rustfmt、Clippy、RV64GCとRV32IMのベアメタルターゲットを固定 |
| QEMUゲスト | RISC-V RV64GCおよびQEMU `virt` | OpenSBI、S-mode、1ハート、128 MiB RAM |
| NEORV32 | RISC-V RV32IM | M-mode、内蔵IMEM 32,768バイト、内蔵DMEM 16,192バイト |
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

起動後のRV32シェルは`help`、`info`、`uptime`、`memory`、`echo`、`ls`、`cat`、`clear`、`shutdown`を実行できます。
`uptime`は`cycle`カウンターをシステムクロックで換算し、`memory`はIMEM/DMEMの占有量を表示し、`shutdown`は`wfi`でCPUを停止します。
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
17. [Rustでユーザープログラムを書く](docs/guide/17-rust-guest.md)
18. [Device Treeとヒープの成長](docs/guide/18-heap-and-fdt.md)
19. [プリエンプティブscheduler](docs/guide/19-scheduler.md)
20. [virtio-blkでディスクを読み書きする](docs/guide/20-virtio-blk.md)
21. [FAT32の読み書き](docs/guide/21-fat32.md)
22. [file descriptorとpipe](docs/guide/22-file-descriptors-and-pipes.md)
23. [spawn、waitpid、exec](docs/guide/23-process-syscalls.md)

## 設計資料

- [全体構成と起動段階](docs/reference/architecture.md)
- [QEMU `virt`のメモリーマップ](docs/reference/memory-map.md)
- [MiniContainer Guest ABI](docs/reference/minicontainer-abi.md)
- [NEORV32でアプリケーションを動かす方式の検討](docs/reference/neorv32-applications.md)
- [NEORV32 read-only FAT32設計](docs/reference/sd-fat32.md)
- [用語集](docs/reference/glossary.md)
- [問題の切り分け方](docs/reference/troubleshooting.md)
- [発展ロードマップ](docs/reference/roadmap.md)

## テスト

対象を絞るときは`cargo xtask test <対象>`を使います。
指定できる対象の一覧は、引数を付けずに`cargo xtask`を実行すると表示されます。
リリース前の全検査は次のコマンドで実行します。

```sh
cargo xtask check
```

このコマンドは、書式、Markdownリンク、ガイドの構造、公開文書、RV64とRV32のClippyおよびクロスビルド、ホストテスト、全QEMU経路を順に検査します。
段階の一覧は[テストハーネスの章](docs/guide/11-test-harness.md)にあります。

## 現在の制約

MiniOSが実行対象にするのは、静的RISC-V 64 ELFだけです。
OCI image、volume、Linux binary互換、multi-tenant isolation、Windowsは保証しません。

次の機能は未実装です。

- **network**：virtio-netとprotocol stackはありません。
- **マルチハート**：1ハートだけを起動し、kernel内の共有状態はlockを前提にしていません。
- **割り込み駆動のI/O**：UART入力とvirtio-blkの完了はpollingで待ちます。
- **user heap**：guestが実行中にメモリーを追加で確保するsystem callはありません。
- **NEORV32以外の実機driver**：実機経路はNEORV32のUARTとSDカードだけです。

実装済みの機能にも、教材として小さく保つための上限があります。

- 同時に動かせるprocessは4個までで、各processが開けるfileも4個までです。
- pipeのbufferは256 byteで、満杯のときは書き込み側が待ちます。
- FAT32へ新しく作れるfile名とdirectory名は、8.3形式へ正規化できる名前だけです。
- `cargo xtask run`と各testは起動ごとにdisk imageを作り直して終了時に削除するため、書き込んだ内容は次の起動へ残りません。
- ヒープはmanaged RAM末尾の1 MiBから始まり、不足するとframe poolのpageを取り込んで成長しますが、取り込んだpageは返しません。
- ハードウェアアドレス、タイムベース、RAMの上端はOpenSBIが渡すDevice Treeから発見しますが、対象machineはQEMU `virt`の配置契約に限定しています。
- シェルが受け付ける入力は印字可能なASCIIで最大128バイトです。

今後の計画は[発展ロードマップ](docs/reference/roadmap.md)にあります。

## セキュリティー上の位置づけ

MiniOSはOSの仕組みを学ぶための実装であり、本番用のセキュリティー境界ではありません。
未信頼コードの隔離には使用せず、脆弱性の報告方法は[Security Policy](SECURITY.md)を参照してください。

## ライセンス

MiniOSはMIT LicenseまたはApache License 2.0の条件で利用できます。
詳細は[LICENSE-MIT](LICENSE-MIT)と[LICENSE-APACHE](LICENSE-APACHE)を参照してください。
SPDX表記は`MIT OR Apache-2.0`です。
