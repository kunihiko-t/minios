# virtio-blk + FAT32 on RV64 QEMU

## 背景

read-only storage経路はRV32 NEORV32で実績済み（GPIO-SPI SD + FAT32
`ls`/`cat`）。`storage::fat32`は`SectorReader` traitへgenericであり、
transportを差し替えればRV64でも動く。QEMU `virt`は`-device
virtio-blk-device`をvirtio-mmio slotへ配置し、DTBへ`virtio,mmio` nodeを
載せる。FDT発見・heap-backed queue memory・pollベースの1 queueで、割り込み
処理（PLIC未実装）を持ち込まずにread-only block経路を足す。

## 設計

### lib: `kernel/src/fdt.rs`

- `NodeScan`へ`is_virtio_mmio`を追加（`compatible="virtio,mmio"`を含む
  node）。`MachineSpec`へ`virtio_mmio: [usize; VIRTIO_MMIO_MAX]`と
  `virtio_mmio_count`を追加し、発見順にregベースを収める。
  QEMU virtのslotは8個で上限も8。

### lib: `kernel/src/vm/kernel.rs`

- `KernelMapPlan::with_device_pages(&[usize])`を追加。各baseは4 KiB整列・
  managed RAM外・既存mappingと非重複を検証し、S-mode R+Wの借用mappingと
  して列挙する（`with_payload_pages`と同じ形状）。

### lib: `kernel/src/storage/virtio_blk.rs`（新規、`not(riscv32)`）

- `pub trait Mmio { read32/write32 }`でregister fileを抽象化し、host testへ
  fake deviceを差せるようにする。
- queue/request bufferは`#[repr(C, align(4096))]`な`Box`ed 4 KiB領域1個へ
  まとめる（desc 8×16 B + avail + used + blk header 16 B + status 1 B +
  data 512 B）。kernelはmanaged RAMを恒等mapするためVA==PAでregisterへ渡せる。
- 初期化はmodern MMIO v2 handshake: reset→ACK→DRIVER→feature negotiation
  （VERSION_1のみ受理）→FEATURES_OK→queue登録→DRIVER_OK。
  DeviceID!=2（block以外）はreject。
- `read_sector(lba)`: 3 desc chain（header OUT、data IN、status IN）を組み、
  avail idxを上げてnotifyし、used idxをbounded pollする。status byte検査まで
  含める。deviceからの書き込みはDMA完了後に読むため、avail更新前に
  `fence(Release)`、used観測後に`fence(Acquire)`を挟む。
- `SectorReader`を実装し`Fat32`へそのまま繋ぐ。
- エラー: `BadMagic`/`UnsupportedVersion`/`NotBlockDevice`/
  `FeatureRejected`/`QueueTooSmall`/`Timeout`/`DeviceStatus(u8)`。

### bin: `kernel/src/main.rs`

- `drivers::virtio_mmio`（新規、riscv64）: `Mmio`のvolatile実装。
- `KernelMapPlan`へ`with_device_pages(&machine.virtio_mmio[..count])`を適用。
- `qemu-test-virtio` feature: slotを順にprobeしblock deviceを見つけて
  `VirtioBlk::init`→`Fat32::mount`→`read_root_file("HELLO.TXT")`の内容を
  `[MINIOS_TEST] virtio: ok <content>`として出す。

### xtask

- `xtask/src/disk.rs`（新規）: deterministicなsuperfloppy FAT32 image生成
  （parserが要求する`data_cluster_count >= 65525`を満たすため
  spc=1・約36 MiB）。root directoryに`HELLO.TXT`と既知内容を置く。
- `qemu.rs`へ`Virtio` test kindを追加: imageをtemp dirへ生成し、
  `-drive file=...,format=raw,if=none,id=blk0 -device
  virtio-blk-device,drive=blk0`で起動、marker検証。
- `cli.rs`/`lib.rs`へ`virtio` phaseを配線（全phase数 34→35）。

## 不変条件

- queue memoryは`Box`所有でdevice生存中は移動しない（heapは追記のみで
  relocしない）。
- MMIO registerへのwriteはvolatile経由のみ。
- host testのfake deviceはnotify時に同じpointerを解釈してqueueを消化する。
- RV32経路へ影響しない（`sd`/`fat32`のrv32 gateは維持、`virtio_blk`は
  `not(riscv32)`）。

## QEMU上の確認結果

- QEMU `virt`は常時8個のvirtio-mmio transport (`0x1000_1000..0x1000_8000`)
  を生成し、DTBにも8 nodeが現れる。空slotもMagicValueを返すため、
  probeは`DeviceID==2`まで確認しないとblockの有無を区別できない。
- `-device virtio-blk-device`は既定でPCI側へ接続されるため、
  `bus=virtio-mmio-bus.0`を明示する必要がある。
- QEMU `virt`のvirtio-mmio transportは既定で`force-legacy=true`
  (Version=1のlegacy interface) になるため、modern v2を使うには
  `-global virtio-mmio.force-legacy=false`が必須。

## 検証

- host test: fake MMIO deviceでinit handshake、desc chain、used ring、
  エラー経路（bad magic・非block・FEATURES_OK拒否・timeout・status!=0）を
  網羅。FAT32 image builder自体は既存host testのfixtureと同じgeometryで
  検証する。
- QEMU: `cargo xtask test virtio`が実deviceへmount+readまで通す。
- 全phaseゲート + RV32 build。

## 非目標

- 書き込み・flush・multi-queue・interrupt駆動・PCI virtio。
- shellへの`ls`/`cat`統合（RV64 shellは存在しない）。
- 実機SD経路の変更。
