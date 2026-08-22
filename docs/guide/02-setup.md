# 2. 開発環境とQEMU

## 学習目標

MiniOSのbuildに必要なRust 1.98.0、`riscv64gc-unknown-none-elf` target、QEMU 8.2.0以上を
`cargo xtask setup`で検査し、不足時の直し方を説明できるようになります。

## 背景

cross buildでは、host compilerが使えることとguest用standard library componentが入っている
ことは別問題です。またQEMUのcommand名と導入package名はmacOSとLinuxで異なります。手順を
人間の記憶に任せず、host toolの`xtask`が実行fileとversionを同じ基準で診断します。

## 実装

`cargo xtask setup`は次を調べます。

- `rustc --version`: version tokenがsuffixなしのRust stable 1.98.0と完全一致すること
- `rustup target list --installed`: bare-metal guest target
- `qemu-system-riscv64 --version`: QEMU 8.2.0以上

Apple Silicon macOSではQEMU 11.1.0で検証しました。Ubuntu 24.04のQEMU 8.2 seriesをCI互換の
下限にし、suffix付きpackage versionも解析します。関係するhost実装は
[`xtask/src/tools.rs`](../../xtask/src/tools.rs)です。

Rustは「1.98以上」ではなくexact pinです。`1.98.1`など別stableや`1.98.0-nightly`/`-dev`のような
channel suffixも拒否し、検出したtokenと必要な`1.98.0 stable`をerrorに表示します。QEMUだけは
8.2.0以上というcompatibility floorであり、この二つのversion policyを混同しません。

## 実行と確認

```console
$ cargo xtask setup
Rust: rustc 1.98.0 (88d9e12ae 2026-08-18)
Rust target: riscv64gc-unknown-none-elf
QEMU: 11.1.0
```

commit hashやQEMUのminor versionは環境により変わります。三項目が表示され、終了status 0なら
準備完了です。

## よくある失敗

### Rustが固定versionではない

`Rust <version> is not supported; exact Rust 1.98.0 stable is required`と出たら、
`rustc --version`と`rustup show active-toolchain`を確認します。repositoryの`rust-toolchain.toml`を
変更せず、`rustup toolchain install 1.98.0`を実行してからsetupを再実行してください。nightlyやdev
suffixを持つcompilerは、numeric部分が1.98.0でもacceptance対象外です。

### RISC-V targetがない

症状に`Rust target riscv64gc-unknown-none-elf is not installed`が含まれる場合は次を実行します。

```sh
rustup target add riscv64gc-unknown-none-elf --toolchain 1.98.0
```

### QEMUがない

macOSは`brew install qemu`、Ubuntu/Debianは`sudo apt-get install qemu-system-misc`を使います。
導入後、直接`qemu-system-riscv64 --version`を確認してからsetupを再実行します。詳細は
[troubleshooting](../reference/troubleshooting.md)にもまとめています。

## 演習

`rustup target list --installed`と`qemu-system-riscv64 --version`を個別に実行し、setup出力の
どの行へ対応するか確認してください。次に`cargo xtask setup`の終了statusをshellで確認し、
文章ではなくprocess境界で成功が判定されていることを確かめます。

## 次の章

[第1章](01-introduction.md)へ戻れます。次は
[第3章: `no_std`とlink配置](03-no-std-and-linking.md)で、OSなしで実行できるELFを作ります。
