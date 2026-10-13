# 発展ロードマップ

この文書は、実装済みの範囲、次の受け入れ単位、その後の方向を区別します。
現在のrelease gateは、RV64とRV32のクロスビルド、host test、全QEMU経路を実行します。
段階の一覧は[テストハーネスの章](../guide/11-test-harness.md)にあります。

## 実装済み

物理フレームアロケーターは、ヒープ領域、boot payload予約領域、FDT予約領域を除く`align_up(__kernel_end, 0x1000)..0x8770_0000`を管理します。

汎用ヒープの第一歩も完了しています。
managed RAM末尾の1 MiB固定領域を16バイト粒度のfirst-fit free-listで管理し、`#[global_allocator]`経由で`alloc` crateの`Box`や`Vec`を利用できます。
解放時はaddress昇順のlistで隣接ブロックを併合し、二重解放と領域外ポインターを実行時に拒否します。
`cargo xtask test heap`は、`Vec`の成長、`Box`の割り当てと解放、初期領域を超える割り当てでの動的拡張、統計値の整合をQEMU上で確認します。
ヒープはOOM時にframe poolの最上位pageを取り込んで下方へ成長し、ヒープとprocess frameが同一poolを分け合います。
単一ハートかつ割り込み内で割り当てない規約を前提とし、成長したpageはヒープへ返しません。

Device Tree対応も完了しています。
`kernel_main`はOpenSBIが`a1`へ渡すDTBをbare modeで解析し、RAM範囲、16550 UARTベース、`/cpus`の`timebase-frequency`を`fdt::MachineSpec`として発見します。
QEMU `virt`の配置契約に合わせてRAM最後の2 MiBをFDT予約領域とし、その直下の6 MiBをboot payload窓へ置きます。
`cargo xtask test fdt`は、発見したmachine記述がhost側の期待値と一致することをQEMU上で確認します。

Sv39の節目は完了しています。
カーネルは4 KiB leafだけを使う三段page tableを構築し、section、managed RAM、UARTをS-mode専用で恒等写像します。
`satp`の更新直後には`sfence.vma`を実行し、既存のboot、trap、timer、memory、shell経路をactiveなカーネルアドレス空間で維持します。
`cargo xtask test vm`は全sectionとUARTの物理addressと`R/W/X/U`を検査し、payload開始位置が未写像であることを観測します。

実行前ELF loaderの節目も完了しています。
loaderは静的なRISC-V ELF64をallocation前に検証し、最大8個の`PT_LOAD`、2,048 user page、16 stack pageをヒープ上の可変長な所有権台帳へmaterializeします。
成功時の`LoadedImage`はinactiveな`AddressSpace`、entry point、user stack上端を所有し、失敗時と正常破棄時の両方で所有フレームを回収できます。
`cargo xtask test elf`はfile byte、partial page、BSS、stack、guard、U bit、破棄後のallocator統計をS-modeから観測します。

U-mode実行も完了しています。
`UserContext`は`sepc`、user stack、`sstatus.SPP=0`を準備し、`user.S`は`sscratch`でkernel trap stackへ切り替えて`sret`します。
`write`は`a7`、`a0`、`a1`、`a2`の規約を使い、user pointerのrangeとpage権限を検査してからUART control frameを送ります。
`exit`とfatal trapの後は`UserRun`がkernel trap stack、user page、page tableを回収します。

MiniBundle boot payloadも完了しています。
予約windowのheaderを先に検証してからmanifestとELF rangeをparseし、使用pageだけをS-mode read-onlyでmapします。
`cargo xtask test payload`はQEMU loader、Ready、stdout、stderr、Exit、回収diagnosticを確認します。
manifestの`name`と`arg=`は初期user stackへ配置され、guestは`a0=argc`と`a1=argv`から読み取れます。
`cargo xtask test payload-args`は、この引数がmanifestの順序どおりguestへ届くことを確認します。

