# 20. virtio-blkでディスクを読み書きする

## 学習目標

FDTが報告するvirtio-mmio slotから、block deviceを探し出す流れを説明できるようになります。
reset、ACK、DRIVER、FEATURES_OK、DRIVER_OKの順に進むvirtio v2の初期化手順と、`VIRTIO_F_VERSION_1`だけを要求するfeature negotiationを追います。
header、data、statusの3-desc chainでsectorを要求し、used ringをpollして完了を待つ仕組みを確認します。
QEMU検査用のFAT32 disk imageをxtaskが組み立て、QEMUへ接続する手順を確認します。

## 背景

第19章までのprocessは、boot payloadとしてRAMへ置かれたbyte列だけを入力にしていました。
fileを永続的に置く場所を持つには、kernelがblock deviceからsectorを読み書きできなければなりません。

QEMU `virt`は、`virtio,mmio`互換のtransport slotを複数持ちます。
各slotは固定のregister fileであり、deviceを接続したslotだけが有効な`DeviceID`を返します。
**virtqueue**は、driverがrequestを置くdescriptor tableとavail ring、deviceが完了を返すused ringの三つから成る共有メモリ上のqueueです。

MiniOSのvirtio-blk driverは、完了を割り込みではなくused ringのpollで待ちます。
PLIC driverを持たない現段階でもdiskを使えるようにするためで、割り込み完了への置き換えは[ロードマップ](../reference/roadmap.md)の割り込み駆動I/Oで扱います。

## 実装

### deviceの発見とregisterアクセス

[`kernel/src/fdt.rs`](../../kernel/src/fdt.rs)のparserは、`compatible`に`virtio,mmio`を含むnodeの`reg`先頭addressを発見順に`MachineSpec::virtio_mmio`へ記録します。
記録数の上限は`VIRTIO_MMIO_MAX`の8で、有効数は`virtio_mmio_count`が持ちます。

[`KernelMapPlan::with_device_pages`](../../kernel/src/vm/kernel.rs)は、記録された各baseから1 pageをS-mode R+Wでidentity mapします。
baseが0、page非整列、既存mappingとの重複のいずれかなら`InvalidDeviceRange`で拒否します。
deviceが接続されていないslotもmapしますが、後のprobeで弾かれるだけなので問題になりません。

[`MmioRegs`](../../kernel/src/drivers/virtio_mmio.rs)は、このbaseからのoffsetをvolatileな32-bit load/storeへ変換する薄い型です。
driver本体は[`Mmio`](../../kernel/src/storage/virtio_blk.rs) traitだけに依存するため、host testは`FakeDevice`という偽のregister fileで同じ初期化手順を検査できます。

### DMA領域の所有

[`VirtioRegion`](../../kernel/src/storage/virtio_blk.rs)は、descriptor table、avail ring、used ring、request header、status byte、512 byteのdata bufferを1 pageへ詰めた`#[repr(C, align(4096))]`の構造体です。
queueの長さは`QUEUE_LEN`の8に固定しています。

kernel側では[`VirtioRegionPage`](../../kernel/src/main.rs)がframe poolの1 pageを所有します。
`new`はpage全域を0で埋めてから`VirtioRegion`として貸し出し、`Drop`でframeをpoolへ返します。
managed RAMはidentity map済みのため、frameの物理addressがそのままdeviceへ渡すDMA addressになります。
`used.idx`の初期値が不定だと完了判定を誤るため、0埋めは省略できません。

### 初期化手順

[`VirtioBlk::init`](../../kernel/src/storage/virtio_blk.rs)は、最初に領域の4 KiB整列、`MagicValue`（`0x7472_6976`）、`Version`、`DeviceID`を検査します。
`Version`は2（modern interface）だけを受理し、legacy interfaceの1は`UnsupportedVersion(1)`で拒否します。
`DeviceID`が2（block）でなければ`NotBlockDevice`を返します。

