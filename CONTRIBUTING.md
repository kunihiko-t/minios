# コントリビューションガイド

MiniOSへの改善提案、修正、学習資料の追加を歓迎します。

## 開発を始める前に

必要なRustターゲットとQEMUを確認するため、リポジトリのルートで次を実行してください。

```sh
cargo xtask setup
```

変更を送る前には、同じ環境で統一検査を実行してください。

```sh
cargo xtask check
```

## parserのfuzz検査

FAT32 volume parser、ELF headerとload plan、boot payloadのmanifest経路に対して、`cargo xtask fuzz`で決定性の変異入力を流せます。
QEMUもゲスト用toolchainも要りません。

```sh
cargo xtask fuzz --target fat32
cargo xtask fuzz --target elf --seed 9 --iters 100000
```

対象は`fat32`、`elf`、`boot-payload`の三つで、入力は`xtask/corpus/<target>/`のseedを乱数seedで変異させて作ります。
同じseedは同じ入力列を生成します。
panic、入力あたりtimeout（`--input-timeout`、既定5秒）、allocation上限（`--alloc-cap`、既定16 MiB）の超過だけをfindingとして報告し、parserが返す通常のerrorはfindingに含めません。

findingの入力は`target/fuzz/`に保存され、次のcommandで再現できます。

```sh
cargo xtask fuzz --target elf --input target/fuzz/<file>.bin
```

`cargo xtask check`には含めません。
parserへ変更を加えたときや、不審な入力を見つけたときに実行してください。

## コードと文書

コメントは、日本語の学習ガイドと同じく、読者がOSの仕組みを理解する助けになる内容を日本語で記述します。
実装そのものから明らかな操作の繰り返しではなく、設計上の理由、ハードウェア上の制約、失敗時の意味を説明してください。

## ライセンス

貢献を送ることで、その貢献をMIT LicenseまたはApache License 2.0の条件で提供することに同意したものとします。
SPDX表記は`MIT OR Apache-2.0`です。

## 脆弱性の報告

未公開の脆弱性は公開Issueに書かないでください。
[Security Policy](SECURITY.md)のGitHub private vulnerability reportingを使って非公開で報告してください。
