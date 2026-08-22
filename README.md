# MiniOS

MiniOSはRustとRISC-V 64でOSの基礎を段階的に学ぶための、小さな`no_std` kernelです。QEMU `virt`上の
OpenSBIからS-modeで起動し、UART shell、trap、100 Hz timer、bitmap物理page allocatorを日本語教材と
再現可能なtest harnessと一緒に提供します。

## quick start: 五つのcommand

Rust 1.98.0、RISC-V target、QEMU 8.2.0以上を用意し、repository rootで順に実行します。

```sh
cargo xtask setup
cargo xtask build
cargo xtask run
cargo xtask test
cargo xtask check
```

`cargo xtask run`はQEMUを対話起動します。promptが出たら次のように確認し、最後は`shutdown`で正常終了
してください。

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

`uptime`の数値は実行時点で変わりますが、`uptime: <n> ms`の直後に`ticks: <n>`が1行ずつ出ます。
single-hart構成の`info`はbannerを維持し、その次の行で`hart id: 0`を報告します。

## supported matrix

| 区分 | 対応範囲 | 検証・制約 |
| --- | --- | --- |
| host | Apple Silicon macOS | 主開発環境、QEMU 11.1.0で検証 |
| host | Linux / Ubuntu 24.04 | GitHub Actions、QEMU 8.2 seriesを互換下限に使用 |
| Rust | 1.98.0 stable | rustfmt、Clippy、`riscv64gc-unknown-none-elf`を固定 |
| guest | RISC-V RV64GC / QEMU `virt` | OpenSBI、S-mode、1 hart、128 MiB RAM |
| console | 16550 compatible UART | MMIO base `0x1000_0000`、QEMU serial stdio |

Windows host、別QEMU machine、multi-hart、実機は現在のacceptance対象外です。

## 学習ガイド

全体索引と各章のlearning outcomeは[学習ガイド](docs/guide/README.md)にあります。

1. [MiniOSで学ぶこと](docs/guide/01-introduction.md)
2. [開発環境とQEMU](docs/guide/02-setup.md)
3. [`no_std`とlink配置](docs/guide/03-no-std-and-linking.md)
4. [OpenSBIからの起動](docs/guide/04-boot-with-opensbi.md)
5. [UART](docs/guide/05-uart.md)
6. [panicと緊急診断](docs/guide/06-panic-and-diagnostics.md)
7. [例外・割り込み](docs/guide/07-traps-and-interrupts.md)
8. [timer割り込み](docs/guide/08-timer-interrupts.md)
9. [物理page管理](docs/guide/09-physical-memory.md)
10. [UART shell](docs/guide/10-shell.md)
11. [test harness](docs/guide/11-test-harness.md)
12. [次に作るもの](docs/guide/12-next-steps.md)

## architectureとreference

- [全体architectureと8-step boot flow](docs/reference/architecture.md)
- [QEMU `virt` memory map](docs/reference/memory-map.md)
- [用語集](docs/reference/glossary.md)
- [troubleshooting](docs/reference/troubleshooting.md)
- [発展roadmap](docs/reference/roadmap.md)

## test

focused testは`cargo xtask test [all|boot|trap|timer|memory|shell]`、release gateは次です。

```sh
cargo xtask check
```

format、Markdown link、guide構造、Clippy、cross build、host tests、QEMU 5経路を14 phaseで実行します。

## current limitations

dynamic heap、Sv39 virtual memory、user mode、system calls、process、VirtIO、filesystem、network、
multi-hart、Device Tree解析、実機driverは未実装です。hardware address、10 MHz timebase、128 MiB上端は
QEMU `virt`へ固定しています。shellはprintable ASCII最大128 bytesで、永続storageもsecurity境界も
提供しません。

## license status

このrepositoryには現時点で`LICENSE` fileがありません。したがって再利用・再配布条件はまだ明示
されていません。公開利用へ移す前に、ownerが意図するlicenseを選び`LICENSE`とこの節を更新してください。
