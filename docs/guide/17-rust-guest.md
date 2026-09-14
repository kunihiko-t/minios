# 17. Rustでユーザープログラムを書く

## 学習目標

`no_std`と`no_main`のRustプログラムが、静的なRV64 ELFとしてbuildされる流れを説明できるようになります。
guest専用linker scriptの配置契約と、`_start`から`argc/argv`を受け取る起動規約を追います。
`write`と`exit`の呼び出し規約、panic時の終了経路を確認します。
`read`によるstdin転送と、EOFまでのechoを繰り返す第二のguestを確認します。
`cargo xtask bundle`によるMiniBundle生成と、QEMUでのend-to-end実行を確認します。

## 背景

第16章までのpayload testは、手書きのRISC-V命令列をELFへ埋め込んで実行しました。
argvをstdoutへ出すだけの検査にも分岐offsetの手計算が必要で、引数の個数や長さを変えるたびに命令を作り直さなければなりません。
旧argv fixtureは各引数を5 byte固定で読む作りであり、長さの異なる引数には対応できませんでした。

Rustでguestを書くと、引数文字列の長さを測り、`argv`をpointer列としてたどり、panicを終了codeへ畳む処理を型付きのまま記述できます。
buildはCargoの再現可能な手順になり、host testはkernelと同じELF parserで配置契約を検査できます。
QEMUでの実行結果は、手書きfixtureと同じcontrol frame列として検証できます。

この章のguestはRISC-V RV64GCを対象にし、QEMU `virt`上のS-mode kernelがU-modeで実行します。
NEORV32向けのRV32IM実機経路はM-modeで動作するkernel shell専用であり、RV64 guestの実行基盤（S-mode、U-mode、Sv39）を持たないため、この章の対象外です。
実機でアプリケーションを動かす方式の検討は、教材の範囲外の調査事項として残しています。

## 実装

guest本体は[`guest/src/main.rs`](../../guest/src/main.rs)にあります。
`#![no_std]`と`#![no_main]`により、Rust runtimeと`main`の起動時処理を使わず、`_start`を唯一の入口にします。

[`guest/linker.ld`](../../guest/linker.ld)はloaderとの配置契約を作ります。
`ENTRY(_start)`と`KEEP(*(.text.entry))`で入口を先頭に固定し、`.text`と`.rodata`を`0x0010_0000`（`USER_START`）からのR+X segmentへ置きます。
`.data`と`.bss`は次pageからのR+W segmentへ置き、`W+X`のsegmentは作りません。
現在のguestは`.data`と`.bss`が空のため、実際にmapされるのはR+Xの`PT_LOAD`だけです。
program headerの2個目は`PT_RISCV_ATTRIBUTES`であり、loaderは`PT_LOAD`以外を読み飛ばします。
[`guest/build.rs`](../../guest/build.rs)はこのscriptを呼び出しcwdに依存しない絶対pathでlinkerへ渡します。

[`_start`](../../guest/src/main.rs)は`.text.entry`へ置いたnaked関数です。
初期`sp`はkernelが16 byte整列済みで用意し、`a0=argc`と`a1=argv`は第一・第二引数としてそのまま[`guest_main`](../../guest/src/main.rs)へ流れるため、register操作は不要です。
`guest_main`は`argv[0]`から`argv[argc - 1]`までのNUL終端文字列を順に`write`し、終了code42で`exit`します。
`argv[0]`はmanifestの`name`、`argv[1]`以降は`arg=`行の順序どおりの文字列です。

system callは[`minios_abi::syscall`](../../abi/src/syscall.rs)の番号に従います。
`sys_write`は`a0=fd`（引数兼戻り値）、`a1=pointer`、`a2=len`、`a7=1`で`ecall`し、書いたbyte数か負のerrnoを受けます。
`sys_exit`は`a0=code`、`a7=2`で`ecall`し、kernelがguestへ戻らない契約のため`noreturn`です。
panic handlerと`write`失敗時は終了code70で`exit`し、沈黙した停止や未定義の継続を作りません。

第二のguestは[`guest/src/bin/stdin_cat.rs`](../../guest/src/bin/stdin_cat.rs)にあります。
`sys_read`は`a0=0`（`STDIN`）、`a1=pointer`、`a2=len`、`a7=3`で`ecall`し、読んだbyte数、EOFの0、負のerrnoを受けます。
`guest_main`はstack上の512 byte bufferへ`read`し、読んだ分だけ`write`するloopをEOFまで繰り返してから終了code42で`exit`します。
frame境界と要求長は一致しなくてよく、kernelがframe内の残りを次の`read`へ繰り越します。
`read`失敗時も終了code70で`exit`します。

[`xtask/src/guest.rs`](../../xtask/src/guest.rs)の`build_guest`は、`riscv64gc-unknown-none-elf`向けにrelease buildします。
host testはbuild済みELFをkernelの[`ElfImage`](../../kernel/src/elf/header.rs)と[`LoadPlan`](../../kernel/src/elf/plan.rs)で検査します。
独自parserを挟まないため、実行時の受理条件と検査条件が一致します。

[`xtask/src/bundle.rs`](../../xtask/src/bundle.rs)は`cargo xtask bundle`の本体です。
`render_manifest`は`version=1`、`name=`、`arg=`行を組み立て、[`Manifest::parse`](../../abi/src/manifest.rs)で文字種、件数、長さ、全体4 KiBを検査します。
`build_bundle`は96 byte header、manifest、8 byte境界までのゼロpadding、ELFを順に並べ、`digest` fieldだけをゼロにしたheaderと残り全部のSHA-256を格納します。
入力だけから決まる固定手順のため、同じ入力は同じbyte列になります。