Rustユーザープログラムの節目も完了しています。
`guest/` crateは`no_std`のRustプログラムを静的RV64 ELFへbuildし、`cargo xtask bundle`がmanifestとdigest付きMiniBundleを決定的に生成します。
`cargo xtask test payload-args`はbuild済みguestをQEMU loaderへ渡し、program nameと二つの引数のstdout出力、終了code42、回収diagnosticをframe順序で検証します。
手書きargv ELF fixtureはRust guestへ置き換え、stderr経路を担うMK6 fixtureは残しています。

NEORV32向けRV32IMカーネルも起動できます。
M-modeの入口がIMEMに置かれた`.data`初期値をDMEMへコピーし、BSSをゼロ化してからUART0と対話シェルを起動します。
`cargo xtask check`は、この経路をrelease設定でClippyとクロスビルドに通します。
NEORV32実機経路はkernel shellまでに限定し、U-mode guestは対象外とします。
検討記録は[NEORV32でアプリケーションを動かす方式の検討](neorv32-applications.md)にあります。

NEORV32向けread-only storageの節目も完了しています。
GPIO-SPI経由のSD sector readerとFAT32 parserをRV32 shellの`ls`と`cat`から使い、host testでprotocolと配置を検証します。
RV32IMEM契約は実効32 KiBへ更新し、RV32 buildは`opt-level=z`で収めます。
設計は[NEORV32 read-only FAT32設計](sd-fat32.md)にあります。

複数processのプリエンプティブ実行も完了しています。
manifest v2のbundleは最大4個のimageを宣言でき、kernelは各imageを独立したaddress space・専用kernel trap stack・保存contextを持つ`Process`としてspawnします。
U-mode実行中のsupervisor timer割り込みは`TrapAction::Timer`へ分類され、trap handlerがtickを再アームしてkernelへ戻ると、`ProcessTable`のround-robinが次のprocessを選んで`__run_user`へ再投入します。
各processの終了は`PROC_EXIT` frameで個別に通知され、tableから取り除いて全所有frameを回収します。
`cargo xtask test sched`は、出力のたびに`yield`するprocessの出力の間に短命processの出力が挟まることと、切り替え回数の報告をQEMU上で確認します。
`read`は入力未到着ならprocessを`BlockedOnStdin`へ回してecallをやり直すため、stdin待ちの間も他processが進みます（`cargo xtask test sched-io`で検証）。
Stdin frameの受信は`StdinStaging`の再開可能なdecoderが担い、frame途中のbyte枯渇でもprocessは再びstdin待ちへ戻ります（`cargo xtask test sched-io-partial`で検証）。
guestからの動的process生成も完了しており、`spawn`はFAT32上のELFを新processとして起動してpidを返し、`getpid`は呼び出しprocessのpidを返します（`cargo xtask test file-spawn`で検証）。
`waitpid`は対象processの終了codeを終了台帳から回収し、対象がliveなら呼び出しprocessを`BlockedOnPid`へ回して対象の終了で起こします（`cargo xtask test file-waitpid`で検証）。
file metadataの公開も完了しており、`stat`はpathで解決したfileまたはdirectoryの`Stat`（sizeとkind）をuser bufferへ、`fstat`はopen中のfdのmetadataを同じ形式で返します（`cargo xtask test file-stat`で検証）。
`readdir`はdirectoryの中身をindex順の`DirEnt`（nameとkind）として返し、空pathはroot directoryを指します（`cargo xtask test file-readdir`で検証）。
`exec`は呼び出しprocessのimageをFAT32上のELFで置き替え、pidとfd tableを引き継いだまま新imageのentryから再開します（`cargo xtask test file-exec`で検証）。
`spawn`したchildは呼び出し側のfd tableのsnapshotを引き継ぎ、parentが開いたfileを同じfd番号とoffsetで読めます（`cargo xtask test file-fdinherit`で検証）。
`pipe`はkernel所有の256 byte ring bufferのread/write両端をfdとして返し、継承した端経由でprocess間へbyteを流せます。空のreadと満杯のwriteは`BlockedOnPipe`で待ち、端のcloseやprocess終了がwaiterを起こします（`cargo xtask test file-pipe`で検証）。

## 今後の段階

