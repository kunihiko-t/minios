# 発展ロードマップ

この文書は、実装済みの範囲、次の受け入れ単位、その後の方向を区別します。
現在のrelease gateは、host testの後に12個のQEMU経路を含む24段階を実行します。

## 実装済み

物理フレームアロケーターは、boot payload予約領域を除く`align_up(__kernel_end, 0x1000)..0x8780_0000`を管理します。

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

## 次

Device TreeはRAM、UART、timebaseの固定値をmachine記述へ置き換えるときに導入します。
汎用heapは固定容量の単一address spaceを越え、可変個のkernel objectとprocessを管理するときに導入します。
その後にscheduler、VirtIO block、file system、network、multi-hart、実機対応を進めます。

OCI image、Linux binary互換、multi-tenant isolationはこの実装の目標に含めません。

[U-modeの学習章](../guide/15-user-mode.md)と[payloadの学習章](../guide/16-boot-payload.md)は実行と回収の境界を説明します。