検査を通ると、`Status`へ0を書いてresetし、読み戻しが0になるまで`POLL_LIMIT`回を上限に待ちます。
続いてACK、DRIVERを順に立てます。

feature negotiationでは、device featuresのhigh wordを読み、bit 0（全体のbit 32）の`VIRTIO_F_VERSION_1`が無ければ`FeatureRejected`を返します。
driver featuresにはlow wordへ0、high wordへ`VERSION_1`だけを書き、FEATURES_OKを立てます。
deviceが受理しなかった場合は`Status`の読み戻しからFEATURES_OKが消えるため、これも`FeatureRejected`として扱います。

queue 0の登録では、`QueueNumMax`が3未満なら`QueueTooSmall`を返します。
一回のrequestに3 descriptorを使うためです。
queue長は`QueueNumMax`と8の小さい方とし、desc、avail（driver area）、used（device area）の各addressを`offset_of!`で求めてlowとhighの32 bitに分けて書きます。
`fence(SeqCst)`で領域の内容を可視化してから`QueueReady`へ1を書き、config空間の先頭8 byteから容量（sector数）を読んでDRIVER_OKを立てます。

### sector requestとpolling

`submit_sector_request`は、一回のrequestを3-desc chainとしてqueueへ流します。
desc 0はdevice-readableな16 byteのrequest header、desc 1は512 byteのdata buffer、desc 2はdevice-writableな1 byteのstatusです。
読み取り（`BLK_REQUEST_IN`）ではdesc 1に`DESC_WRITE`を立て、書き込み（`BLK_REQUEST_OUT`）では外してdevice-readableにします。

statusには事前に`0xff`を書き、avail ringのslotへchain先頭のdescriptor番号0を置きます。
`fence(Release)`の後で`avail.idx`を進め、`fence(SeqCst)`の後で`QueueNotify`へ0を書きます。
この順序により、deviceはdescriptorとbufferの更新を見てからrequestを受け取ります。

完了待ちは`used.idx`が前回値から変わるまでのbusy loopで、`POLL_LIMIT`（5,000,000回）を超えると`Timeout`を返します。
完了後は`fence(Acquire)`でdeviceの書き込みを確定させ、used要素を読み、`InterruptStatus`の値を`InterruptACK`へ書き戻します。
used要素の`id`が0以外なら`BadUsedId`、status byteが0以外なら`DeviceStatus`を返します。
requestは常に一件ずつ発行して完了まで待つため、descriptor 0..2を毎回使い回せます。

[`SectorReader`](../../kernel/src/storage/mod.rs)の`read_sector`と`SectorWriter`の`write_sector`は、どちらも先に`lba`が容量未満かを確かめ、範囲外なら`BadLba`を返します。
`read_sector`は完了後にdata bufferを呼び出し側へコピーし、`write_sector`は送信前にbufferへコピーします。
`SectorWriter`の実装はRV32 build（NEORV32経路）には含まれません。

### probeとdisk image

QEMU検査の[`run_virtio_test`](../../kernel/src/main.rs)は、`virtio_mmio`の各slotで`MmioRegs`と新しい`VirtioRegionPage`を作り、`VirtioBlk::init`を試します。
`BadMagic`、`NotBlockDevice`、`UnsupportedVersion`は次のslotへ進み、失敗した領域はdropでpoolへ返ります。
それ以外のerrorは即座に検査失敗です。
blockが見つかると[`Fat32::mount`](../../kernel/src/storage/fat32.rs)へ渡し、root directoryに`HELLO.TXT`があること、内容が`hello from virtio\n`であることを照合します。

