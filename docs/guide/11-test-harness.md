# 11. テストハーネスの仕組み

## 学習目標

この章では、`cargo xtask` をローカル開発と CI の共通入口にする理由を学びます。読み終えると、
ホスト単体テストと RISC-V ゲスト統合テストの役割の違い、QEMU の marker mode と interactive
mode、timeout 時のプロセス回収と transcript の読み方、`cargo xtask check` の正確な実行順を
説明できるようになります。

## 背景

カーネル本体は `riscv64gc-unknown-none-elf` 向けの `no_std` バイナリです。一方、xtask は
開発機の OS 上で動く通常の Rust プログラムで、Cargo と QEMU を子プロセスとして起動します。
「ホスト」は xtask を実行する macOS または Linux、「ゲスト」は QEMU の RISC-V 仮想機内で
動く MiniOS を指します。プロセスの終了 status、標準入力、標準出力、標準エラー、deadline
（制限時間）がこの章で使う主な OS 概念です。

## 実装

### なぜ xtask パターンを使うのか

Cargo は `cargo xtask ...` を workspace 内の `xtask` binary へ渡せます。この小さな host tool に
環境診断、cross build、QEMU 起動、検証順序を集約すると、追加の task runner や OS ごとの shell
script が不要になります。公開 command は次の5系統です。

```text
cargo xtask setup
cargo xtask build
cargo xtask run
cargo xtask test [all|boot|trap|timer|memory|shell]
cargo xtask check
```

xtask 内から `cargo xtask` を再帰起動してはいけません。`check` は format、Clippy、build、host
test のように Cargo が所有する操作だけを Cargo subprocess にし、QEMU test は xtask 内部の
Rust 関数を直接呼びます。この境界により順序と失敗処理が一つの runner に集まり、再帰による
終わらない検証を防げます。

### host test と guest test を分ける理由

hardware に依存しない処理は host 上で速く、細かく検査します。対象は shell parser と固定長
line buffer、tick から時間への変換、物理 frame allocator、xtask 自身の parser や process
処理です。失敗箇所を通常の Rust test 名で絞れるため、logic bug の調査に QEMU 起動は不要です。

guest test は、cross build だけでは分からない OpenSBI、CSR、trap entry、SBI timer、UART、SBI
reset の結線を実際の RISC-V machine 上で検査します。`cargo xtask test`（引数なし、つまり
`all`）は次の順で実行します。

1. `cargo test -p minios-kernel --lib`
2. `cargo test -p xtask`
3. QEMU boot test
4. QEMU trap test
5. QEMU timer test
6. QEMU memory test
7. QEMU shell test

host test を先に置くことで速い失敗を先に返し、その後は boot から対話 shell へ依存関係の順に
guest 全5経路を確認します。

### QEMU の2つの検証 mode

boot、trap、timer、memory は marker mode です。それぞれ専用 Cargo feature で kernel を build
し、UART transcript と status 0 に加えて次の完全一致 marker を要求します。

```text
[MINIOS_TEST] boot: ok
[MINIOS_TEST] trap: ok
[MINIOS_TEST] timer: ok
[MINIOS_TEST] memory: ok
```

marker がない status 0 は成功ではありません。これにより「QEMU は終了したが、目的の kernel
経路へ到達しなかった」という偽陽性を防ぎます。CRLFをLFへ正規化した後の完全一致lineとして探すため、
markerをprefix/suffixに含むだけの診断行や`ok`のnear-matchは成功扱いしません。

shell は interactive mode です。通常 kernel の最初の `minios> ` を待ち、`help`、`info`、
`uptime`、`memory`、`not-a-command`、`shutdown` を標準入力へ送ります。verifier は各 command
の echo と毎回の新しい prompt、安定した応答、`hart id: 0`、数値形式のuptime/ticksとmemory統計、
最後のstatus 0をすべて要求します。最初のprompt以降を一つのordered transcriptとして読み、helpの
全6 responseも含めて各lineを位置付きで検証します。したがってresponseの並べ替え、prompt反復、
`minios> helper`のようなprefix near-match、途中のunexpected lineは通りません。末尾の空行だけは
許可します。この一連のtranscriptでUART送受信、parser、timer、allocator、SBI resetを同じsession内で
検査できます。

### timeout、cleanup、失敗 transcript

各 QEMU test の deadline は5秒です。xtask は stdout と stderr を起動直後から別 thread で読み、
pipe buffer が満杯になって guest が停止することを防ぎます。deadline を越えた場合は QEMU を
kill して必ず `wait` し、子プロセスを残しません。エラーには shell-safe に引用した実際の QEMU
program、kernel path、全 flag、設定された deadline、受信済み stdout と stderr、cleanup 自体が
失敗した場合はその診断も残ります。QEMU 起動前の build failure では Cargo command に test 用
feature が残るため、どの guest image を準備していたかも追跡できます。

marker 不足、shell 出力不足、非0 status の場合も transcript を省略しません。最後に見えた初期化
段階や marker を、失敗した phase header と併せて読むと、build failure、guest 内の明示的失敗、
hang を区別できます。Cargo subprocess の失敗も、実行 command、終了 status、stdout/stderr を
表示します。

### `check` の正確な phase 順序

