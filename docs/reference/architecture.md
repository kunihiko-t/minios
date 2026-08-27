# MiniOSの全体構成

MiniOSは、ハードウェアに依存する処理を小さい境界へ閉じ込め、純粋なロジックをホストでテストできるライブラリーへ分けています。
依存方向はシェルとカーネルの入口から型の付いたAPIへ向かいます。
上位モジュールがCSR、SBIのレジスター、UARTのoffset、PTEのbit列を直接操作することはありません。

[Sv39と実行前ELFロードの構成図](../assets/sv39-loaded-image-architecture.html)は、起動後にactiveになるカーネルアドレス空間と、ELF loaderが構築するinactiveな`LoadedImage`を一枚で示します。
図は全体の責務と所有権に絞り、Sv39の三段page walkとELFの全拒否分岐を[第13章](../guide/13-sv39.md)と[第14章](../guide/14-elf-loading.md)へ残しています。

## カーネルのモジュール境界

### バイナリーの入口とリンク

- `kernel/linker.ld`：`_start`、ページ境界へそろえたELFセクション、BSS、64 KiBの起動用スタック、`__kernel_end`を定義します。
- `kernel/src/arch/riscv64/entry.S`：OpenSBIの`a0/a1`を保持し、スタックとBSSを準備して`kernel_main`へ渡します。
- `kernel/src/main.rs`：ハートID、トラップ、物理フレームアロケーター、カーネルアドレス空間、Sv39、タイマー、テスト用機能、シェルを順に接続します。
  下位モジュールの実装詳細は、型の付いた関数を通して呼び出します。

### `arch/riscv64`

- `mod.rs`：RISC-V専用のassembly codeと`csr`、`sbi`、`trap`をまとめる境界です。
- `csr.rs`：`scause`、`sepc`、`stval`、`time`、`sstatus`、`sie`の読み取りと、S-modeや各bitの不変条件を要求する`unsafe`な書き込みを提供します。
  `activate_sv39`はroot PPNから`MODE=8`の`satp`値を作り、`csrw satp`の直後に`sfence.vma`を実行します。
- `sbi.rs`：`set_timer(deadline)`、型の付いたreset種別と理由、`system_reset`、割り込みを無効にした`wfi`を提供します。
  共通の`SbiRet`と`SbiError`には、ホストでテストできるlibrary型を再利用します。
- `trap.S`：256 byteのS-modeフレームへ整数レジスターを保存し、Rustのhandlerを呼び、復元後に`sret`します。
  現在のフレームはuser trap contextを保存してU-modeと往復する実装ではありません。
- `trap.rs`：`scause`を`Interrupt`と`Exception`へ分け、Direct modeの`stvec`初期化、timerの振り分け、breakpointの受け入れテスト、予期しないtrapの診断を担当します。

### 機器とコンソール

- `drivers/uart.rs`：QEMU `virt`の16550互換UARTについて、`write_byte`、`read_byte`、`has_byte`、`fmt::Write`だけを公開します。
  文字列の扱いとcommand処理は持ちません。
- `console.rs`：書式付き出力macroの実装、待機する1 byte入力、1 byte出力、lockを使わない緊急出力を提供します。
  機器のregister配置は上位モジュールから見えません。

### 時刻

- `time.rs`：10 MHzと100 Hzの定数、`ticks_to_millis`、`ticks`、`uptime_millis`をホストとゲストへ公開します。
  RISC-V側では、最初のSBI deadline、STIEとSIE、割り込みごとのtick加算と再予約を担当します。

### 物理メモリー

- `memory/frame.rs`：`PhysFrame`、`FrameError`、`FrameStats`、const genericsを使う`FrameAllocator`を提供します。
  `FrameAllocator::new`は、未所有の排他的な物理範囲を取得する`unsafe`な境界です。
  4 KiBのalignmentと容量は実装が検査します。
  `Clone`でも`Copy`でもない`PhysFrame`と、その値を消費する`deallocate`により、取得後のページ所有権をsafe codeから複製または偽造できません。
  allocatorの上端はboot payload開始位置の`0x8780_0000`であり、`0x8780_0000..0x8800_0000`を割り当てません。
- `memory/mod.rs`：物理メモリーの定数、リンカーセクションを検査する`KernelSections`、フレーム管理の名前空間を提供します。
  汎用ヒープは扱いません。

### Sv39仮想メモリー

- `vm/address.rs`：Sv39の正規仮想アドレス、4 KiBページ、VPN、44 bit PPNを型の生成時に検査します。
- `vm/pte.rs`：branchとleafのencodeとdecode、`R/W/X/U`権限、書き込みだけのleafと`W+X`の拒否を担当します。
- `vm/storage.rs`：テスト用またはQEMUの物理フレームへ、ページ境界を越えないゼロ化、PTEの読み書き、byte copyを提供します。
- `vm/table.rs`：三段page walk、新規写像、変換、固定容量の所有フレーム記録、構築途中と正常破棄の回収を担当します。
  `AddressSpaceStorage`は最大2,688個の所有フレームを記録し、別のallocatorが破棄を拒否した場合は再試行可能な`AddressSpace`を失いません。
- `vm/kernel.rs`：`.text`、`.rodata`、writable section、起動用stack、管理対象RAM、UARTのS-mode専用恒等写像を列挙します。
  カーネルイメージとUARTの物理フレームは借用し、page tableの物理フレームだけを`AddressSpace`が所有します。

