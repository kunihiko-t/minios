# MiniOSの全体構成

MiniOSは、ハードウェアに依存する処理を小さい境界へ閉じ込め、純粋なロジックをホストでテストできるライブラリーへ分けています。
依存方向はシェルとカーネルの入口から型の付いたAPIへ向かいます。
上位モジュールがCSR、SBIのレジスター、UARTのオフセットを直接操作することはありません。

## カーネルのモジュール境界

### バイナリーの入口とリンク

- `kernel/linker.ld`：`_start`、ELFセクション、BSS、64 KiBの起動用スタック、`__kernel_end`を定義します。
- `kernel/src/arch/riscv64/entry.S`：OpenSBIの`a0/a1`を保持し、スタックとBSSを準備して`kernel_main`へ渡します。
- `kernel/src/main.rs`：ハートIDの記録、トラップ、タイマー、フレームアロケーターの順序付き初期化、テスト用機能、シェル、異常時の診断をまとめます。
  下位モジュールの実装詳細は、型の付いた関数を通して呼び出します。

### `arch/riscv64`

- `mod.rs`：RISC-V専用のアセンブリーコードと`csr`、`sbi`、`trap`をまとめる境界です。
- `csr.rs`：`scause`、`sepc`、`stval`、`time`、`sstatus`、`sie`の読み取りと、S-modeや各ビットの不変条件を要求する`unsafe`な書き込みを提供します。
  呼び出し側は、生のインラインアセンブリーをほかのモジュールへ複製しません。
- `sbi.rs`：`set_timer(deadline)`、型の付いたリセット種別と理由、`system_reset`、割り込みを無効にした`wfi`を提供します。
  共通の`SbiRet`と`SbiError`には、ホストでテストできるライブラリー型を再利用します。
- `trap.S`：256バイトのフレームへ整数レジスターを保存し、Rustのハンドラーを呼び、復元後に`sret`します。
- `trap.rs`：`scause`を`Interrupt`と`Exception`へ分け、Directモードの`stvec`初期化、タイマーの振り分け、ブレークポイントの受け入れテスト、予期しないトラップの診断を担当します。

### 機器とコンソール

- `drivers/uart.rs`：QEMU `virt`の16550互換UARTについて、`write_byte`、`read_byte`、`has_byte`、`fmt::Write`だけを公開します。
  文字列の扱いとコマンド処理は持ちません。
- `console.rs`：書式付き出力マクロの実装、待機する1バイト入力、1バイト出力、ロックを使わない緊急出力を提供します。
  機器のレジスター配置は上位モジュールから見えません。

### 時刻

- `time.rs`：10 MHzと100 Hzの定数、`ticks_to_millis`、`ticks`、`uptime_millis`をホストとゲストへ公開します。
  RISC-V側では、最初のSBIデッドライン、STIEとSIE、割り込みごとのティック加算と再予約を担当します。

### メモリー

- `memory/frame.rs`：`PhysFrame`、`FrameError`、`FrameStats`、const genericsを使う`FrameAllocator`を提供します。
  `FrameAllocator::new`は、未所有の排他的な物理範囲を取得する`unsafe`な境界です。
  4 KiBのアラインメントと容量は実装が検査します。
  `Clone`でも`Copy`でもない`PhysFrame`と、その値を消費する`deallocate`により、取得後のページ所有権を安全なコードから複製または偽造できません。
  ビットマップは範囲外と二重解放を診断しますが、`unsafe`の呼び出し側やアロケーター外部のサブシステムが作った別名参照は検出しません。
- `memory/mod.rs`：物理フレーム管理の名前空間です。
  仮想アドレスとヒープはまだ扱いません。

### シェル

