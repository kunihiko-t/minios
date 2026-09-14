# NEORV32 read-only FAT32設計

## 状態

この仕様は、Tang Nano 20K上でのMiniOS最初のstorage節目を定義します。
MiniOSは既存imageからbootし続けます。boot後、RV32 shellは基板内蔵のmicroSD slot上のfileを参照できます。

## 目標

RV32 shell commandを二つ追加します。

- `ls`はFAT32 root directoryのentryを一覧表示します。
- `cat NAME.EXT`はroot directory直下の通常file一つをconsoleへ出力します。

実装は既存UART shellを保ち、`kernel/linker_neorv32.ld`が定義するNEORV32内蔵memoryに収めます。

## 構成

実装はMiniOSリポジトリ内の小さい三層です。

1. NEORV32のGPIO-SPI busがTang Nano 20KのmicroSD pinを駆動します。
2. SD protocol moduleがSDHCまたはSDXC cardを初期化し、CMD17とCRC16検証で512 byte sectorを読み取ります。
3. read-only FAT32 moduleが、host testでも差し替え可能なsector reader interface越しにpartition、boot sector、FAT、root directoryをparseします。

shellは一つのSD/FAT32 sessionを所有し、同期的に呼び出します。
heap、background task、cache、VFS、third-party crateは導入しません。

FPGAリポジトリはHDL、pin制約、bitstreamを担い続けます。
MiniOSはFPGAリポジトリのsource fileやscriptに依存しません。
既存FPGAの`sd_probe`は独立のhardware診断のまま残し、その実証済みprotocol動作をMiniOSへ移植してMiniOSのtestで覆います。

## hardware契約

実証済みGPIO mappingは、この節目では固定です。

| GPIO | SD signal | Tang Nano 20K pin |
| --- | --- | --- |
| 0 | CLK | 83 |
| 1 | CMD/MOSI | 82 |
| 2 | 論理正のselect。FPGAが反転してDAT3/CSへ | 81 |
| 3 | DAT0/MISO | 84 |

SPI modeではDAT1とDAT2を使いません。
reset時とすべてのerror pathは、cardをdeselectし、CLKをlow、MOSIをhighのまま残します。
初期化は96 MHz CPU clockで375 kHz以下で動作します。

SD層はSDHCとSDXCのblock addressingだけを扱います。
送るcommandはCMD0、CMD8、CMD55/ACMD41、CMD58、CMD17だけです。
mediaへの書込、消去、format commandは実装しません。

## 配置

論理sectorはすべて正確に512 byteです。FAT32 volumeは次のどちらかで見つけます。

1. LBA 0が有効なFAT32 boot sectorなら、superfloppyとしてmountします。
2. そうでなければ`55aa`署名付きMBRを要求し、四つのprimary entryを順に走査して、最初の非空`0x0b`または`0x0c` partitionをmountします。

GPT、拡張partition、protective MBR、FAT12、FAT16、exFATは対象外です。
64 GB級cardは、既存内容の確認後にMiniOSの外でFAT32にformatし直す必要があります。

FAT32 parserはaddress計算の前にすべての値を検証します。
最低限、512 byte sector、FAT32範囲内の2のべき乗sectors-per-cluster、非0のreserved sector数、一つまたは二つのFAT、非0のFAT32 FAT size、0のFAT16 root entry数とFAT size、2以上のroot cluster、選択volume内に収まるdataとFAT範囲を要求します。
演算はchecked演算です。不正または循環するcluster chainはvolume外読み取りの代わりに失敗します。走査は算出したdata cluster数で打ち切ります。

## file動作

初版はFAT32 root directoryと8.3 short nameだけを扱います。
削除済みentry、long file name entry、volume labelは無視します。
`ls`は通常fileまたはdirectoryを正規化short nameで表示し、通常fileにはbyte長も付けます。

