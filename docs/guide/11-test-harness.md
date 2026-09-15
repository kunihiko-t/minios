# 11. テストハーネスの仕組み

## 学習目標

`cargo xtask`をローカル開発とCIの共通入口にする理由を学びます。
読み終えると、ホスト単体テストとRISC-Vゲスト統合テストの違い、QEMUのマーカーモードと対話モード、時間切れになったプロセスの回収、対話記録の読み方を説明できるようになります。
`cargo xtask check`が実行する33段階の順序も確認します。

## 背景

カーネル本体はRV64GCとRV32IMのベアメタルターゲット向けに作る`no_std`バイナリーです。
一方、`xtask`は開発機のOS上で動く通常のRustプログラムで、CargoとQEMUを子プロセスとして起動します。
この章でいう**ホスト**は`xtask`を実行するmacOSまたはLinux、**ゲスト**はQEMUのRISC-V仮想機内で動くMiniOSです。
プロセスの終了ステータス、標準入力、標準出力、標準エラー、制限時間も使います。

## 実装

### `xtask`に開発コマンドを集める理由

Cargoは`cargo xtask ...`を、ワークスペース内の`xtask`バイナリーへ渡せます。
この小さなホスト用プログラムへ、環境診断、クロスビルド、QEMUの起動、検証順序を集約しています。
別のタスク実行ツールや、OSごとのシェルスクリプトは必要ありません。
公開しているコマンドは次の六系統です。

```text
cargo xtask setup
cargo xtask build
cargo xtask run
cargo xtask bundle [--name <name>] [--arg <value>]... [--output <path>]
cargo xtask test [all|boot|trap|timer|memory|vm|elf|user-entry|user-trap|user-syscall|user-exit|fdt|heap|payload|payload-args|payload-stdin|sched|sched-io|sched-io-partial|shell]
cargo xtask check
```

`xtask`の内部から`cargo xtask`を再帰的に起動すると、検証が終わりません。
`check`は、書式、Clippy、ビルド、ホストテストなどCargoが担う操作だけをCargoの子プロセスへ渡します。
QEMUテストは`xtask`内のRust関数を直接呼びます。
この分担により、順序と失敗処理を一つの実行管理部へ集め、再帰呼び出しを防いでいます。

### ホストテストとゲストテストの役割

ハードウェアに依存しない処理は、ホスト上で速く細かく検査します。
対象は、シェルのパーサー、固定長の行バッファー、ティックから時間への変換、物理フレームアロケーター、Sv39のpage walk、ELFの検証と配置、`xtask`自身の引数解析とプロセス処理です。
通常のRustテスト名から失敗箇所を絞れるため、ロジックの不具合を調べるだけならQEMUを起動する必要はありません。

ゲストテストは、クロスビルドだけでは分からないOpenSBI、CSR、トラップの入口、SBIタイマー、UART、SBIリセットの接続をRISC-V仮想機上で検査します。
`cargo xtask test`を引数なしで実行すると`all`となり、次の順序で進みます。

1. `cargo test -p minios-kernel --lib`
2. `cargo test -p xtask`
3. QEMU起動テスト
4. QEMUトラップテスト
5. QEMUタイマーテスト
6. QEMUメモリーテスト
7. QEMU VMテスト
8. QEMU ELFテスト
9. QEMU user-entryテスト
10. QEMU user-trapテスト
11. QEMU user-syscallテスト
12. QEMU user-exitテスト
13. QEMU FDTテスト
14. QEMUヒープテスト
15. QEMU payloadテスト
16. QEMU payload-argsテスト
17. QEMU payload-stdinテスト
18. QEMU schedテスト
19. QEMU sched-ioテスト
20. QEMU sched-io-partialテスト
21. QEMUシェルテスト

速いホストテストを先に実行してから、起動、トラップ、タイマー、メモリー、VM、ELF、U-mode、FDT、ヒープ、payload、payload-args、payload-stdin、スケジューラー、stdin待ちprocessを含むスケジューラー、分割frame受信、対話シェルという依存関係の順にゲストの19経路を確認します。

