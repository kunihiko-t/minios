# 12. 次に作るもの

## 学習目標

OSの実装順を、すべての将来機能に共通する一本道ではなく、今回の受け入れ条件に必要な依存関係として説明できるようになります。
Device Treeと汎用ヒープをSv39より先に置いた旧順序と、固定容量の単一アドレス空間を先行させた現在の順序を比較します。
現在のinactiveな`LoadedImage`から、U-mode遷移、user trap context、`write`、`exit`へ進む最小の段階も考えます。

## 背景

旧ロードマップは、Device Treeで利用可能なRAMを発見し、汎用ヒープからページテーブルの管理情報を確保した後にSv39へ進む順序でした。
この順序は複数のmachineと可変個のアドレス空間へ広げやすい一方、ハードウェア発見、動的確保、仮想メモリーの失敗を同じ段階へ持ち込みます。

MiniContainerの最初の実行単位に必要なのは、QEMU `virt`上で一つのユーザーイメージを配置できるアドレス空間です。
そこで現在の実装は、固定された128 MiB RAMとUARTの配置を維持し、2,688個までの所有フレームを静的な`AddressSpaceStorage`へ記録しました。
この上限に収まる一つのactiveなカーネル空間と一つのinactiveなユーザー空間に対象を絞ったため、Device Treeと汎用ヒープを前提にせずSv39とELF loaderを先に検証できました。

この順序変更は「Device Treeとヒープが不要になった」ことを意味しません。
固定アドレスと固定容量を複数machine、複数process、動的な実行数へ広げる段階では、ハードウェア記述と失敗可能な動的確保が必要になります。

## 実装

現在までに、物理フレームアロケーター、Sv39のactiveなカーネル空間、静的RISC-V 64 ELFから作るinactiveな`LoadedImage`が完成しています。
次は次の順序で、実行前の所有物を一つずつ実行状態へ接続します。

1. **U-mode遷移とuser trap context**：`LoadedImage`のentryとuser stack上端を初期レジスターへ設定し、カーネルスタックと保存領域を用意して`sret`します。
2. **`write` system call**：U-modeの`ecall`をS-modeへ戻し、user pointerの範囲とPTE権限を検査してからUARTへbyte列を出します。
3. **`exit` system call**：終了コードをhostへ伝える内部状態を定義し、ユーザー空間とカーネルスタックの所有フレームを回収します。
4. **MiniBundle payload統合**：予約済み物理領域からMiniBundle内のELFを取り出し、既存のminiOS loaderへ渡します。
5. **QEMU host runtime**：MiniContainer側でQEMU子プロセス、UART、終了状態、timeout、後始末を一つのlifecycleへ接続します。

Device Treeと汎用ヒープは、QEMU `virt`の固定値を外す段階と、固定容量の単一アドレス空間を越える段階で導入します。
その後にprocessとscheduler、VirtIO、file system、network、multi-hart、実機対応を進めます。
各段階の完了条件は[発展ロードマップ](../reference/roadmap.md)にあります。

## 実行と確認

次の段階へ進む前と実装後に、19段階のrelease gateを実行します。

```sh
cargo xtask check
```

新しいproduction behaviorには、まず失敗するhost testかQEMU経路を追加します。
U-mode遷移にはentryと特権レベルを観測するQEMU marker、`write`には正常なbufferと不正なuser pointer、`exit`には終了コードと全所有フレームの回収が必要です。
既存の`vm`と`elf`経路も残し、activeなカーネル空間とinactiveな`LoadedImage`の前提が壊れていないことを確認します。

## よくある失敗

- 旧順序を現在の必須条件として残す：現在のSv39とELF loaderはDevice Treeも汎用ヒープも使わず、固定容量の所有権表で動いています。
- inactiveな`LoadedImage`を実行済みと記述する：`satp`はカーネル空間を指したままで、user entryへ`sret`していません。
- U-mode遷移と全system callを同時に作る：特権遷移、trap保存、pointer検査、UART出力、終了時回収の失敗を区別できるよう、`write`と`exit`へ範囲を絞ります。
- payload統合でELF loaderを作り直す：MiniBundleから得たELF byte sliceを既存の検証とmaterializeへ渡し、所有権とrollbackの規約を一つに保ちます。
- 固定容量を暗黙の無制限構造として扱う：`PT_LOAD`は8個、user imageは2,048ページ、所有フレームは2,688個という拒否境界を維持します。

## 演習

U-mode遷移、`write`、`exit`の三項目について、直接の前提、hostで検査できる純粋ロジック、QEMUでしか観測できない状態、失敗時に回収する所有物を四列の表へ整理してください。
次に、Device Treeまたは汎用ヒープを先行させた場合に各列がどう増えるかを書き足してください。
追加した依存が最初の`write`と`exit`の受け入れ条件に必要かを調べ、不要なら後続段階へ戻します。

## 次の章

[第13章「Sv39と単一アドレス空間」](13-sv39.md)では、先行実装したactiveなカーネル空間のpage walkと権限を追います。
[第14章「ELFを実行前アドレス空間へ配置する」](14-elf-loading.md)では、固定容量の所有権表がinactiveな`LoadedImage`を構築して回収する流れを追います。
[全体構成](../reference/architecture.md)、[メモリーマップ](../reference/memory-map.md)、[発展ロードマップ](../reference/roadmap.md)も実装順の根拠として参照してください。