host側の[`xtask/src/disk.rs`](../../xtask/src/disk.rs)は、`DiskImage::create`で決定的なFAT32 imageを組み立てます。
partitionを持たないsuperfloppyで、`VOLUME_SECTORS`は70,000、1 cluster 1 sector、reserved sector 32、FAT 2面です。
`fat_sectors`はdata cluster数がFAT32の下限65,525以上になり、かつFATが1 sector余らない大きさへ収束させます。
rootには`HELLO.TXT`、`DOCS` directory、LFN付きの`Long File Name.txt`を置き、`DOCS`にはshellやspawn検査用のfileを置きます。
fileへは非0 byteを含むsectorだけをseekして書き、末尾を`set_len`で宣言するsparse fileにします。

[`qemu_command_with_disk`](../../xtask/src/qemu.rs)は、imageを`-drive file=...,format=raw,if=none,id=blk0`として登録し、`virtio-blk-device,drive=blk0,bus=virtio-mmio-bus.0`で最初のslotへ接続します。
`-global virtio-mmio.force-legacy=false`を付けるため、QEMUはlegacyの`Version`1ではなくmodernの2を報告します。

## 実行と確認

QEMUでのend-to-end検査は次のコマンドです。

```console
$ cargo xtask test virtio
...
[ok] traps
[ok] timer
[ok] memory
[MINIOS_TEST] virtio: block at 0x10001000 capacity=70000 sectors
[MINIOS_TEST] virtio: ok
phase 1/1 passed (elapsed: 0.398s)
summary: PASSED all 1 phases (elapsed: 0.398s)
```

harnessは`[MINIOS_TEST] virtio: ok`のmarkerと正常終了を検証します。
`block at`行は、最初のslot `0x10001000`でblock deviceが見つかり、config空間の容量が`VOLUME_SECTORS`と一致したことを示します。

初期化の拒否境界とpollingの上限は、`FakeDevice`を使うhost testで確認します。

```sh
cargo test -p minios-kernel --locked virtio_blk
```

disk imageがkernelのFAT32 parserで読めることは、`cargo test -p xtask --locked disk`で確認します。

## よくある失敗

- `VirtioRegionPage`を0埋めせずに使う：`used.idx`の初期値が前回値と食い違い、requestを出す前に完了と誤認します。
- `force-legacy=false`を外す：QEMUが`Version`1を返し、`run_virtio_test`はslotを読み飛ばして`no block device found`で失敗します。
- FEATURES_OKの読み戻しを省く：deviceがfeatureを拒否したまま初期化が進み、後続のqueue操作の意味が定まりません。
- `avail.idx`の更新や`QueueNotify`の前にfenceを置かない：deviceが更新前のdescriptorやstatusを読み、古いsectorや不正な要求を処理する可能性があります。
- 書き込みでもdesc 1に`DESC_WRITE`を立てる：data bufferがdevice-writableになり、deviceはbufferを書き込み元として読めません。
- `init`が失敗した領域を手で解放する：`VirtioRegionPage`の`Drop`がframeをpoolへ返すため、二重解放になります。

## 演習

[`kernel/src/storage/virtio_blk.rs`](../../kernel/src/storage/virtio_blk.rs)の`init`で、`max < 3`の検査を`max < 2`へ緩めてください。
`cargo test -p minios-kernel --locked virtio_blk`で`init_rejects_a_queue_too_small_for_a_request`が失敗することを確認し、元へ戻します。

host testの`FakeDevice::complete_request`は`BLK_REQUEST_IN`だけを想定しています。
`request_type`に応じてdata bufferの向きとdesc 1のflagを切り替え、`write_sector`の後に`read_sector`で同じbyte列が返るtestを追加してください。

[`xtask/src/disk.rs`](../../xtask/src/disk.rs)の`HELLO_TXT`を変更し、`cargo xtask test virtio`が`HELLO.TXT content mismatch`で失敗することを確かめてください。
kernel側の`EXPECTED`と二か所で同じ内容を持つ理由を考えます。

## 次の章

[第21章「FAT32の読み書き」](21-fat32.md)では、この章の`SectorReader`と`SectorWriter`の上でFAT32 volumeをmountし、directoryとfileを扱う仕組みを追います。