### QEMUの三つの検証モード

起動、トラップ、タイマー、メモリー、VM、ELF、user-entry、user-trap、user-syscall、FDT、ヒープのテストは**マーカーモード**です。
テストごとのCargo機能を有効にしてカーネルをビルドし、UARTの記録、終了ステータス0、次の完全一致するマーカーを要求します。

```text
[MINIOS_TEST] boot: ok
[MINIOS_TEST] trap: ok
[MINIOS_TEST] timer: ok
[MINIOS_TEST] memory: ok
[MINIOS_TEST] vm: ok
[MINIOS_TEST] elf: ok
[MINIOS_TEST] user-entry: reached
[MINIOS_TEST] user-trap: rejected
[MINIOS_TEST] user-syscall: ok
[MINIOS_TEST] fdt: ram=0x80000000..0x88000000 uart=0x10000000 timebase=10000000
[MINIOS_TEST] heap: ok
```

マーカーがなければ、終了ステータスが0でも成功とは見なしません。
この条件により、「QEMUは終了したが、検査対象のカーネル処理へ到達しなかった」という誤検出を防ぎます。
CRLFをLFへ変換した後の一行と完全一致することを調べるため、診断行にマーカーを含むだけの場合や、似た文字列は通りません。

`user-exit`、`payload`、`payload-args`、`payload-stdin`、`sched`、`sched-io`は**control frameモード**です。
MiniContainer control protocolのframeを解析し、Ready、標準出力、標準エラー、Exit、回収診断の順序と内容を検査します。
`payload-args`ではmanifestの`name`と二つの`arg=`が、初期スタックの`argv`を通って順番どおり標準出力へ届くことを確認します。
この経路には標準エラーframeがないため、検証部はReady、三つの標準出力、Exit、回収診断だけを要求します。
`sched`ではmanifest v2の二つのimageを通常カーネルへ渡し、busy-waitするprocessの出力`a1`と`a3`の間に短命processの`b1`が挟まること、二つの`ProcExit`frameが届くこと、回収診断が`switches`を1以上として報告することを要求します。
この順序条件は、プリエンプションを伴わない逐次実行を確実に弾きます。
`sched-io`では三つのimageを渡し、`read`でstdin待ちするreader processの`r1`と`r2`の間に、busy-waitするprocessと短命processの出力がすべて挟まることを要求します。
入力frameは`b3`を観測してから送るため、`b3 < r2`の順序は「block中も他processが進む」ことの直接証拠です。
`sched-io-partial`では同じimageへStdin frameを分割して送り、header途中で書き込みを止めてから残りを送ります。
途中受信でもreaderが再びblockしてframeが正しく完結することを、`r2`到達と正常な`ProcExit`で検証します。

シェルテストは**対話モード**です。
通常のカーネルが最初の`minios> `を出すまで待ち、`help`、`info`、`uptime`、`memory`、`not-a-command`、`shutdown`を標準入力へ送ります。
検証部は、各コマンドのエコー、毎回の新しいプロンプト、安定した応答、`hart id: 0`、稼働時間とティックとメモリー統計の数値形式、最後の終了ステータス0を要求します。
最初のプロンプト以降を順序付きの記録として読み、`help`が返す六行を含めて各行の位置を検査します。
応答の並べ替え、プロンプトの重複、`minios> helper`のような前方一致、途中の予期しない行は失敗です。
末尾の空行だけは許可します。
この記録から、UARTの送受信、パーサー、タイマー、アロケーター、SBIリセットを同じQEMUセッション内で検査できます。

### 時間切れと失敗時の記録

