# 2. 開発環境とQEMU

## 学習目標

MiniOSのビルドに必要なRust 1.98.0、`riscv64gc-unknown-none-elf`ターゲット、QEMU 8.2.0以上を`cargo xtask setup`で検査します。
不足しているものがあったときの直し方も説明できるようになります。

## 背景

クロスビルドでは、ホストでRustコンパイラーを使えることと、ゲスト用の標準ライブラリー部品が入っていることは別の条件です。
QEMUのコマンド名と導入するパッケージ名も、macOSとLinuxで異なります。
手順を記憶に頼らずに済むよう、ホストで動く`xtask`が、実行ファイルとバージョンを同じ基準で診断します。

## 実装

`cargo xtask setup`は次の三項目を調べます。

- `rustc --version`：バージョン番号が接尾辞のない安定版Rust 1.98.0と完全に一致すること
- `rustup target list --installed`：ベアメタル用のゲストターゲットが導入されていること
- `qemu-system-riscv64 --version`：QEMU 8.2.0以上であること

Apple Silicon搭載macOSではQEMU 11.1.0で検証しました。
Ubuntu 24.04が提供するQEMU 8.2系をCIでの互換性の下限とし、接尾辞付きのパッケージバージョンも解析します。
ホスト側の実装は[`xtask/src/tools.rs`](../../xtask/src/tools.rs)にあります。

Rustは「1.98以上」ではなく、1.98.0に固定しています。
`1.98.1`のような別の安定版や、`1.98.0-nightly`と`1.98.0-dev`のようなチャンネル接尾辞も拒否します。
検出したバージョンと必要な`1.98.0 stable`はエラーに表示されます。
QEMUだけは8.2.0以上を許すため、二つの方針を混同しないでください。

## 実行と確認

```console
$ cargo xtask setup
Rust: rustc 1.98.0 (88d9e12ae 2026-08-18)
Rust target: riscv64gc-unknown-none-elf
QEMU: 11.1.0
```

コミットハッシュとQEMUのマイナーバージョンは環境によって変わります。
三項目が表示され、終了ステータスが0なら準備完了です。

## よくある失敗

### Rustが固定バージョンではない

`Rust <version> is not supported; exact Rust 1.98.0 stable is required`と表示されたら、`rustc --version`と`rustup show active-toolchain`を確認します。
リポジトリの`rust-toolchain.toml`は変更せず、`rustup toolchain install 1.98.0`を実行してから`setup`を再実行してください。
数値部分が1.98.0でも、`nightly`や`dev`の接尾辞を持つコンパイラーは対象外です。

### RISC-Vターゲットがない

診断に`Rust target riscv64gc-unknown-none-elf is not installed`が含まれる場合は、次のコマンドを実行します。

```sh
rustup target add riscv64gc-unknown-none-elf --toolchain 1.98.0
```

### QEMUがない

macOSでは`brew install qemu`、UbuntuとDebianでは`sudo apt-get install qemu-system-misc`を使います。
導入後に`qemu-system-riscv64 --version`を直接実行し、その後で`setup`を再実行します。
詳しくは[問題の切り分け方](../reference/troubleshooting.md)を参照してください。

## 演習

`rustup target list --installed`と`qemu-system-riscv64 --version`を個別に実行し、`setup`のどの出力に対応するか確認してください。
次に、シェルで`cargo xtask setup`の終了ステータスを調べ、成功が文章ではなくプロセスの境界でも判定されていることを確かめます。

## 次の章

[第1章](01-introduction.md)へ戻れます。
次は[第3章「`no_std`とリンク配置」](03-no-std-and-linking.md)で、OSなしで実行できるELFを作ります。
