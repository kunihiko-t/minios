//! FAT32 parser。読み取りに加え、RV32 build以外では`SectorWriter`を備えた
//! backend上でfile/directoryの作成・書き込み・削除・renameを行う。BPBとFATの
//! 値はaddress計算の前にすべて検証し、checked演算とdata cluster数上限で
//! volume外参照と無限loopを防ぐ。

mod parse;
mod read;
#[cfg(test)]
mod tests;
#[cfg(not(target_arch = "riscv32"))]
mod write;

#[derive(Debug, PartialEq, Eq)]
pub enum FatError<E> {
    Read(E),
    Unsupported,
    InvalidFilesystem,
    NotFound,
    IsDirectory,
    NotDirectory,
    InvalidName,
    CorruptChain,
    /// free clusterやdir slotが枯渇した。
    NoSpace,
    /// write offsetがfile sizeを越えた（穴あきwriteは非対応）。
    InvalidOffset,
    /// `rename`でdirectoryを自身または子孫の中へ移そうとした。
    MoveIntoItself,
    /// `create_dir`の対象名が既存のentryと衝突した（file/dirを問わない）。
    Exists,
    /// `remove_dir`の対象が`.`/`..`以外のentryを残している。
    NotEmpty,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirEntry {
    /// 表示名。直前のLFN record列がchecksum一致した場合は長い名前、
    /// それ以外は8.3の`BASE.EXT`形。
    name: [u8; 255],
    name_len: u8,
    /// alias照合用のraw 8.3名（space padding済み・大文字）。
    short_name: [u8; 11],
    first_cluster: u32,
    size: u32,
    directory: bool,
}

impl DirEntry {
    pub fn name(&self) -> &str {
        // parse時に0x21..=0x7eと'.'だけへ正規化済みのため、UTF-8変換は失敗しない。
        core::str::from_utf8(&self.name[..self.name_len as usize]).expect("validated FAT name")
    }

    pub const fn size(&self) -> u32 {
        self.size
    }

    pub const fn is_directory(&self) -> bool {
        self.directory
    }
}

pub struct Fat32<R> {
    reader: R,
    partition_start: u32,
    volume_sectors: u32,
    fat_start: u32,
    fat_sectors: u32,
    /// FATの面数（1か2）。readは第1 FATだけを使い、writeは全面へ写す。
    #[cfg(not(target_arch = "riscv32"))]
    fat_count: u32,
    /// BPBが指すFSInfo sectorの絶対LBA。0は「FSInfo無し」。
    #[cfg(not(target_arch = "riscv32"))]
    fsinfo_lba: u32,
    /// FSInfoをこのsessionで既に「不明」へ揃えたか。最初のFAT更新でのみ
    /// sectorへ触れるためのflag。
    #[cfg(not(target_arch = "riscv32"))]
    fsinfo_marked: bool,
    data_start: u32,
    sectors_per_cluster: u8,
    root_cluster: u32,
    data_cluster_count: u32,
}

/// `Fat32::open_file`/`create_file`が返すfile位置の記述子。fd tableへ
/// そのまま格納できるplain dataであり、session自身への参照は持たない。
/// `dir_cluster`/`dir_index`はdir entryの物理位置で、sizeや
/// first_clusterのwrite-backに使う。
#[cfg(not(target_arch = "riscv32"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileDesc {
    first_cluster: u32,
    size: u32,
    dir_cluster: u32,
    dir_index: u32,
}

#[cfg(not(target_arch = "riscv32"))]
impl FileDesc {
    /// このfileのdir entryがある物理位置 `(dir_cluster, dir_index)`。
    /// `unlink`したentryを指すfdを失効させる照合へ使う。
    pub const fn dir_location(&self) -> (u32, u32) {
        (self.dir_cluster, self.dir_index)
    }

    /// dir entryのwrite-back先を更新する。cross-directory moveでentryが
    /// 別の物理位置へ移った際、開いているfdを新しい位置へ追従させる。
    pub fn set_dir_location(&mut self, dir_cluster: u32, dir_index: u32) {
        self.dir_cluster = dir_cluster;
        self.dir_index = dir_index;
    }

    /// 現在のfileサイズ。`write_range`が伸ばした分も反映される。
    /// `lseek`の`SEEK_END`基準に使う。
    pub const fn size(&self) -> u32 {
        self.size
    }

    /// host testが任意のdir位置を持つ記述子を組み立てるための構築子。
    /// fd失効の照合だけを検査する用途なので内容値は検証しない。
    #[cfg(test)]
    pub(crate) const fn for_test(dir_cluster: u32, dir_index: u32) -> Self {
        Self {
            first_cluster: 0,
            size: 0,
            dir_cluster,
            dir_index,
        }
    }
}