各QEMUテストの制限時間は5秒です。
`xtask`は標準出力と標準エラーを起動直後から別々のスレッドで読み、パイプのバッファーが埋まってゲストが停止することを防ぎます。
制限時間を超えるとQEMUを終了し、必ず`wait`で回収するため、子プロセスを残しません。
エラーには、シェルで安全に再実行できるよう引用したQEMUプログラム、カーネルのパス、全オプション、制限時間、受信済みの標準出力と標準エラーを含めます。
プロセスの回収自体が失敗した場合は、その診断も残ります。
QEMU起動前にビルドが失敗した場合も、Cargoコマンドにはテスト用機能が残るため、準備していたゲストイメージを追跡できます。

マーカー不足、シェル出力不足、0以外の終了ステータスでも、対話記録を省略しません。
失敗した段階の見出しと、最後に見えた初期化行やマーカーを照合すると、ビルド失敗、ゲスト内の明示的な失敗、停止を区別できます。
Cargoの子プロセスが失敗した場合も、実行コマンド、終了ステータス、標準出力、標準エラーを表示します。

### `check`が実行する33段階

`cargo xtask check`は、次の33段階をこの順に実行し、最初の失敗で停止します。
書式検査の直後に教材のリンクと章構造を調べ、その後でコンパイラーを動かします。
静的検査より前にQEMUを起動しないことと、検査していないバイナリーをゲストテストへ渡さないことが、この順序を固定する理由です。

```text
1. cargo fmt --all -- --check
2. check local Markdown links
3. check guide chapter structure
4. check public publication files
5. cargo clippy -p xtask --all-targets --locked -- -D warnings
6. cargo clippy -p minios-abi --all-targets --locked -- -D warnings
7. cargo clippy -p minios-guest --target riscv64gc-unknown-none-elf --release --locked -- -D warnings
8. cargo clippy -p minios-kernel --lib --locked -- -D warnings
9. cargo clippy -p minios-kernel --bin minios-kernel --target riscv64gc-unknown-none-elf --locked -- -D warnings
10. cargo clippy -p minios-kernel --bin minios-kernel --target riscv32im-unknown-none-elf --release --locked -- -D warnings
11. cargo build -p minios-kernel --bin minios-kernel --target riscv64gc-unknown-none-elf --locked
12. cargo build -p minios-kernel --bin minios-kernel --target riscv32im-unknown-none-elf --release --locked
13. cargo test -p minios-abi --locked
14. cargo test -p minios-kernel --lib --locked
15. cargo test -p xtask --locked
16. QEMU boot test
17. QEMU trap test
18. QEMU timer test
19. QEMU memory test
20. QEMU VM test
21. QEMU ELF test
22. QEMU user-entry test
23. QEMU user-trap test
24. QEMU user-syscall test
25. QEMU user-exit test
26. QEMU fdt test
27. QEMU heap test
28. QEMU payload test
29. QEMU payload-args test
30. QEMU payload-stdin test
31. QEMU sched test
32. QEMU sched-io test
33. QEMU sched-io-partial test
34. QEMU shell test
```

各見出しは`[現在/総数]`、各段階の結果は経過時間を表示します。
全段階に成功すると`summary: PASSED all 34 phases`を表示します。
失敗時には、停止した段階の番号、成功数、失敗数、全体の経過時間を表示します。

### 関係するソースファイル

- `xtask/src/cli.rs`：公開コマンドとテスト対象の引数構文
- `xtask/src/lib.rs`：段階の順序、最初の失敗で止まる実行管理、結果の要約
- `xtask/src/cargo.rs`：Cargoの子プロセスと、コマンド、終了ステータス、出力の診断
- `xtask/src/qemu.rs`：QEMUの引数、マーカーと対話の検証、制限時間、プロセスの終了と回収、対話記録
- `xtask/src/docs.rs`：ローカルのMarkdownリンクと第1章から第17章までの必須構造
- `kernel/src/main.rs`：テスト用機能ごとのマーカーとシェルの起動
- `.github/workflows/ci.yml`：Linux上で同じ`setup`と`check`を呼ぶCI

## 実行と確認

