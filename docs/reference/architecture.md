# MiniOSの全体構成

MiniOSは、ハードウェアに依存する処理を小さい境界へ閉じ込め、純粋なロジックをホストでテストできるライブラリーへ分けています。
静的RISC-V 64 ELFをMiniBundle boot payloadから検証し、U-modeで`write`と`exit`を実行して所有frameを回収します。
user ELFはRust製guest crateからbuildし、`cargo xtask bundle`でMiniBundleへ格納します。
NEORV32向けRV32IMカーネルは、M-modeで起動してUARTシェルを実行する別の小さな経路です。
依存方向はシェルとカーネルの入口から型の付いたAPIへ向かいます。
上位モジュールがCSR、SBIのレジスター、UARTのoffset、PTEのbit列を直接操作することはありません。

[Sv39と実行前ELFロードの構成図](../assets/sv39-loaded-image-architecture.html)は、起動後にactiveになるカーネルアドレス空間と、ELF loaderが構築するinactiveな`LoadedImage`を一枚で示します。
図は全体の責務と所有権に絞り、Sv39の三段page walkとELFの全拒否分岐を[第13章](../guide/13-sv39.md)と[第14章](../guide/14-elf-loading.md)へ残しています。

## カーネルのモジュール境界

### バイナリーの入口とリンク

- `kernel/linker.ld`：`_start`、ページ境界へそろえたELFセクション、BSS、64 KiBの起動用スタック、`__kernel_end`を定義します。
- `kernel/src/arch/riscv64/entry.S`：OpenSBIの`a0/a1`を保持し、スタックとBSSを準備して`kernel_main`へ渡します。
- `kernel/linker_neorv32.ld`：NEORV32の内蔵IMEMとDMEMへRV32のセクションを分け、`.data`の格納元と実行先を定義します。
- `kernel/src/arch/riscv32/entry.S`：スタックを設定し、`.data`をIMEMからDMEMへコピーしてBSSをゼロ化した後、`kernel_main32`を呼びます。
- `kernel/src/main.rs`：ハートID、トラップ、物理フレームアロケーター、カーネルアドレス空間、Sv39、タイマー、U-mode test、boot payload、シェルを順に接続します。
  RV32ではUARTを初期化し、対話シェルへ直接進みます。

### `arch/riscv64`

- `mod.rs`：RISC-V専用のassembly codeと`csr`、`sbi`、`trap`をまとめる境界です。
- `csr.rs`：`scause`、`sepc`、`stval`、`time`、`sstatus`、`sie`の読み取りと、S-modeや各bitの不変条件を要求する`unsafe`な書き込みを提供します。
  `activate_sv39`はroot PPNから`MODE=8`の`satp`値を作り、`csrw satp`の直後に`sfence.vma`を実行します。
- `sbi.rs`：`set_timer(deadline)`、型の付いたreset種別と理由、`system_reset`、割り込みを無効にした`wfi`を提供します。
  共通の`SbiRet`と`SbiError`には、ホストでテストできるlibrary型を再利用します。
- `user.S`：`sscratch`と`sp`を交換して専用kernel trap stackへ全registerを保存し、`sret`でU-modeと往復します。
- `trap.S`：S-modeで発生したtimerなどのtrapを保存し、既存のS-mode handlerへ渡します。
- `trap.rs`：`scause`を`Interrupt`と`Exception`へ分け、Direct modeの`stvec`初期化、timerの振り分け、breakpointの受け入れテスト、予期しないtrapの診断を担当します。

### 機器とコンソール

- `drivers/uart.rs`：QEMU `virt`の16550互換UARTについて、`write_byte`、`read_byte`、`has_byte`、`fmt::Write`だけを公開します。
  文字列の扱いとcommand処理は持ちません。
- `drivers/neorv32_uart.rs`：NEORV32のUART0について、ボーレート設定、送信FIFO待ち、受信確認を提供します。
- `console.rs`：書式付き出力macroの実装、待機する1 byte入力、1 byte出力、lockを使わない緊急出力を提供します。
  ターゲットに応じたUARTを選ぶため、機器のregister配置は上位モジュールから見えません。

