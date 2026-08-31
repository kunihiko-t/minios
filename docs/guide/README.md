# MiniOS学習ガイド

このガイドは、前の章で作った機能を次の章の土台にする実習教材です。
初めて読むときは第1章から第16章まで順に進み、実装中は各章の「実行と確認」と「よくある失敗」を参照してください。

## 全16章

1. [MiniOSで学ぶこと](01-introduction.md)：到達点を知り、ホストとゲストの境界を分類する。
2. [開発環境とQEMU](02-setup.md)：RustターゲットとQEMUを、再現可能なコマンドで診断する。
3. [`no_std`とリンク配置](03-no-std-and-linking.md)：ELFセクションとカーネルのメモリー境界を説明する。
4. [OpenSBIからの起動](04-boot-with-opensbi.md)：ファームウェアのABIからRustの入口までを追跡する。
5. [UARTで文字を送受信する](05-uart.md)：volatileなMMIOとコンソール層の責務を区別する。
6. [パニックと緊急診断](06-panic-and-diagnostics.md)：ロックを使わない異常終了経路を設計する。
7. [RISC-Vの例外と割り込み](07-traps-and-interrupts.md)：CSRとトラップフレームから原因を読み取る。
8. [Supervisorタイマー割り込み](08-timer-interrupts.md)：SBIのデッドラインと100 Hzのティックを説明する。
9. [物理メモリーとページ管理](09-physical-memory.md)：ビットマップの所有権と統計値の不変条件を検証する。
10. [UART対話シェル](10-shell.md)：入力長の制限とコマンドの作用を分離する。
11. [テストハーネスの仕組み](11-test-harness.md)：ホスト、QEMU、CIを同じ24段階で検証する。
12. [次に作るもの](12-next-steps.md)：完了した順序変更と、その後の拡張を依存関係で説明する。
13. [Sv39と単一アドレス空間](13-sv39.md)：三段page walk、PTE権限、activeなカーネル写像を検証する。
14. [ELFを実行前アドレス空間へ配置する](14-elf-loading.md)：検証済みELFからinactiveな`LoadedImage`を構築して回収する。
15. [U-modeでELFを実行する](15-user-mode.md)：`sret`、`sscratch`、system call、終了後の回収を追う。
16. [boot payloadを実行する](16-boot-payload.md)：MiniBundleの検証、read-only mapping、QEMU loaderを追う。

## ガイド内の移動

各章末の「次の章」には、前後の章と必要な資料へのリンクがあります。
最初は「次の章」のリンクをたどり、用語やアドレスを確認したときはブラウザーの戻る操作で同じ位置へ戻ると読み進めやすくなります。
第1章の前と第16章の次は、この索引です。
`01-...md`から`16-...md`までの各章は、「学習目標」「背景」「実装」「実行と確認」「よくある失敗」「演習」「次の章」の七つの節を持ちます。
`cargo xtask check`は、この構造も検査します。

[リポジトリのREADME](../../README.md) | [全体構成](../reference/architecture.md) | [問題の切り分け方](../reference/troubleshooting.md)
