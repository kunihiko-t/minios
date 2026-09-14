# NEORV32でアプリケーションを動かす方式の検討

調査Issue（NEORV32でRustアプリケーションを実行する方式を決める）の記録です。
M3のRust guestはRV64とQEMU `virt`専用であり、この文書はFPGA実機経路の判断材料と、将来分離U-mode案を採る場合の前提条件を残します。
実装は含みません。

## 結論

- 採用：案C — FPGA版はkernel shellまでとし、U-mode guestは対象外にします。
- 対象外：案A（M-mode単一address spaceへの直接リンク）、案B（分離したU-mode guest）。

採用理由は次の五つです。

1. IMEM残量が2,184 byteしかなく、案Bの新規コード（trap入口、context、dispatch、PMP設定、argv配置、配置器）が収まらない見込みです。
2. 実機へguestを届ける配送路（QEMU `-device loader`相当）がなく、UART受信か静的埋め込みの新設が必要です。
3. FPGA実行の自動試験路がなく、trapとPMPの正否をCIで検証できません。
4. M3はNEORV32でのguest実行を対象外と定めています。
5. U-mode、system call、分離の教育目標はQEMU RV64で自動試験付きに達成済みです。

案Aは分離を教えられず、IMEM拡張なしでは数百byte級しか載りません。
案Bの再訪条件は、IMEMの32 KiB化（FPGA再合成）、guest配送路、simまたは実機の試験路が揃うことです。

## 前提と制約（実測）

RV32IMカーネル（release）の実測値です。

| 領域 | 全体 | 使用 | 残量 |
| --- | ---: | ---: | ---: |
| IMEM | 24,288 byte | `.text` 22,100 byte＋`.data`初期値4 byte | 2,184 byte |
| DMEM | 16,192 byte | `.data` 4 byte、`.bss` 0 byte、boot stack 4,096 byte | 約12,092 byte |

参考として、RV64 guestの`.text`は140 byteです。file全体の5,592 byteの大半はsymbolであり、load対象ではありません。

このリポジトリにNEORV32のVHDL構成は含まれません。
U-modeとPMPの有無はFPGA構成側のtopを確認することが初手であり、NEORV32の既定値はU-mode無効、PMP 0リージョンです。
NEORV32にS-modeとMMUはなく、保護機構はPMPだけです。
M-mode trap（`mtvec`、`mepc`、`mcause`、`mret`）は常時利用できます。

## 調査結果

### U-modeと保護機構の利用可否

U拡張は`CPU_EXTENSION_RISCV_U`（既定`false`）で有効化するoptional機能であり、CSR権限の分離をもたらします。
PMPは`PMP_NUM_REGIONS`（既定`0`、0から16）と`PMP_MIN_GRANULARITY`（既定4 byte）で有効化するoptional機能です。
U-mode単体ではメモリー分離がなく、PMP併用でリージョンによる分離が可能です。
PMPのmode（TORとNAPOT）は版により対応が異なるため、利用版の資料で確認します。
いずれの有効化もFPGAの再合成が必要です。

### RV32 guest ABIを追加する場合の共有可否

| 部位 | 共有可否 | 理由 |
| --- | --- | --- |
| 初期stack | layout手順のみ | 8 byte語長、Sv39 `AddressSpace`、U/W検査に依存し、RV32用に4 byte語長とflat書き込みの別実装が必要 |
| syscall dispatch | 番号と検査のみ | 番号、fd、長さ検査は中立で再利用可。`copy_from_user`はSv39 page walk依存のためflat range検査のbackendが必要 |
| trap | 共有なし | `user.S`、`trap.S`、`trap.rs`、`csr.rs`はS-mode、`sepc`、`sret`、8 byte context専用。M-mode用に新規が必要 |
| ELF loader | 共有なし | `ELFCLASS64`専用かつSv39配置専用。RV32はELF32とMMUなし直接配置のため別実装か、raw配置でloader省略の選択 |
| ABI定数とframe形式 | そのまま共有 | `minios-abi`は`no_std`無依存でRV32ビルド済み |
| guest本体 | 作り直し | RV32IM向けbuildと4 byte argv読みが必要 |

### IMEMとDMEMへの収容可否

案Bの新規コードはRV64対応部の規模から数KBの見積もりであり、現IMEM残量2,184 byteに収まらない見込みです。
確定には試作計測が必要です。
DMEMは数KB級のguestとstackであれば約12,092 byteの残量に収まります。
IMEMが拘束条件であり、拡張（再合成）かkernel縮小（`opt-level="z"`、LTO、機能削減）が前提になります。

### 案Aと案Bの差

