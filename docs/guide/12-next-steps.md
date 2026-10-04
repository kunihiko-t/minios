# 12. 次に作るもの

## 学習目標

OSの実装順を、すべての将来機能に共通する一本道ではなく、受け入れ条件に必要な依存関係として説明できるようになります。
Device Treeと汎用ヒープをSv39より先に置いた旧順序と、固定容量の単一アドレス空間を先行させた実際の順序を比較します。
第13章以降で追う機能と、その後の計画を記した[発展ロードマップ](../reference/roadmap.md)の位置づけを確認します。

## 背景

旧ロードマップは、Device Treeで利用可能なRAMを発見し、汎用ヒープからページテーブルの管理情報を確保した後にSv39へ進む順序でした。
この順序は複数のmachineと可変個のアドレス空間へ広げやすい一方、ハードウェア発見、動的確保、仮想メモリーの失敗を同じ段階へ持ち込みます。

MiniContainerの最初の実行単位に必要なのは、QEMU `virt`上で一つのユーザーイメージを配置できるアドレス空間でした。
そこでSv39とELF loaderの段階では、固定された128 MiB RAMとUARTの配置を維持し、所有フレームを静的な表へ記録しました。
一つのactiveなカーネル空間と一つのinactiveなユーザー空間に対象を絞ったため、Device Treeと汎用ヒープを前提にせずSv39とELF loaderを先に検証できました。

この順序変更は「Device Treeとヒープが不要になった」ことを意味しませんでした。
U-mode実行が動いた後にDevice Treeとヒープを導入し、所有フレームの静的な表はヒープ上の可変長な台帳へ置き換わりました。
その上に複数processのscheduler、virtio-blkとFAT32、file descriptor、processを生成するsystem callが積み上がっています。

## 実装

第13章から第23章は、次の順序で機能を追います。
各章の前提は直前までの章であり、順序そのものが依存関係を表しています。

1. **アドレス空間と実行**：[第13章](13-sv39.md)のSv39、[第14章](14-elf-loading.md)のELF配置、[第15章](15-user-mode.md)のU-mode実行、[第16章](16-boot-payload.md)のboot payload、[第17章](17-rust-guest.md)のRust guest。
2. **動的な資源管理**：[第18章](18-heap-and-fdt.md)のDevice Treeとヒープの成長、[第19章](19-scheduler.md)のプリエンプティブscheduler。
3. **永続データとprocess間の連携**：[第20章](20-virtio-blk.md)のvirtio-blk、[第21章](21-fat32.md)のFAT32、[第22章](22-file-descriptors-and-pipes.md)のfile descriptorとpipe、[第23章](23-process-syscalls.md)の`spawn`、`waitpid`、`exec`。

NEORV32向けには、RV32IMのM-mode起動、UART0、SD/FAT32の読み出し、対話シェルまでを実装しています。
この実機経路はQEMU側のSv39やU-modeを前提にせず、共通のコンソール、入力処理、FAT32 parserを別のハードウェアへ接続します。

第23章より先の計画は[発展ロードマップ](../reference/roadmap.md)にあります。
ロードマップは作業を段階に分け、各段階の受け入れ条件を`cargo xtask test`の経路として書いています。

## 実行と確認

全検査にはrelease gateを実行します。

```sh
cargo xtask check
```

`vm`と`elf`の経路は、後続の章で機能が増えた後も残しています。
activeなカーネル空間と実行前`LoadedImage`の前提が、ヒープやschedulerの導入で壊れていないことを毎回確認するためです。

## よくある失敗

- 旧順序を必須条件として読む：Sv39とELF loaderは汎用ヒープを使わずに動き始め、ヒープは後から所有フレームの台帳を引き取りました。
  Device Treeはmachine記述を発見するだけであり、動的確保の前提ではありません。
- `LoadedImage`を常に実行中と記述する：ELF loaderが返した直後はinactiveであり、kernel mappingとtrap stackを加えた後だけ`sret`します。
- U-mode実行をLinux互換と記述する：system callはMiniOS独自のABIであり、番号も引数の規約もLinuxとは異なります。
- 固定容量を暗黙の無制限構造として扱う：`PT_LOAD`の個数、user imageのページ数、process数、open file数には拒否境界があります。

## 演習

U-mode遷移、`write`、`exit`の三項目について、直接の前提、hostで検査できる純粋ロジック、QEMUでしか観測できない状態、失敗時に回収する所有物を四列の表へ整理してください。
次に、汎用ヒープを先行させた場合に各列がどう増えるかを書き足してください。
追加した依存が最初の`write`と`exit`の受け入れ条件に必要かを調べ、不要なら後続の章へ戻します。

## 次の章

[第13章「Sv39と単一アドレス空間」](13-sv39.md)では、先行実装したactiveなカーネル空間のpage walkと権限を追います。
[全体構成](../reference/architecture.md)、[メモリーマップ](../reference/memory-map.md)、[発展ロードマップ](../reference/roadmap.md)も実装順の根拠として参照してください。
