# 2. 開発環境とQEMU

## この章の目標

MiniOSをビルドする前に、Rust、RISC-Vターゲット、QEMUが使えることを確認します。環境確認の入口はすべて次のコマンドです。

```sh
cargo xtask setup
```

成功時には、Rustのバージョン、`riscv64gc-unknown-none-elf`ターゲット、QEMUのバージョンが表示されます。`cargo xtask`は、この後のビルド、実行、テストにも使う共通の入口です。

## 検出するツール

`cargo xtask setup`は次の3つを確認します。

- `rustc --version`: カーネルとホスト側ハーネスをビルドするRust stableです。MiniOSはRust 1.98.0に固定します。
- `rustup target list --installed`: 裸のRISC-Vカーネルをクロスビルドするための`riscv64gc-unknown-none-elf`ターゲットです。
- `qemu-system-riscv64 --version`: RISC-V 64の`virt`マシンを実行するQEMUです。MiniOSには9.0.0以上が必要です。

## 検証済みのmacOS環境

Apple Silicon搭載macOSで次のQEMU出力を確認しました。

```text
QEMU emulator version 11.1.0
```

この環境では、セットアップハーネスは次のように表示されます。

```text
Rust: rustc 1.98.0 (88d9e12ae 2026-08-18)
Rust target: riscv64gc-unknown-none-elf
QEMU: 11.1.0
```

## よくある失敗と修正方法

### RISC-Vターゲットがない

`Rust target riscv64gc-unknown-none-elf is not installed`と表示された場合は、次を実行してからもう一度`cargo xtask setup`を実行します。

```sh
rustup target add riscv64gc-unknown-none-elf
```

### QEMUがない

macOSで`qemu-system-riscv64 is not installed`と表示された場合は、HomebrewでQEMUを導入します。

```sh
brew install qemu
```

導入後に、もう一度次を実行してQEMU 9.0.0以上が検出されることを確認してください。

```sh
cargo xtask setup
```

## 確認演習

この章の終わりに`cargo xtask setup`が終了コード0で完了することを確認してください。次章では、この環境を使って`no_std`カーネルとリンカスクリプトを準備します。