`cat`はroot内でASCII大文字小文字不問のshort name lookupを行います。
pathなし8.3名を正確に一つ受け付け、directoryを拒否し、directory entryの宣言file sizeを上限に単一512 byte bufferでstream出力します。
空fileはdata clusterを読まずに成功します。
subdirectory走査とlong name lookupは対象外です。

## interface

SD protocolは既存のtest可能なbyte bus境界を保ちます。

```rust
trait Bus {
    fn select(&mut self, active: bool) -> Result<(), SdError>;
    fn transfer(&mut self, tx: u8) -> Result<u8, SdError>;
    fn delay_ms(&mut self, milliseconds: u32);
}
```

FAT32 parserは一つのsector境界に依存します。

```rust
trait SectorReader {
    type Error;
    fn read_sector(
        &mut self,
        lba: u32,
        destination: &mut [u8; 512],
    ) -> Result<(), Self::Error>;
}
```

本番実装はNEORV32 GPIO SD readerです。
traitはhost testをMMIOから隔離するためだけにあり、汎用deviceやVFSの枠組みではありません。

## errorとconsole出力

SDとFAT32のerrorは内部では型付きです。
shellは`sd:`接頭辞付きの短い安定messageへ写像します。必須の利用者可視caseは次のとおりです。

- card初期化またはsector読み取りの失敗。
- 未対応または不正なpartitionとfilesystem。
- root entryなし。
- 要求entryがdirectory。
- 不正な`cat`引数。

error時は`minios> ` promptへ戻り、chip selectを解放します。
errorでkernelをpanicさせず、cardへ書込commandを送りません。

## test

開発はred-green-refactorで進めます。host testは次を覆います。

- 実証済みSD初期化frame、block address、CRC16、timeout、chip select cleanup動作。
- superfloppy検出と順序どおりのMBR primary partition選択。
- 未対応配置と不正または桁あふれBPB値の拒否。
- sectorとcluster境界をまたぐroot directory反復。
- 削除済み、LFN、volume labelのfiltering。
- 8.3名の正規化と大文字小文字不問lookup。
- 空、一 sector、複数 clusterのfile streaming。
- 不正、範囲外、循環のcluster chain。
- RV64 command動作を変えない`ls`と`cat`のcommand parse。

build gateは既存host test群に加え、locked RV32 release buildです。
成果物ELFは`kernel/linker_neorv32.ld`の実効32 KiB（32,768 byte）IMEMと不変の16,192 byte DMEM契約に収めます。
FPGA top levelとlinkerはどちらも32,768 byteを宣言します。
これはNEORV32が実装する2のべき乗address範囲であり、旧来の2のべき乗でない24,288 byte要求は32 KiBへ丸められます。
既存place-and-route報告は追加BSRAM costなしにこのaddress範囲を示します。
明示の32 KiB契約はhardware address範囲と一致し、DMEMは不変です。

RV32 buildは`.cargo/config.toml`で`opt-level=z`（riscv32im専用）のため、QEMU向けRV64とhostのbuildには影響しません。
実測`.text`は29,832 byteで残り2,936 byteです。超過時はlink時に失敗し、`cargo xtask check`のRV32 buildが予算を強制します。

hardware受け入れtestは既知root file入りFAT32 cardを使います。
既存MiniOS imageの起動後に次を確認します。

1. `ls`が既知short nameと正しいsizeを表示する。
2. 大文字小文字どちらの綴りでも`cat`が正確な内容を表示する。
3. 存在しない名の`cat`が安定のnot-found errorを表示する。
4. 成功と失敗の後もshellが応答し続ける。

## 対象外

- SDからのMiniOS起動や他program起動。
- SD書込、消去、format、file作成や更新。
- exFAT、FAT12、FAT16、GPT、拡張partition。
- long file nameとsubdirectory走査。
- VFS、block cache、非同期I/O、hardware SPI再設計。
- FPGAとOpenOCDのbuild・load workflowの`xtask`化。storage pathのhardware動作後の別follow-upとする。
