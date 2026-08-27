# 発展ロードマップ

この文書は、実装済みの範囲、次の受け入れ単位、その後の方向を区別します。
現在のrelease gateは、既存経路を保ったまま19段階を実行します。

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

## 次

次の受け入れ単位は、inactiveな`LoadedImage`で最小のU-modeプログラムを実行し、`write`と`exit`でS-modeへ戻すことです。

1. **U-mode遷移**：`LoadedImage`のentryとuser stack上端から初期registerを作り、`sret`でU-modeへ入ります。
2. **user trap context**：U-modeの全整数register、`sepc`、`sstatus`をカーネルstackへ保存し、system call後に復元します。
3. **`write`**：user pointerの範囲とPTE権限を検査し、許可されたbyteだけをUARTへ出力します。
4. **`exit`**：終了codeを記録し、user address spaceとkernel stackを回収して、hostへ観測できる状態へ変換します。

### 受け入れ条件

- U-modeで実行を開始し、S-mode専用pageへのaccessがpage faultになる。
- U-modeの`ecall`がuser trap contextを失わずにS-modeのdispatchへ到達する。
- `write`が正常なbufferを出力し、範囲外、未写像、書き込み不可など規約外のuser pointerをtyped errorとして拒否する。
- `exit`が終了codeを保持し、`LoadedImage`とkernel stackの所有frameを回収する。
- 未知のsystem call numberと予期しないuser trapを診断して実行単位を停止する。
- 既存の19段階gateが引き続き成功し、新しいU-mode経路が専用markerまたは終了記録を持つ。

この段階ではMiniBundle parser、QEMU host runtime、`minictr` CLIをminiOSへ実装しません。

## その後

MiniBundle payload統合は、予約物理領域から得たELF byte sliceを既存のminiOS ELF loaderへ渡します。
ELFの検証、segment配置、ownership、rollbackを別のloaderとして再実装しません。

QEMU子process、UART入出力、終了status、timeout、cleanupはMiniContainerのhost runtimeが担当します。
`minictr run`は、そのruntimeとbundle storeを接続した後の段階です。

Device TreeはRAM、UART、timebaseの固定値をmachine記述へ置き換えるときに導入します。
汎用heapは固定容量の単一address spaceを越え、可変個のkernel objectとprocessを管理するときに導入します。
その後にscheduler、VirtIO block、file system、network、multi-hart、実機対応を進めます。

学習上の理由と演習は[第12章](../guide/12-next-steps.md)、現在のSv39とELFの境界は[第13章](../guide/13-sv39.md)と[第14章](../guide/14-elf-loading.md)を参照してください。