完成度を上げる作業は、新機能の追加だけを指しません。
現在の実装には、文書と実装の食い違い、guestから使えないkernel機能、処理の上限が小さすぎて実用に届かない箇所が残っています。
そこで以下では、作業を五つの段階に分け、各段階の受け入れ条件を`cargo xtask test`の経路または`cargo xtask check`の検査として書きます。
段階の順序は依存関係で決めており、段階1はuser spaceのlibraryを前提とし、段階3は段階2の割り込み基盤を前提とします。

### 段階0：文書と実装の整合（完了）

段階0では、新しいkernel codeを加えずに文書を実装へ追いつかせました。

- release gateの段階数を本文へ手書きする方式をやめ、`cargo xtask check`のdocs検査が第11章の実行例と段階一覧を検査計画と照合するようにしました。
- READMEの「現在の制約」を、未実装機能と実装済み機能の上限に分けて書き直しました。
- 学習ガイドへ第18章から第23章を追加し、第12章を第13章以降と本書への案内に縮めました。
- guestのsyscall wrapperを`minios_guest` library（`guest/src/lib.rs`と`guest/src/sys.rs`）へまとめ、全guest programがそれを使うようにしました。

### 段階1：user spaceを実用に届かせる（完了）

段階1では、guestが「自分でprogramを書いて動かせる」水準へ到達させました。
user heap、時刻と待機、fdの複製、spawnへの引数渡し、user library、user mode shellがそろい、FAT32上のprogramをpipeとredirectでつないで実行できます。

- **`sbrk`**（完了）：guestは`SbrkAllocator`をglobal allocatorにして`Vec`と`String`を使えます（`cargo xtask test user-heap`で検証）。
- **`clock`と`sleep`**（完了）：`clock`は`time.rs`の`uptime_millis`を返し、`sleep`は`BlockedUntil(tick)`で待機中のprocessにCPUを渡します（`cargo xtask test user-sleep`で検証）。
  `sleep`は戻り値0を書いてecallの次へ進めてからblockするため、起床後にsyscallをやり直しません。
- **`yield`**（完了）：sched_aのbusy-waitを`yield`で置き換え、schedulerの検証がtime sliceに依存しなくなりました。
- **`dup2`と`MAX_OPEN_FILES`の引き上げ**（完了）：fd 0、1、2をconsole entryを持つ普通のslotへ変え、`dup2`でpipeやfileへ付け替えられるようにしました（`cargo xtask test user-dup`で検証）。
  open file数の上限は4から16へ上げ、`spawn`したchildは差し替えたfd 1をそのまま継承します。
- **`spawn`への引数渡し**（完了）：`spawn`は`a2`と`a3`で`[pointer, length]`のentry配列を受け取り、manifestと同じ初期stack ABIでchildの`argv`を積みます（`cargo xtask test spawn-args`で検証）。
  `argc`が0なら従来どおりpathのbasenameだけを`argv[0]`にするため、既存の呼び出し側は変わりません。
- **user library crate**（完了）：`minios_guest` libraryへ`entry!`、`println!`、`File`、`spawn`のような薄い型を加え、全sample guestを書き直しました。
  `_start`と`panic_handler`は各programから消え、不正なpointer、失効したfd、型で表せない引数を渡す検査だけが生のsyscall wrapperを使います。
- **user mode shell**（完了）：`SH.ELF`はstdinを1行ずつ読み、`BIN/NAME.ELF`へ解決したcommandを最大3個の`|`によるpipelineと`<`、`>`によるredirectで起動します（`cargo xtask test user-shell`で検証）。
  shellは`dup2`でfd 0と1を付け替えてから`spawn`し、consoleへ戻してから起動順に`waitpid`します。
  kernel shellは起動と診断に残し、`cargo xtask run`は引き続きkernel shellを起動します。

受け入れ条件は`cargo xtask test user-heap`、`user-sleep`、`user-dup`、`user-shell`の四経路で、特に`user-shell`はQEMU上で`cat FILE.TXT | wc`相当のpipelineが動くことを観測します（完了）。

### 段階2：kernelの堅牢化と整理

段階1で機能が増えると、kernel内の大きなfileと暗黙の規約が保守の障害になります。
この段階では振る舞いを変えず、境界と検査を整えます。