- `shell/line.rs`：容量が固定された印字可能ASCIIバッファー、Backspace、入力超過状態の保持、状態の初期化を純粋なロジックとして提供します。
- `shell/command.rs`：前後の空白を除いた入力をコマンド列挙型へ分類するだけで、UARTとグローバル状態へ作用しません。
- `shell/mod.rs`：UARTのポーリングとエコー、プロンプト、コマンドの振り分けを担当します。
  タイマーの読み取り用APIと、一つだけ存在するアロケーターへの参照を使います。
  カーネルの入口から受け取ったハートIDを`info`へ渡します。
  `uptime_millis()`と`ticks()`を読み、`uptime: <n> ms`、`ticks: <n>`の順に出力します。
  `shutdown`だけは型の付いたSBIリセット境界へ渡します。

### ホストでテストできるライブラリー

- `kernel/src/lib.rs`：トラップ原因の解読、メモリー管理、SBI戻り値の変換、シェルの純粋ロジック、時刻変換を公開します。
  RISC-Vのランタイムだけで使う実装は`cfg(target_arch = "riscv64")`で分離します。
- `kernel/src/sbi.rs`：SBIのエラーと値を変換する純粋な規約を定義します。

## `xtask`のモジュール境界

- `xtask/src/main.rs`：プロセス引数、読みやすいエラー、終了ステータスだけを担当します。
- `cli.rs`：`setup`、`build`、`run`、`test`、`check`とテスト対象の引数構文を定義します。
- `tools.rs`：rustc、rustupターゲット、QEMUの検出、バージョン解析、環境別の修正コマンドを担当します。
- `cargo.rs`：Cargoの子プロセス、クロスビルド、ELFのパス、コマンドと出力の診断を担当します。
- `qemu.rs`：QEMU `virt`の引数、対話モードとマーカーモード、制限時間、並行した出力の読み取り、プロセスの終了と回収、記録の検証を担当します。
- `docs.rs`：リポジトリ内の相対Markdownリンクと、第1章から第12章までの七つの必須節を検査します。
  コードフェンス、同じ長さのバッククォートによる行内コード、エスケープされた区切り文字はリンク解析から除きます。
- `lib.rs`：公開コマンドを14段階の計画へ変換し、最初の失敗で停止して経過時間をまとめます。

## 起動からシェルまでの八段階

1. **QEMUからOpenSBIへ**：`-machine virt -m 128M -smp 1 -bios default`でファームウェアを起動します。
2. **OpenSBIから`_start`へ**：カーネルELFを`0x8020_0000`へ配置し、ハートIDを`a0`、DTBアドレスを`a1`へ入れてS-modeへ制御を渡します。
3. **アセンブリーによる準備**：`_start`が`SIE`を止め、`__boot_stack_end`を`sp`へ設定します。
4. **Rustが必要とする状態**：`__bss_start..__bss_end`をゼロ化し、`kernel_main(hart_id, dtb)`をC ABIで呼び出します。
5. **ランタイムの初期化**：`kernel_main`がハートIDを記録し、トラップベクター、最初のタイマーという順に初期化します。
   続いて、OpenSBIとカーネルイメージを除外した`__kernel_end..0x8800_0000`を、局所的な物理フレームアロケーターへ排他的に渡します。
6. **UARTへの状態表示**：`[ok] traps`、`[ok] timer`、`[ok] memory`と起動バナーをUARTへ出します。
   テスト用機能が有効なら、マーカーを出してSBIリセットへ進みます。
7. **シェルのループ**：通常のビルドは`minios> `を表示し、UART入力を長さの限られた一行へ反映してコマンドを処理します。
8. **非同期タイマー**：シェル実行中もSupervisorタイマートラップが入り、レジスターの保存、ティックの更新、次のデッドライン予約、レジスターの復元を経て、`sret`で中断位置へ戻ります。

シェルの安定した応答は、`info`の`MiniOS 0.1.0 on RISC-V 64`と`hart id: 0`、`uptime`の`uptime: <n> ms`と`ticks: <n>`、`memory`の`total`、`allocated`、`free`です。
QEMUの検証部は、最初のプロンプト以降について、コマンドのエコーと全応答をこの順の完全一致する行として検査します。

アドレスと占有範囲は[メモリーマップ](memory-map.md)、略語は[用語集](glossary.md)を参照してください。