### 時刻

- `time.rs`：FDTの`/cpus`から発見したtimebaseと100 Hzのティック周波数、`ticks_to_millis`、`ticks`、`uptime_millis`をホストとゲストへ公開します。
  RISC-V側では、最初のSBI deadline、STIEとSIE、割り込みごとのtick加算と再予約を担当します。

### 物理メモリー

- `memory/frame.rs`：`PhysFrame`、`FrameError`、`FrameStats`、const genericsを使う`FrameAllocator`を提供します。
  `FrameAllocator::new`は、未所有の排他的な物理範囲を取得する`unsafe`な境界です。
  4 KiBのalignmentと容量は実装が検査します。
  `Clone`でも`Copy`でもない`PhysFrame`と、その値を消費する`deallocate`により、取得後のページ所有権をsafe codeから複製または偽造できません。
  allocatorの上端はヒープ領域の直下であり、ヒープ、payload窓、FDT予約を合わせた`0x8770_0000..0x8800_0000`を割り当てません。
  各境界は実行時に`fdt::MachineSpec`がDTBから導きます。
- `memory/heap.rs`：managed RAM末尾の1 MiB固定領域を16バイト粒度で分割するfirst-fit free-listヒープを提供します。
  `dealloc`はaddress昇順の空きリストへ挿入して隣接ブロックを併合し、二重解放と領域外ポインターを拒否します。
  `#[global_allocator]`経由で`alloc` crateへ供給され、単一ハートかつ割り込み内で割り当てない規約を前提とします。
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
payloadがない通常bootでは予約領域は未写像です。
payloadがあるbootでは検証済みの使用pageだけをS-mode read-onlyでmapします。

### ELFの検証と配置

- `elf/header.rs`：借用したbyte sliceからELF64 headerとprogram headerをchecked parseし、little-endian、RISC-V、`ET_EXEC`を検査します。
  動的loaderを要求する`PT_INTERP`と`PT_DYNAMIC`は拒否します。
- `elf/plan.rs`：最大8個の`PT_LOAD`についてfile範囲、memory範囲、alignment、合同条件、page重複、`W+X`、entry、user range、stackとの衝突をallocation前に検査します。
  user imageはpageへ丸めた合計2,048ページまでです。
- `elf/load.rs`：検証済み`LoadPlan`からsegment、BSS、16ページのuser stackをmaterializeし、`LoadedImage`を返します。
  各user leafは`U=1`であり、`0x3ffe_f000..0x3fff_0000`のguard pageを未写像に保ちます。

ELF loaderが返す`LoadedImage`は、実行前は**inactive**です。
`load_image_with_kernel_mappings`はkernel mappingをborrowed leafとしてimageへ加え、`UserRun::new`はkernel trap stackを確保します。
`__run_user`が実行用rootを`satp`へ設定し、`sfence.vma`と`fence.i`を実行してからentry pointへ`sret`します。
構築失敗時はbuilderが所有frameをrollbackし、`exit`またはfatal trap後は`UserRun::reclaim`がkernel trap stack、page table、user pageを回収します。

### U-mode system call

- `user/context.rs`：entry、user stack、`sstatus.SPP=0`を持つ`UserContext`を定義します。
- `user/memory.rs`：user pointerを参照として解釈せず、pageごとに`U=1`とread権限を確認してcopyします。
- `user/syscall.rs`：`a7`の番号と`a0`、`a1`、`a2`のargumentsから`write`と`exit`をdispatchします。
- `user/run.rs`：実行用address spaceとkernel trap stackを所有し、Exit control frameの後に回収します。
- `boot_payload.rs`：予約windowから固定長headerを先に検証し、manifestとELF rangeを二段目でparseします。

### 複数processとスケジューリング