最初に、ツールチェーン、RISC-Vターゲット、QEMUを診断します。

```console
$ cargo xtask setup
Rust: rustc 1.98.0 (...)
Rust target: riscv64gc-unknown-none-elf
QEMU: 11.1.0
```

この教材はQEMU 11.1.0で動作を確認しています。
`setup`は、Rustが接尾辞のない安定版1.98.0と完全に一致することを要求し、別の安定版と`nightly`または`dev`の接尾辞を拒否します。
QEMUの最低バージョンは8.2.0です。
Ubuntu 24.04が提供する保守対象のQEMU 8.2系と、長期的に互換性がある`virt`、UART、OpenSBI、プロセス制御用のインターフェースをCIの互換境界にしています。

次に、全検査の入口を実行します。
QEMUのバージョンと各段階の秒数は環境によって変わります。

```console
$ cargo xtask check
[1/34] cargo fmt --all -- --check
phase 1/34 passed (elapsed: ...s)
...
[34/34] QEMU shell test
phase 34/34 passed (elapsed: ...s)
summary: PASSED all 34 phases (elapsed: ...s)
```

この実行例の段階数は、`xtask`が組み立てた検査計画と一致するか文書検査で確認します。
検査段階を追加または削除したときは、教材の古い実行例を残したままにできません。

一つの経路だけを繰り返す場合は、たとえば`cargo xtask test trap`を使います。
最終確認では`cargo xtask check`へ戻り、ホストとゲストの全経路を検査してください。

### Linux CIとローカル検証の対応

GitHub Actionsは`ubuntu-24.04`へ`qemu-system-misc`を導入し、Rust 1.98.0、RV64GCとRV32IMのベアメタルターゲット、rustfmt、Clippyを固定します。
CIはpull requestの作成・更新時と、`main`へのpush時に実行します。
リポジトリ内のPRブランチへpushしてもpull request側の実行だけが始まるため、同じコミットを二重に検査しません。
キャッシュするのはCargoのレジストリーとGitデータ、ワークスペースの`target`だけです。
その後に実行するプロジェクト固有のコマンドは、ローカルと同じ`cargo xtask setup`と`cargo xtask check`だけです。
CI専用の検証スクリプトを持たないため、開発者が手元で通した入口とCIの判定がずれにくくなります。

## よくある失敗

- `missing Rust target`：`rustup target add riscv64gc-unknown-none-elf --toolchain 1.98.0`を実行し、`cargo xtask setup`を再実行します。
- `qemu-system-riscv64`がない：macOSでは`brew install qemu`、Ubuntuでは`sudo apt-get install qemu-system-misc`を使います。
- Clippyで停止する：結果の要約直前にある段階のコマンドを単独で実行し、最初の警告を修正します。
  `#[allow]`でまとめて抑制せず、原因を取り除きます。
- QEMUが時間切れになる：保存されたUART記録の最後の初期化行を探します。
  QEMUが残っていないことも確かめ、同じ`cargo xtask test <filter>`で再現します。
- マーカーがない：終了ステータス0だけを見ず、期待するマーカー、実際の記録、対応するカーネル機能を照合します。
- シェルの出力が足りない：`minios> <command>`というエコーと各応答の順を追い、毎回プロンプトへ戻っているか確認します。

## 演習

`cargo xtask test memory`を実行し、段階の見出し、メモリーマーカー、経過時間、成功の要約を探してください。
次に、`xtask/src/qemu.rs`のメモリーマーカーを一時的に一文字だけ変えます。
終了ステータスが0でも、検証部が記録付きで失敗することを確認してください。
確認後は変更を戻し、`cargo xtask check`で全経路を再検証します。

## 次の章

[第10章「UART対話シェル」](10-shell.md)へ戻ると、対話検証部が観測するコマンド処理を読み直せます。
次は[第12章「次に作るもの」](12-next-steps.md)で、現在のU-mode実行をschedulerや複数processへ広げる場合の境界を確認します。