`kernel_main`が構築して`satp`へ設定するカーネル`AddressSpace`は**active**です。
この空間は`.text`を`R+X`、`.rodata`を`R`、writable sectionとstackとmanaged RAMとUARTを`R+W`で写像し、すべて`U=0`にします。
boot payload予約領域は未写像です。

### ELFの検証と配置

- `elf/header.rs`：借用したbyte sliceからELF64 headerとprogram headerをchecked parseし、little-endian、RISC-V、`ET_EXEC`を検査します。
- `elf/plan.rs`：最大8個の`PT_LOAD`についてfile範囲、memory範囲、alignment、合同条件、page重複、`W+X`、entry、user range、stackとの衝突をallocation前に検査します。
  user imageはpageへ丸めた合計2,048ページまでです。
- `elf/load.rs`：検証済み`LoadPlan`からsegment、BSS、16ページのuser stackをmaterializeし、`LoadedImage`を返します。
  各user leafは`U=1`であり、`0x3ffe_f000..0x3fff_0000`のguard pageを未写像に保ちます。

ELF loaderが返す`LoadedImage`は**inactive**です。
この値は`AddressSpace`、entry point、user stack上端を所有しますが、そのrootを`satp`へ設定せず、entry pointを実行しません。
構築失敗時はbuilderが所有フレームをrollbackし、成功後は`LoadedImage::destroy`がpage table、user page、stackを回収します。

### シェル

- `shell/line.rs`：容量が固定された印字可能ASCII buffer、Backspace、入力超過状態の保持、状態の初期化を純粋なロジックとして提供します。
- `shell/command.rs`：前後の空白を除いた入力をcommand列挙型へ分類するだけで、UARTとglobal状態へ作用しません。
- `shell/mod.rs`：UARTのpollingとecho、prompt、commandの振り分けを担当します。
  timerの読み取り用APIと、一つだけ存在するallocatorへの参照を使います。
  `shutdown`だけは型の付いたSBI reset境界へ渡します。

### ホストでテストできるライブラリー

- `kernel/src/lib.rs`：trap原因の解読、物理メモリー、Sv39、ELF、SBI戻り値の変換、シェルの純粋ロジック、時刻変換を公開します。
  RISC-V runtimeだけで使う実装は`cfg(target_arch = "riscv64")`で分離します。
- `kernel/src/sbi.rs`：SBIのerrorと値を変換する純粋な規約を定義します。

## `xtask`のモジュール境界

- `xtask/src/main.rs`：process引数、読みやすいerror、終了statusだけを担当します。
- `cli.rs`：`setup`、`build`、`run`、`test`、`check`と、`vm`と`elf`を含むテスト対象の引数構文を定義します。
- `tools.rs`：rustc、rustup target、QEMUの検出、version解析、環境別の修正commandを担当します。
- `cargo.rs`：Cargoの子process、cross build、ELFのpath、commandと出力の診断を担当します。
- `qemu.rs`：QEMU `virt`の引数、対話modeとmarker mode、制限時間、並行した出力の読み取り、processの終了と回収、記録の検証を担当します。
  `vm`経路はactiveなkernel mapping、`elf`経路はinactiveな`LoadedImage`のbyte、BSS、stack、guard、回収をS-modeから観測します。
- `docs.rs`：リポジトリ内の相対Markdown linkと、第1章から第14章までの七つの必須節を検査します。
  code fence、同じ長さのbacktickによるinline code、escapeされた区切り文字はlink解析から除きます。
- `lib.rs`：公開commandを19段階の計画へ変換し、最初の失敗で停止して経過時間をまとめます。

## 起動からシェルまで

1. **QEMUからOpenSBIへ**：`-machine virt -m 128M -smp 1 -bios default`でfirmwareを起動します。
2. **OpenSBIから`_start`へ**：kernel ELFを`0x8020_0000`へ配置し、hart IDを`a0`、DTB addressを`a1`へ入れてS-modeへ制御を渡します。
3. **assemblyによる準備**：`_start`が`SIE`を止め、`__boot_stack_end`を`sp`へ設定し、BSSをゼロ化して`kernel_main(hart_id, dtb)`を呼び出します。
4. **bare modeの初期化**：`kernel_main`がhart IDを記録し、trap vectorと物理フレームallocatorを準備します。
5. **カーネル空間の構築**：各セクション、managed RAM、UARTを恒等写像し、activeになる`AddressSpace`を完成させます。
6. **Sv39の有効化**：root PPNを`satp`へ設定し、直後に`sfence.vma`を実行します。
7. **runtimeの継続**：activeな写像の上で最初のtimer deadlineを設定し、`[ok] timer`と`[ok] memory`をUARTへ出します。
8. **テストまたはシェル**：テスト用機能は対象を観測してmarkerを出し、通常buildはbannerと`minios> `を表示してcommandを処理します。
9. **非同期timer**：シェル実行中もSupervisor timer trapが入り、レジスターの保存、tickの更新、次のdeadline予約、レジスターの復元を経て`sret`で中断位置へ戻ります。

現在の`kernel_main`はinactiveな`LoadedImage`を通常bootで作成せず、U-modeへ遷移しません。
U-mode用trap context、`write`、`exit`、MiniBundle payloadの取り出しは[発展ロードマップ](roadmap.md)の次段階です。

addressと占有範囲は[メモリーマップ](memory-map.md)、用語は[用語集](glossary.md)を参照してください。