- `process.rs`：再入可能な実行単位`Process`と、最大4 slotのround-robin`ProcessTable`を定義します。
  各`Process`は`LoadedImage`（user address spaceとその所有frame）、4ページの専用kernel trap stack、前回中断時の`UserContext`を所有します。
  allocatorやframe memoryへの参照は保持しないため、生存中のprocess同士がborrowを共有しません。
- manifest v2のbundleは`image=`sectionごとに`elf=<offset>,<len>`で共有ELF領域内のrangeを宣言し、slot indexがmanifest順のpidになります。
- U-mode実行中のsupervisor timer割り込みは`user/trap.rs`が`TrapAction::Timer`へ分類し、trap handlerはtickを再アームしてから`Preempted`のoutcomeでkernelへ戻ります。
  切り替えはtrap内ではなく`run_boot_payload`のdispatch loopが行うため、kernel trap stackは常に「実行中process専用」の不変条件を保ちます。
- processの`exit`またはfatal trapでslotを取り除き、全所有frameを回収してから次を選びます。
  manifest v2では終了を`PROC_EXIT` frame（pidと終了code）で個別に通知し、v1の単一imageでは従来の`EXIT` frameを維持します。
- `read`は入力未到着のとき`SyscallFlow::Blocked`を返し、`sepc`をecallへ戻してkernelへ戻ります。
  processは`BlockedOnStdin`として再選対象から外れ、UARTのdata-readyを検出した時点で起こされ、同じecallをやり直して完了します。
  Stdin frameの受信は`StdinStaging`内の再開可能なdecoderがbyte単位で蓄積し、frame途中でbyteが尽きた再試行は`WouldBlock`として再び`Blocked`へ戻るため、受信途中の間も他processが進み続けます。

### シェル

- `shell/line.rs`：容量が固定された印字可能ASCII buffer、Backspace、入力超過状態の保持、状態の初期化を純粋なロジックとして提供します。
- `shell/command.rs`：前後の空白を除いた入力をcommand列挙型へ分類するだけで、UARTとglobal状態へ作用しません。
- `shell/mod.rs`：UARTのpollingとecho、prompt、commandの振り分けを担当します。
  RV64ではtimerの読み取り用APIと一つだけ存在するallocatorへの参照を使い、`shutdown`をSBI reset境界へ渡します。
  RV32では`uptime`を`cycle`カウンターから、`memory`をIMEM/DMEMのリンカー境界から求め、`shutdown`を`wfi`による停止として実行し、CRLFのLFを一度だけ読み飛ばします。

### ホストでテストできるライブラリー

- `kernel/src/lib.rs`：trap原因の解読、物理メモリー、Sv39、ELF、SBI戻り値の変換、シェルの純粋ロジック、時刻変換を公開します。
  RISC-V runtimeだけで使う実装は`cfg(target_arch = "riscv64")`で分離します。
- `kernel/src/sbi.rs`：SBIのerrorと値を変換する純粋な規約を定義します。

## `xtask`のモジュール境界

- `xtask/src/main.rs`：process引数、読みやすいerror、終了statusだけを担当します。
- `cli.rs`：`setup`、`build`、`run`、`bundle`、`test`、`check`と、user-entry、user-trap、user-syscall、user-exit、payload、payload-args、payload-stdin、schedを含む引数構文を定義します。
- `tools.rs`：rustc、rustup target、QEMUの検出、version解析、環境別の修正commandを担当します。
- `cargo.rs`：Cargoの子process、cross build、ELFのpath、commandと出力の診断を担当します。
- `guest.rs`：Rust guestのrelease buildと、kernelのELF parserによる配置契約のhost検査を担当します。
- `bundle.rs`：manifestの生成と検証、MiniBundleの正規配置、複数imageの連結と`elf=`range記録、SHA-256 digest、`cargo xtask bundle`のfile出力を担当します。
- `qemu.rs`：QEMU `virt`の引数、MiniBundle loader、marker mode、制限時間、並行した出力の読み取り、childのkillとwait、記録の検証を担当します。
  `user-exit`、`payload`、`payload-args`、`payload-stdin`経路はstdout、必要な場合はstderr、Exit、回収をcontrol frameで観測します。
  `payload-args`経路のbundleにはbuild済みRust guestを格納します。
  `sched`経路は二つのguestを持つmanifest v2 bundleを通常カーネルへ渡し、stdoutの交差と`PROC_EXIT` frameを検査します。