QEMUでの実行は第16章のpayload経路をそのまま使います。
[`payload_args_bundle_bytes`](../../xtask/src/qemu.rs)はbuild済みRust guestと`name=hello`、`arg=alpha`、`arg=bravo`のmanifestからMiniBundleを作り、QEMU loaderへ渡します。
kernel側の変更はなく、[`run_boot_payload`](../../kernel/src/main.rs)がargv配置、U-mode実行、frame回収、diagnostic出力を行います。

## 実行と確認

guest binaryは次のコマンドでbuildします。
成果物は`target/riscv64gc-unknown-none-elf/release/minios-guest`です。

```sh
cargo build -p minios-guest --target riscv64gc-unknown-none-elf --release --locked
```

MiniBundleはbuildと生成を一つの開発コマンドで実行します。

```console
$ cargo xtask bundle --name hello --arg alpha --arg bravo
wrote .../target/minios-guest.mcb (5736 bytes, name=hello, args=2, sha256=7cccfed0359b13a27f34465595f12703807839fff57328cb76991404a9fc4521)
```

byte数とdigestはtoolchainの出力に依存して変わりますが、`name`、`args`、ELF配置は固定です。
`--name`と`--arg`を省略した場合は`name=minios-guest`、引数なしのbundleになります。

QEMUでのend-to-end実行は次のコマンドです。

```console
$ cargo xtask test payload-args
...
MiniOS payload: ok code=42
...
summary: PASSED all 1 phases (elapsed: ...)
```

host harnessはReady、stdout（`hello`、`alpha`、`bravo`の順）、Exit（code 42）、回収diagnosticの完全なframe列を検証します。
`hello`はmanifestの`name`、`alpha`と`bravo`は`arg=`行であり、guestが`argc/argv`から読んだ順序どおりです。
timeout時はQEMU childをkillしてwaitし、受信済み出力を診断へ残します。

stdin転送のend-to-end実行は次のコマンドです。

```console
$ cargo xtask test payload-stdin
...
MiniOS payload: ok code=42
...
summary: PASSED all 1 phases (elapsed: ...)
```

host harnessはReadyを待ってから`ab`、`cdef`、EOFの`STDIN` frame列を送り、Ready、stdout（`ab`、`cdef`の順）、Exit（code 42）、回収diagnosticの完全なframe列を検証します。
cat guestの要求512 byteに対し、kernelは届いたframe分だけを返し、残りを次の`read`へ繰り越します。

host側の配置検査は次のコマンドで実行します。

```sh
cargo test -p xtask --locked bundle
```

## よくある失敗

- `fn main`を残す：`#![no_main]`のguestは`_start`が唯一の入口であり、`main`を使うとstdの起動時処理を要求します。
- linker scriptを渡さない：entryが`USER_START`に来ず、R+X配置にもならないため、kernel loaderが受理しません。
- `.text.entry`の`KEEP`を外す：最適化で`_start`が消えるか先頭に来なくなり、entry契約が壊れます。
- 初期stackの上位を書き換える：argv文字列は`sp`より上位の読み取り専用初期データであり、以降のstack使用は`sp`より下位へ行います。
- manifestの`name`に空白を入れる：文字種違反のため、`InvalidName`を報告してbundle生成が失敗します。
- guestにstderr出力を期待する：guestはstdoutだけを書き、stderr frame経路はMK6 payload testが担います。
- EOFを送らずに`read`の完了を待つ：`read`はbyteかEOFが届くまで待機するため、入力の末尾には長さ0の`STDIN` frameを送ります。
- NEORV32でguestを実行しようとする：実機経路はRV32IMのM-mode kernel shell専用であり、RV64 guestはQEMU `virt`でのみ実行します。
- bundleのbyte数やdigestを固定値として記録する：toolchainで変わるため、検証はABI decoderとframe順序で行います。

## 演習

[`guest/src/main.rs`](../../guest/src/main.rs)の正常終了code（42）を別の値へ変え、`cargo xtask test payload-args`がExit frameの不一致で失敗することを確認してください。
期待frameは[`PAYLOAD_EXIT_FRAME`](../../xtask/src/qemu.rs)の4 byteです。
確認後はcodeを42へ戻し、testが再び通ることを確かめます。

次に、次の二つの失敗を起こし、省略されない診断を読む練習をします。

```sh
cargo xtask bundle --name 'bad name'
cargo xtask bundle --arg a --arg b --arg c --arg d --arg e --arg f --arg g --arg h --arg i --arg j --arg k --arg l --arg m --arg n --arg o --arg p --arg q
```

一つ目は`InvalidName`、二つ目は17個目の引数に対する`TooManyArgs`を報告します。
診断には入力の`name`と引数件数が含まれることを確認します。

## 次の章

このガイドの実装順はここで終わります。
全章の索引は[学習ガイド](README.md)へ戻ります。
実行基盤の詳細は[第15章「U-modeでELFを実行する」](15-user-mode.md)と[第16章「boot payloadを実行する」](16-boot-payload.md)を、ABIの規約は[MiniContainer Guest ABI](../reference/minicontainer-abi.md)を参照してください。
