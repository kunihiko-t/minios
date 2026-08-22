# 問題の切り分け方

症状に最も近い項目を選び、診断コマンドをリポジトリのルートでそのまま実行してください。
複数の原因を同時に直さず、最初の失敗を一つ解消してから同じコマンドを再実行します。

## Rustが安定版1.98.0ではない

- **症状**：`Rust <version> is not supported; exact Rust 1.98.0 stable is required`、またはrustcのバージョン解析エラーで`cargo xtask setup`が停止する。
- **診断コマンド**：`rustc --version && rustup show active-toolchain`
- **考えられる原因**：別の安定版を明示している、`PATH`上のrustcがrustupの固定を迂回している、または`1.98.0-nightly`と`1.98.0-dev`のような接尾辞付きコンパイラーを使っている。
- **修正方法**：`rustup toolchain install 1.98.0`を実行し、リポジトリの`rust-toolchain.toml`が選ばれるシェルから`cargo xtask setup`を再実行する。
  数値部分だけを比較して接尾辞を無視したり、固定用ファイルを別のバージョンへ変更したりしない。

## RISC-Vターゲットがない

- **症状**：`can't find crate for core`、または`Rust target riscv64gc-unknown-none-elf is not installed`と表示される。
- **診断コマンド**：`rustup target list --installed --toolchain 1.98.0`
- **考えられる原因**：固定したツールチェーンに、ベアメタルRISC-V用のターゲット部品が入っていない。
- **修正方法**：`rustup target add riscv64gc-unknown-none-elf --toolchain 1.98.0`を実行し、`cargo xtask setup`を再実行する。

## QEMUがない、または古い

- **症状**：`qemu-system-riscv64 is not installed`、`command not found`、または最低バージョンを満たさないというエラーが出る。
- **診断コマンド**：`qemu-system-riscv64 --version`
- **考えられる原因**：QEMUが未導入、`PATH`が一致していない、またはバージョンが8.2.0未満である。
- **修正方法**：macOSでは`brew install qemu`、UbuntuとDebianでは`sudo apt-get install qemu-system-misc`を使い、`cargo xtask setup`で検出結果を確認する。

## リンカーのセクションが重なる

- **症状**：クロスビルドのリンカーが、領域の重複または再配置のエラーで停止する。
- **診断コマンド**：`cargo build -p minios-kernel --bin minios-kernel --target riscv64gc-unknown-none-elf`
- **考えられる原因**：開始位置`0x8020_0000`、4 KiBのアラインメント、小さなデータとBSSの回収、起動用スタック、`__kernel_end`のいずれかが崩れている。
- **修正方法**：[`linker.ld`](../../kernel/linker.ld)を[メモリーマップ](memory-map.md)と照合し、`cargo test -p xtask cargo::tests::linker_places_small_data_and_bss_probes_inside_boundaries`も実行する。

## UART出力がない

- **症状**：OpenSBIの出力は見えるが、`[ok] traps`と`MiniOS booting...`が一行も出ない。
- **診断コマンド**：`cargo xtask test boot`
- **考えられる原因**：カーネルの入口へ到達していない、スタックかBSSの初期化が壊れている、UARTのベースアドレス`0x1000_0000`かLine Status Registerのビットを誤っている。
- **修正方法**：記録内の`Domain0 Next Address`が`0x8020_0000`か確認し、`entry.S`の`sp`、BSSループ、UARTの送信可能ビット5、volatileな書き込みの順に調べる。

## 起動直後にトラップする

- **症状**：プロンプトの前に`unexpected trap`と`scause/sepc/stval`が出る、または同じトラップを繰り返す。
- **診断コマンド**：`cargo xtask test trap`
- **考えられる原因**：`stvec`のアラインメント、トラップフレームのオフセット、保存と復元の非対称、準備前のSIE有効化のいずれかに問題がある。
- **修正方法**：`scause`の割り込みビットと原因コードを分け、`sepc`をシンボルの位置へ対応付ける。
  `trap.S`の256バイトフレームとx1およびx3からx31までの対称性、トラップ、タイマー、SIEという初期化順も確認する。

## タイマーテストが時間切れになる

- **症状**：`cargo xtask test timer`が5秒で時間切れになり、タイマーのマーカーがない。
- **診断コマンド**：`cargo xtask test timer`
- **考えられる原因**：SBI TIMEの拡張IDか関数ID、10 MHzから100,000サイクルへの換算、STIEとSIE、原因コード5の振り分け、再予約のいずれかが壊れている。
- **修正方法**：`time` CSRで読んだ値に100,000を加えた絶対デッドラインを渡すこと、STIEのビット5より後に全体のSIEビット1を立てること、ハンドラーがSBIエラーを隠さないことを確認する。

## シェル入力が上限を超える

- **症状**：129バイト以上の行で`error: input exceeds 128 bytes`が表示され、Backspaceを押しても実行されない。
- **診断コマンド**：`cargo test -p minios-kernel --lib shell::line::tests`
- **考えられる原因**：長い入力では設計どおりの動作である。
  短い入力でも起きる場合は、容量の計算かCRとLFの処理が退行している。
- **修正方法**：長い行を短くして再入力する。
  実装を直す場合も、上限を超えた後のBackspaceで無効状態を解除せず、次のプロンプトで呼ぶ`clear()`だけが戻す個別テストを保つ。

## QEMUプロセスが残る

- **症状**：テスト終了後も`qemu-system-riscv64`が動いている、端末へ制御が戻らない、または次のテストへ干渉する。
- **診断コマンド**：`pgrep -fl qemu-system-riscv64`
- **考えられる原因**：手動セッションで`shutdown`していない、または時間切れの経路が終了後に`wait`で回収できていない。
- **修正方法**：対話セッションでは`shutdown`を使う。
  自動テストでは、エラーに表示されたコマンド、制限時間、回収時の診断を保存し、`cargo test -p xtask qemu::tests::timeout_reaps_process_and_preserves_both_streams`を実行する。
  自分が起動していないQEMUプロセスを一括終了しない。

## CIだけ失敗する

- **症状**：ローカルの`cargo xtask check`は通るが、Ubuntu上のGitHub Actionsだけ失敗する。
- **診断コマンド**：`cargo xtask setup && cargo xtask check`
- **考えられる原因**：大文字と小文字を区別するパス、書式の差分、Ubuntu版QEMU 8.2の接尾辞、コミットしていないファイルへの依存、ローカルとCIのツールチェーン差のいずれかである。
- **修正方法**：CIログで最初に失敗した段階を、同じコマンドで再現する。
  `git status --short`で追跡ファイルを確認し、`setup`の出力でRust 1.98.0とQEMUの下限を照合する。
  CIだけで検査を飛ばさず、共通の`xtask`境界にある移植性の問題を直す。

[README](../../README.md) | [全体構成](architecture.md) | [学習ガイド](../guide/README.md)