- `docs.rs`：リポジトリ内の相対Markdown linkと、第1章から第17章までの七つの必須節を検査します。
  code fence、同じ長さのbacktickによるinline code、escapeされた区切り文字はlink解析から除きます。
- `lib.rs`：公開commandを33段階の計画へ変換し、RV64とRV32のクロスビルド、host test、user runtimeとpayloadのQEMU testを実行します。

## Rustユーザープログラム

- `guest/src/main.rs`：`no_std`と`no_main`のguest本体であり、`_start`、`guest_main(argc, argv)`、`write`と`exit`の`ecall`、panic時の終了code70を提供します。
- `guest/linker.ld`：`_start`を先頭に固定し、`.text`と`.rodata`を`0x0010_0000`からのR+X segmentへ置く配置契約を定義します。
- `guest/build.rs`：linker scriptを呼び出しcwdに依存しない絶対pathで渡します。

`cargo xtask bundle`はguestのbuild、manifest生成、MiniBundle file出力を一つの開発commandにまとめます。
`cargo xtask test payload-args`はbuild済みguestを含むbundleをQEMU loaderへ渡し、program nameと二つの引数のstdout出力、終了code、回収diagnosticをframe順序で検証します。
このguestはRV64GCとQEMU `virt`専用であり、RV32IMのNEORV32実機経路では実行しません。

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

payload bootでは`kernel_main`がMiniBundleを二段階で検証し、manifestの各imageをprocessとしてspawnしてからround-robinでU-modeへ遷移させます。
U-modeの`ecall`は`sscratch`によるstack交換を通り、`write`または`exit`を処理してからkernelへ戻ります。
U-mode中のtimer割り込みは実行中processのtrap stackへcontextを保存し、kernel側のdispatch loopが次のprocessを選んで再開します。

## NEORV32の起動からシェルまで

1. FPGAのブート機構が、カーネルのロードイメージを内蔵IMEMへ配置して`pc=0`から実行します。
2. RV32の`_start`がDMEM上端へスタックを置き、`.data`をIMEMからDMEMへコピーしてBSSをゼロ化します。
3. `kernel_main32`が96 MHzと19,200 baudの前提でUART0を初期化し、起動メッセージを出します。
4. 固定長の入力バッファーを使うシェルが`help`、`info`、`echo`、`ls`、`cat`を処理します。

この経路はOpenSBI、Sv39、タイマー、物理ページ管理、U-modeを使いません。
RV64の仕組みをそのまま縮小した構成ではなく、UARTとシェルの境界を実機へ移植するための入口です。
アプリケーション実行方式の検討記録は[NEORV32でアプリケーションを動かす方式の検討](neorv32-applications.md)にあります。

## NEORV32 read-only storage

- `storage/sd.rs`：SDHCとSDXC専用のread-only SPI driverであり、CMD17によるsector読み取りとCRC16検証を行います。
- `storage/fat32.rs`：read-only FAT32 parserであり、partition選択、BPB検証、root directory反復、8.3名lookupを行います。
- `drivers/neorv32_sd.rs`：GPIO bit-bangのSPI busであり、`rdcycle`基準のdelayで初期化時375 kHz以下を保ちます。
- `shell`の`ls`と`cat`：単一sessionを使い回し、型付きerrorを`sd:`接頭辞の安定messageへ写像します。

IMEM契約は実効32 KiBであり、RV32 buildは`opt-level=z`で収めます。
詳細は[NEORV32 read-only FAT32設計](sd-fat32.md)を参照してください。

addressと占有範囲は[メモリーマップ](memory-map.md)、用語は[用語集](glossary.md)を参照してください。