/// `Fat32::stat`が返すfile metadata。`size`とdirectory bitだけを持ち、
/// recordの物理位置は返さない。`stat`はside effectを持たないため
/// write-back先は不要である。
#[cfg(not(target_arch = "riscv32"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileStat {
    /// fileのbyteサイズ。directoryはFAT32のdir entry規約に従い0。
    pub size: u32,
    /// directoryなら`true`。
    pub directory: bool,
}

/// directory走査中のentryの物理位置。`dir_head`はそのentryを含む
/// directory chainの先頭cluster、`cluster`はrecordを含むcluster、
/// `index`はそのcluster内のrecord番号（`sector * 16 + record`）。
/// RV32はlocをwrite-backへ使わないため、fieldの未読は許容する。
#[cfg_attr(target_arch = "riscv32", allow(dead_code))]
#[derive(Clone, Copy)]
struct DirLoc {
    dir_head: u32,
    cluster: u32,
    index: u32,
}

/// `rename`の結果。`moved`はsource entryが別directoryへ物理移動した
/// 場合の`(旧loc, 新loc)`、`replaced`は置換で消えたtarget entryの
/// loc。control層がfile fdの追従（write-back先更新）と失効へ使う。
/// 同dir内のin-place renameはentryが動かないため`moved`は`None`。
#[cfg(not(target_arch = "riscv32"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RenameOutcome {
    pub moved: Option<((u32, u32), (u32, u32))>,
    pub replaced: Option<(u32, u32)>,
}

struct Geometry {
    partition_start: u32,
    volume_sectors: u32,
    fat_start: u32,
    fat_sectors: u32,
    /// FATの面数（1か2）。readは第1 FATだけを使い、writeは全面へ写す。
    #[cfg(not(target_arch = "riscv32"))]
    fat_count: u32,
    /// BPBが指すFSInfo sectorの絶対LBA。0は「FSInfo無し」。
    #[cfg(not(target_arch = "riscv32"))]
    fsinfo_lba: u32,
    data_start: u32,
    sectors_per_cluster: u8,
    root_cluster: u32,
    data_cluster_count: u32,
}

#[derive(Clone, Copy)]
enum ParseError {
    Unsupported,
    InvalidFilesystem,
}

/// FAT chainの終端印。`0x0fff_fff8`以上はEOCとして扱われる。
#[cfg(not(target_arch = "riscv32"))]
const FAT_EOC: u32 = 0x0fff_ffff;

/// FSInfo sectorの先頭signature `RRaA`。
#[cfg(not(target_arch = "riscv32"))]
const FSINFO_LEAD_SIG: u32 = 0x4161_5252;

/// FSInfo sectorの構造signature `rrAa`（offset 484）。
#[cfg(not(target_arch = "riscv32"))]
const FSINFO_STRUCT_SIG: u32 = 0x6141_7272;

/// FSInfo sectorの末尾signature（offset 508）。
#[cfg(not(target_arch = "riscv32"))]
const FSINFO_TRAIL_SIG: u32 = 0xaa55_0000;

/// FSInfoのfree cluster数・next候補に使う「不明」値。spec上の合法値で、
/// 保持しない代わりに古い実測値を残さないための印。
#[cfg(not(target_arch = "riscv32"))]
const FSINFO_UNKNOWN: u32 = 0xffff_ffff;

/// file作成時に使うarchive attribute。
#[cfg(not(target_arch = "riscv32"))]
const DIR_ATTR_FILE: u8 = 0x20;

/// directory entryに使うattribute。
#[cfg(not(target_arch = "riscv32"))]
const DIR_ATTR_DIR: u8 = 0x10;

/// `.`entryのraw 8.3名。
#[cfg(not(target_arch = "riscv32"))]
const DOT_SHORT_NAME: [u8; 11] = *b".          ";

/// `..`entryのraw 8.3名。
#[cfg(not(target_arch = "riscv32"))]
const DOT_DOT_SHORT_NAME: [u8; 11] = *b"..         ";

/// dir chain内のrecord位置 `(cluster, index)`。`index`はcluster内の
/// record番号で、sectorとbyte offsetは`index / 16`と`index % 16 * 32`
/// から得る。
#[cfg(not(target_arch = "riscv32"))]
type DirRecordPos = (u32, u32);

#[cfg(test)]
impl<R> Fat32<R> {
    pub fn partition_start(&self) -> u32 {
        self.partition_start
    }

    pub fn root_cluster(&self) -> u32 {
        self.root_cluster
    }
}