- **大きなfileの分割**（完了）：`storage/fat32/`は`mod.rs`（type・定数）と`parse.rs`/`read.rs`/`write.rs`/`tests/`へ、`user/syscall/`は`mod.rs`（sink/source境界）と`dispatch.rs`/`tests/`へ、`main.rs`は`qemu_test.rs`/`user_trap.rs`/`proc_runtime.rs`/`user_probe.rs`/`payload.rs`へ分け、各fileは1,500行以下です。
- **user pointer検査の一本化**（完了）：読み出しは`copy_from_user`、書き込みは`copy_to_user`がpageごとの`U`/権限検査を兼ね、`ReadComplete`のcopy-outも`complete_read`経由で`copy_to_user`を通ります。
  `sstatus.SUM`を立てたraw pointer書き込みは廃止し、検査経路と書き込み経路がずれて検査漏れになる形を除きました。
- **processの異常終了の統一**（完了）：fatal trapを起こしたprocessの回収経路が、単一実行時の`UserRun`と複数process時の`ProcessTable`で分かれていました。
  複数process時にfatal trapが起きても他のprocessが継続し、`waitpid`が異常終了codeを受け取れる経路として`cargo xtask test proc-fault`で固定しています。
- **`kill`**（完了）：`kill(pid)`（番号28）を追加し、対象を`Killed`へmarkしてrun loopが`waitpid`へ終了code `EXIT_CODE_KILLED`（75）を渡します（`cargo xtask test proc-kill`で検証）。
- **heap成長pageの返却**（完了）：`dealloc`の直後に`Heap::release_grown_pages`が下端の連続した空きpageを`init_start`まで切り離し、`GlobalAlloc`実装が`FrameAllocator`へ返します。
  初期領域はpoolの管理範囲外のため返さず、`cargo xtask test heap`で返却後の基準復元を検証しています。
- **FAT32 write pathの整合性**（完了）：FSInfoは読まず実値も維持せず、最初のFAT更新で`free`・`next`を規定の「不明」値へ揃える形に決めました。
  signatureの無いsectorへは正しいFSInfoをまるごと書き、二面目が失われたimageは一面目で読み進めて次の更新で再同期します。cluster chainとcluster再利用の規約はhost testで固定しています。
- **host側のfuzz test**（完了）：FAT32 volume parser、ELF headerとload plan、boot payloadのmanifest経路に`cargo xtask fuzz`の決定性harnessを置きました。
  `cargo fuzz`依存は持ち込まず、標準libraryだけのrunnerでseed・corpus・replay・入力あたりtimeout・allocation上限を扱います。
  release gateには含めず、手動実行の手順を`CONTRIBUTING.md`へ書いています。

受け入れ条件は、既存の全QEMU経路が変更前と同じ出力で通ること、`cargo xtask test proc-fault`と`proc-kill`が追加されること、各fileが1,500行以内に収まることです。

### 段階3：割り込み駆動のI/Oとnetwork

ここまでのkernelはUART入力をpollingで読み、virtio-blkも完了を待ち続けます。
networkを扱うには、外部割り込みでdeviceの完了を受け取る基盤が先に要ります。

- **PLIC driver**（完了）：QEMU `virt`のPLICを`drivers/plic.rs`で初期化し、優先度・enable・threshold・claim/completeをS-mode context 1へ設定します。
  FDTの`riscv,plic0`互換nodeから`MachineSpec::plic_base`を取り、kernel mappingに専用のPLIC MMIO窓を追加しています。
- **UART受信割り込み**（完了）：supervisor external割り込みをtrap経路へ接続し、PLICのclaim/dispatch/completeからUARTの受信を処理します。
  全processが待ちへ入ったidleではdispatch loopが`wfi`で眠り、受信割り込みで`BlockedOnStdin`のprocessを起こします（`cargo xtask test irq-uart`で検証）。
