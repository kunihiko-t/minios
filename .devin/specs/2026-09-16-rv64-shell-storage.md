# RV64 shell: virtio-blk経由の`ls`/`cat`

## 背景

virtio-blk + FAT32の検証経路は`qemu-test-virtio`で実証済みだが、interactive
なRV64 shellにはstorage commandがない。RV32 shellが持つ`ls`/`cat`と同等の
経路をvirtio-blk上に張り、`cargo xtask run`とQEMU shell testで人間・CI
双方が触れるようにする。

## 設計

### lib側の変更なし

`SectorReader`/`Fat32`/`VirtioBlk`は全てgenericで再利用する。
`storage`モジュールは全targetでcompile済み。

### bin側

- `drivers::virtio_mmio`と`VirtioRegionPage`を`qemu-test-virtio`専用から
  `riscv64`全域へ広げる (shellが恒久利用するため)。
- `mod storage`に`riscv64`版を追加 (`fat32`/`virtio_blk`/`SectorReader`の
  re-export)。
- `shell/mod.rs`:
  - `Rv64Storage = Fat32<VirtioBlk<MmioRegs, VirtioRegionPage>>`
  - `mount_storage`のRV64版: `machine::spec()`のslotを順にprobeし、
    block deviceを`VirtioBlk::init`して`Fat32::mount`へ渡す。
    sessionは`Option`でlazyに保持する (RV32と同じ契約)。
  - `list_root`/`cat`の出力本体を`Fat32<R>` genericのhelperへ共通化し、
    error messageはarch別 (`sd:`/`virtio:`) に分ける。
  - `Command::Ls`/`Cat`をrv64へ開放 (`command.rs`のcfg変更)。
  - `help`出力に`ls`/`cat`を追加。

### xtask

- `cargo xtask test shell`と`cargo xtask run`にdisk imageを接続し、
  scriptへ`ls`/`cat HELLO.TXT`を追加してtranscript検証する。
- `run`でdiskを常時接続すると対話中に`ls`/`cat`がそのまま使える。

## 不変条件

- `VirtioRegionPage`のframeは`VirtioBlk`の生存中は専有、dropでpoolへ戻る。
- `ls`/`cat`失敗時もshellは落ちない (RV32と同じ)。
- RV32のSD経路・error messageは不変。

## 検証

- host: `command.rs`のparse test、`finish_cat_output`の既存test。
- QEMU: `test shell`が`ls`で`HELLO.TXT`を列挙し`cat HELLO.TXT`の
  内容を印字するまでをtranscript検証。
- 全35 phase + RV32 build。