| 観点 | 案A：M-mode直接リンク | 案B：分離U-mode guest |
| --- | --- | --- |
| 分離 | なし（全M-mode） | PMPリージョンによる分離 |
| loaderとtrap | 不要（関数呼び出し） | M-mode trap、dispatch、配置器が新規 |
| FPGA変更 | 不要 | U有効、PMP有効、IMEM拡張の再合成 |
| guest配送 | FPGA image再build | UART受信か静的埋め込みの新設 |
| 試験 | 実機shell確認の延長 | 実機UART手動のみ。CI自動化なし |
| 教材価値 | 結合とlink配置 | 特権分離とPMP。QEMU章と重複大 |
| 工数 | 小 | 大（複数PR） |

### 教材としてどこまで実装するか

U-mode、system call、分離はQEMU RV64の自動試験付き教材で教え、FPGA章はbring-up、link配置、UART、shellの現行範囲に留めます。
案Bは将来の発展課題とし、この文書の前提条件が揃った場合に再訪します。

## 案Bに必要なもの

案Bを採る場合の前提条件の列挙であり、実装の指示ではありません。

### NEORV32設定

- `CPU_EXTENSION_RISCV_U = true`
- `PMP_NUM_REGIONS >= 4`（guest RX、guest RW、kernel拒否、周辺拒否の目安。利用版の上限とmodeを確認する）
- `PMP_MIN_GRANULARITY = 4`
- `MEM_INT_IMEM_SIZE >= 32768`（再合成）
- `MEM_INT_DMEM_SIZE`は据え置き可、UART0は据え置き
- 初手は現bitstreamのgenerics確認

### memory map案

現状は[`linker_neorv32.ld`](../../kernel/linker_neorv32.ld)のIMEM 24,288 byteとDMEM 16,192 byteです。
IMEM 32 KiB化を例にした案Bの配置案は次のとおりです。

| 領域 | 用途 | 目安 |
| --- | --- | --- |
| IMEM | kernel `.text`と`.rodata` | 22,100 byte（実測） |
| IMEM | trap入口、dispatch、PMP設定、argv配置、配置器 | 約3,000 byte（試作計測で確定） |
| IMEM | guest配置 | 残り約7,600 byte |
| DMEM | kernel `.data`と`.bss` | 4 byte（実測） |
| DMEM | guest argvとstack | 数KB |
| DMEM | boot stack | 4,096 byte |

### ABI差分（RV32 guest ABI v1案）

[MiniContainer Guest ABI](minicontainer-abi.md)からの差分です。

- 語長は8 byteから4 byteへ。`argc`、`argv`、`AT_NULL`、`envp`の配置は4 byte単位。
- `sp`の16 byte整列は維持する。
- register番号（`a0`、`a1`、`a7`）と`write`、`exit`の番号、上限、errnoは維持する。
- `ecall`はM-mode trap（`mcause=8`）として受ける。
- pointerは32 bit。ELFはELF32かraw binary。
- page権限の代わりにPMPリージョンで分離する。U-bitはない。

### テスト方法

- host unit test：純粋logicは従来どおり自動化する。
- QEMU RV32 `virt`：SoC差異（UARTとmemmap）のため一部logicのみ。
- GHDLまたはVerilator sim：VHDLとEDAが必要。
- FPGA実機UART：現行どおり手動で、CI対象外。
- CI自動化なしが最大の制約であり、trapとPMPの回帰を機械的に防げない。

## 案Bの分割案

案C採用のためIssueは起票しません。再訪時に起票する参考分割です。

1. PMPとM-mode trap基盤（`mcause=8`の`ecall`受付と拒否診断、実機UART確認）
2. RV32 guest ABIと`write`・`exit` dispatch（flat range検査、host testと実機確認）
3. guest配置（UART受信または静的埋め込みとargv配置、実機確認）
4. RV32 guest例と教材（`no_std`のRV32IM guest、学習章、host分のcheck組み込み）

## 参照

- [NEORV32](https://github.com/stnolting/neorv32)と[NEORV32資料](https://stnolting.github.io/neorv32/)（U拡張とPMPのgenerics）
- [`linker_neorv32.ld`](../../kernel/linker_neorv32.ld)と[`entry.S`](../../kernel/src/arch/riscv32/entry.S)（現行のRV32起動契約）
- [`user/stack.rs`](../../kernel/src/user/stack.rs)、[`user/syscall.rs`](../../kernel/src/user/syscall.rs)、[`elf/header.rs`](../../kernel/src/elf/header.rs)（共有可否の判定対象）
- [MiniContainer Guest ABI](minicontainer-abi.md)と[発展ロードマップ](roadmap.md)