- **virtio-blkの割り込み化**（完了）：deviceのused-buffer完了をPLIC経由のvirtio割り込みで受け取り、I/O完了待ちのprocessを`BlockedOnIo`で退避させます。
  virtio-mmioのsource番号はFDT nodeの`interrupts`宣言から取り（node列挙順はslot順とは限らないため）、claimがdeviceのinterrupt lineをackします。
  requestは`in_flight`/`done`の状態機械で追跡し、retryが同じ完了をused ringから回収します（`cargo xtask test irq-blk`で検証）。
- **virtio-net driver**（完了）：`kernel/src/net/virtio_net.rs`がrx/txの二つのvirtqueueでethernet frameを送受し、`-netdev user`のgateway 10.0.2.2へARP requestを送ってreplyを受け取ります（`cargo xtask test net-arp`で検証）。
  virtio-mmioのregister/queue共通部は`kernel/src/virtio.rs`へ集約してblockとnetの両driverが共有し、net deviceのused-buffer割り込みもblkと同じPLIC claim経路で受けます。
  virtio_net_hdrは`VIRTIO_NET_F_MRG_RXBUF`をnegotiateした12 byte layoutで統一し、rx bufferは常時deviceへpostしたまま`drain_rx`が回収・再postします。
- **最小のnetwork stack**（完了）：`net/stack.rs`が`Link` traitでdeviceを抽象化し、ARP cache・自分宛ARP requestへのreply・ICMP echoへのreply・4 socketのUDP tableと受信queueを持ちます（`net-ping`で検証）。
  guestへは`socket`、`sendto`、`recvfrom`の三つのsyscallで公開し、未受信の`recvfrom`は`BlockedOnIo`で巻き戻し待機してvirtio-netの受信割り込みで起きます（`net-udp`で検証）。
  socket indexは呼び出しprocessが所有し、終了時に自動回収されます（`net-sock`で検証）。
  TCPは対象外です。

受け入れ条件は`cargo xtask test irq-uart`、`irq-blk`、`net-ping`、`net-udp`の四経路です。
`net-ping`はQEMUのuser network経由でgateway 10.0.2.2へICMP echoを送りreplyを受け取ることを、`net-udp`はguestのUDP datagramがhostのecho socketと往復することを観測します。

### 段階4：multi-hart

最後に、単一hartの前提を外します。
この段階は、heapの「割り込み内で割り当てない」規約と、`ProcessTable`のlockなし設計の両方を置き換えるため、前の段階が落ち着いてから着手します。

- **hartごとのboot**（一部完了）：SBI HSMで二つ目以降のhartを起動します。`smp-boot`が`-smp 2`でsecondary hartのS-mode到達を、`smp-timer`がsecondary側のper-hart trap handlerでのtimer割り込み受信を検証済みです。hartごとのtrap stackとscheduler stateはこれからです。
- **spinlock**（一部完了）：`SpinLock` primitiveを導入し、UART出力をhart間で直列化済みです。heap、`ProcessTable`、fd table、pipe bufferへの適用はこれからです。
- **IPI**（一部完了）：process終了やpipeの起床を別hartへ通知します。`smp-ipi`がboot hartからsecondary hartへのSBI IPI配送（supervisor software割り込みの受信と`sip.SSIP`クリア）を検証済みですが、scheduler経路への接続はこれからです。
- **QEMUの`-smp 2`**：release gateのQEMU経路を`-smp 1`と`-smp 2`の両方で実行します（現状は`smp-boot`・`smp-timer`・`smp-ipi`のみ`-smp 2`）。

受け入れ条件は、全既存経路が`-smp 2`でも同じframe列を出すことと、`cargo xtask test smp-sched`が二つのhartで同時に進むprocessを観測することです。

### 対象外のまま残すもの

OCI image、Linux binary互換、multi-tenant isolation、TCP、Windows host、NEORV32以外の実機driverは、この実装の目標に含めません。
NEORV32経路はkernel shellとread-only FAT32までを維持し、段階1以降の機能はQEMU `virt`だけを対象とします。

[U-modeの学習章](../guide/15-user-mode.md)と[payloadの学習章](../guide/16-boot-payload.md)は実行と回収の境界を説明します。
[Rust guestの学習章](../guide/17-rust-guest.md)はユーザープログラムのbuild、MiniBundle生成、QEMU実行を説明します。
