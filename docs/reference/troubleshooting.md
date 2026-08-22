# troubleshooting

まず症状に最も近い項目を選び、diagnostic commandをそのままrepository rootで実行します。複数の
原因を同時に直さず、最初の失敗を一つ解消して同じcommandを再実行してください。

## RISC-V targetがない

- **症状:** `can't find crate for core`または`Rust target riscv64gc-unknown-none-elf is not installed`。
- **diagnostic command:** `rustup target list --installed --toolchain 1.98.0`
- **likely cause:** 固定toolchainにbare-metal RISC-V target componentが導入されていません。
- **corrective action:** `rustup target add riscv64gc-unknown-none-elf --toolchain 1.98.0`を実行し、
  `cargo xtask setup`を再実行します。

## QEMUがない、または古い

- **症状:** `qemu-system-riscv64 is not installed`、command not found、またはversion floor error。
- **diagnostic command:** `qemu-system-riscv64 --version`
- **likely cause:** QEMU未導入、PATH不一致、または8.2.0未満です。
- **corrective action:** macOSは`brew install qemu`、Ubuntu/Debianは
  `sudo apt-get install qemu-system-misc`を使い、`cargo xtask setup`で検出結果を確認します。

## linker sectionが重なる

- **症状:** cross buildのlinkerがoverlap、region、relocation errorで停止します。
- **diagnostic command:** `cargo build -p minios-kernel --bin minios-kernel --target riscv64gc-unknown-none-elf`
- **likely cause:** `0x8020_0000` start、4 KiB alignment、small data/BSS回収、boot stack、`__kernel_end`の
  いずれかを壊しています。
- **corrective action:** [`linker.ld`](../../kernel/linker.ld)を[memory map](memory-map.md)と照合し、
  `cargo test -p xtask cargo::tests::linker_places_small_data_and_bss_probes_inside_boundaries`も実行します。

## UART出力がない

- **症状:** OpenSBIまでは見えるが`[ok] traps`や`MiniOS booting...`が一行も出ません。
- **diagnostic command:** `cargo xtask test boot`
- **likely cause:** kernel entryへ到達していない、stack/BSS初期化が壊れた、UART base `0x1000_0000`または
  Line Status bitを誤っています。
- **corrective action:** transcriptの`Domain0 Next Address`が`0x8020_0000`か確認し、`entry.S`の`sp`と
  BSS loop、UART transmit-ready bit 5、volatile writeの順に調べます。

## 起動直後にtrapする

- **症状:** prompt前に`unexpected trap`と`scause/sepc/stval`が出る、または同じtrapを繰り返します。
- **diagnostic command:** `cargo xtask test trap`
- **likely cause:** `stvec` alignment、trap frame offset、save/restore非対称、SIEを準備前に有効化した可能性が
  あります。
- **corrective action:** `scause`のinterrupt bit/cause codeをdecodeし、`sepc`をsymbol位置へ対応付けます。
  `trap.S`の256-byte frameとx1/x3..x31の対称性、trap→timer→SIEの初期化順を直します。

## timer testがtimeoutする

- **症状:** `cargo xtask test timer`が5秒でtimeoutしtimer markerがありません。
- **diagnostic command:** `cargo xtask test timer`
- **likely cause:** SBI TIME extension/function、10 MHzから100,000 cycleへの換算、STIE/SIE、cause 5 dispatch、
  または再予約が壊れています。
- **corrective action:** `time` CSRから`now + 100_000`というabsolute deadlineを渡すこと、STIE bit 5の後に
  global SIE bit 1を立てること、handlerがSBI errorを隠さないことを確認します。

## shell入力がoverflowする

- **症状:** 129 byte以上の行で`error: input exceeds 128 bytes`が表示されます。Backspace後も実行されません。
- **diagnostic command:** `cargo test -p minios-kernel --lib shell::line::tests`
- **likely cause:** 通常は設計どおりのbounded inputです。短い入力でも起きるならcapacity countかCR/LF処理の
  regressionです。
- **corrective action:** 長い行を短くして再入力します。実装修正時はoverflow後のBackspaceでinvalid stateを
  解除せず、次promptの`clear()`だけで戻るfocused testを維持します。

## QEMU processが残る

- **症状:** test終了後も`qemu-system-riscv64`が実行中、terminalが戻らない、または次testが干渉します。
- **diagnostic command:** `pgrep -fl qemu-system-riscv64`
- **likely cause:** 手動sessionで`shutdown`していないか、timeout pathがkill後にwaitできていません。
- **corrective action:** 対話sessionでは`shutdown`を使います。自動testではerrorに出たcommand/deadline/cleanup
  診断を保存し、`cargo test -p xtask qemu::tests::timeout_reaps_process_and_preserves_both_streams`を実行します。
  所有していないQEMU processを一括killしないでください。

## CIだけ失敗する

- **症状:** local `cargo xtask check`は通るがUbuntu GitHub Actionsだけ失敗します。
- **diagnostic command:** `cargo xtask setup && cargo xtask check`
- **likely cause:** case-sensitive path、format差分、Ubuntu package版QEMU 8.2のversion suffix、cacheではなく未commit
  fileへの依存、またはlocalとCIのtoolchain差です。
- **corrective action:** CI logの最初のfailed phaseを同じexact commandで再現します。tracked filesを
  `git status --short`で確認し、Rust 1.98.0とQEMU floorをsetup出力で照合します。CI専用skipを追加せず、
  common xtask境界のportable bugを修正します。

[README](../../README.md) | [architecture](architecture.md) | [学習ガイド](../guide/README.md)