`cargo xtask check` は次の14 phase をこの順で実行し、最初の失敗で停止します。formatの直後に
教材navigationと章構造を検査してからcompilerへ進みます。順序を変えると、
速い静的検査より先に QEMU を起動したり、検査していない binary を guest test に渡したりするため、
この並び自体が harness の契約です。

```text
1. cargo fmt --all -- --check
2. check local Markdown links
3. check guide chapter structure
4. cargo clippy -p xtask --all-targets -- -D warnings
5. cargo clippy -p minios-kernel --lib -- -D warnings
6. cargo clippy -p minios-kernel --bin minios-kernel --target riscv64gc-unknown-none-elf -- -D warnings
7. cargo build -p minios-kernel --bin minios-kernel --target riscv64gc-unknown-none-elf
8. cargo test -p minios-kernel --lib
9. cargo test -p xtask
10. QEMU boot test
11. QEMU trap test
12. QEMU timer test
13. QEMU memory test
14. QEMU shell test
```

各 header は `[現在/総数]`、各 phase 結果は elapsed time を表示します。最後は全成功なら
`summary: PASSED all 14 phases`、失敗なら停止した phase 番号、成功数、失敗数、全 elapsed time
を表示します。

### 関係するソースファイル

- `xtask/src/cli.rs`: public command と test filter の構文
- `xtask/src/lib.rs`: phase 順序、first-failure runner、summary
- `xtask/src/cargo.rs`: Cargo subprocess と command/status/transcript 診断
- `xtask/src/qemu.rs`: QEMU 引数、marker/interactive verifier、timeout cleanup
- `xtask/src/docs.rs`: local Markdown linkとChapter 1–12の必須構造
- `kernel/src/main.rs`: test feature ごとの marker と shell 起動
- `.github/workflows/ci.yml`: Linux 上で同じ setup/check を呼ぶ CI

## 実行と確認

まず toolchain、RISC-V target、QEMU を診断します。

```console
$ cargo xtask setup
Rust: rustc 1.98.0 (...)
Rust target: riscv64gc-unknown-none-elf
QEMU: 11.1.0
```

この章の手順は QEMU 11.1.0 で実際に検証しています。Rustはsuffixなしの1.98.0 stableとの完全一致を
setupで要求し、別stableやnightly/dev suffixを拒否します。QEMUでharnessが受け入れる最低versionは
8.2.0です。Ubuntu 24.04 が提供する maintained QEMU 8.2 series と、長期互換のある `virt`、
UART、OpenSBI、process-control interface を CI の互換境界にします。

次に統一入口を実行します。QEMU の version や phase の秒数は環境により変わります。

```console
$ cargo xtask check
[1/14] cargo fmt --all -- --check
phase 1/14 passed (elapsed: ...s)
...
[14/14] QEMU shell test
phase 14/14 passed (elapsed: ...s)
summary: PASSED all 14 phases (elapsed: ...s)
```

一経路だけ反復するときは、たとえば `cargo xtask test trap` を使います。最終確認では必ず
`cargo xtask check` へ戻り、host と guest 全経路を省略しないでください。

### Linux CI とローカル検証の同一性

GitHub Actions は `ubuntu-latest` に `qemu-system-misc` を導入し、Rust 1.98.0、
`riscv64gc-unknown-none-elf` target、rustfmt、Clippy を固定して用意します。cache 対象は Cargo の
registry/git data と workspace の `target` だけです。その後に実行する project command は
ローカルと同じ `cargo xtask setup` と `cargo xtask check` の2つだけです。CI 専用の検証 script
を持たないため、開発者が手元で通した入口と CI の判定がずれません。

## よくある失敗

- `missing Rust target`: `rustup target add riscv64gc-unknown-none-elf --toolchain 1.98.0` を実行し、
  `cargo xtask setup` を再実行します。
- `qemu-system-riscv64` がない: macOS は `brew install qemu`、Ubuntu は
  `sudo apt-get install qemu-system-misc` を使います。
- Clippy で停止する: summary の直前にある phase command を単独実行し、最初の warning を修正
  します。`#[allow]` で一括抑制せず原因を解消します。
- QEMU timeout: 保存された UART transcript の最後の初期化行を探します。timeout 後に QEMU が
  残っていないことも確認し、同じ `cargo xtask test <filter>` で再現します。
- marker 不足: status 0 だけを見ず、期待 marker と実際の transcript、対応する kernel feature
  を照合します。
- shell 出力不足: `minios> <command>` の echo と各 response の順を transcript で追い、prompt
  が毎回戻っているか確認します。

## 演習

`cargo xtask test memory` を実行し、phase header、memory marker、elapsed time、成功 summary の
4点を探してください。次に `xtask/src/qemu.rs` の memory marker 文字列を一時的に1文字だけ変え、
status 0 でも verifier が transcript 付きで失敗することを確認します。確認後は変更を戻し、
`cargo xtask check` で全経路を再検証してください。

## 次の章

[第10章: 対話シェル](10-shell.md)へ戻ると、interactive verifierが観測するcommand処理を
読み直せます。次は[第12章: 次に作るもの](12-next-steps.md)で、この安全網を維持したまま
Device Tree、heap、仮想memory、user modeへ学習範囲を広げる順序を考えます。
