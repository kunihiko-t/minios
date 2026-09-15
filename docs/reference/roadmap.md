# 発展ロードマップ

この文書は、実装済みの範囲、次の受け入れ単位、その後の方向を区別します。
現在のrelease gateは、RV64とRV32のクロスビルド、host test、18個のQEMU経路を含む33段階を実行します。

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
loaderは静的なRISC-V ELF64をallocation前に検証し、最大8個の`PT_LOAD`、2,048 user page、16 stack pageを固定容量の所有権表へmaterializeします。
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
各processの終了は`PROC_EXIT` frameで個別に通知され、slotを取り除いて全所有frameを回収します。
`cargo xtask test sched`は、busy-waitするprocessの出力の間に短命processの出力が挟まることと、切り替え回数の報告をQEMU上で確認します。
`read`は入力未到着ならprocessを`BlockedOnStdin`へ回してecallをやり直すため、stdin待ちの間も他processが進みます（`cargo xtask test sched-io`で検証）。
Stdin frameの受信は`StdinStaging`の再開可能なdecoderが担い、frame途中のbyte枯渇でもprocessは再びstdin待ちへ戻ります（`cargo xtask test sched-io-partial`で検証）。

## 次

汎用heapの動的拡張は実装済みで、ヒープはOOM時にframe allocatorの最上位pageを取り込んで下方へ成長します。
次はこの成長経路を使う可変個のkernel object管理を、固定容量の単一address spaceを越える段階で導入します。
その後にVirtIO block、file system、network、multi-hart、NEORV32以外の実機対応を進めます。

OCI image、Linux binary互換、multi-tenant isolationはこの実装の目標に含めません。

[U-modeの学習章](../guide/15-user-mode.md)と[payloadの学習章](../guide/16-boot-payload.md)は実行と回収の境界を説明します。
[Rust guestの学習章](../guide/17-rust-guest.md)はユーザープログラムのbuild、MiniBundle生成、QEMU実行を説明します。
