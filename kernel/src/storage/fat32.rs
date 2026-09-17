use super::SectorReader;
#[cfg(not(target_arch = "riscv32"))]
use super::SectorWriter;

// read-only FAT32 parser。BPBとFATの値はaddress計算の前にすべて検証し、
// checked演算とdata cluster数上限でvolume外参照と無限loopを防ぐ。
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
    /// `rename`の新旧pathが異なるdirectoryを指した（dir間移動は非対応）。
    CrossDirectory,
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

struct Geometry {
    partition_start: u32,
    volume_sectors: u32,
    fat_start: u32,
    fat_sectors: u32,
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

impl<R: SectorReader> Fat32<R> {
    pub fn mount(mut reader: R) -> Result<Self, FatError<R::Error>> {
        let mut sector = [0; 512];
        reader.read_sector(0, &mut sector).map_err(FatError::Read)?;

        let first_parse = parse_boot_sector(&sector, 0, None);
        if let Ok(geometry) = first_parse {
            return Ok(Self::from_geometry(reader, geometry));
        }
        let first_error = first_parse.err().unwrap();

        if sector[510..512] != [0x55, 0xaa] {
            return Err(first_error.into_fat_error());
        }

        let mut selected = None;
        let mut saw_nonempty = false;
        for index in 0..4 {
            let offset = 446 + 16 * index;
            let partition_type = sector[offset + 4];
            if partition_type == 0 {
                continue;
            }
            saw_nonempty = true;
            if partition_type == 0xee || is_extended_partition(partition_type) {
                return Err(FatError::Unsupported);
            }
            if partition_type != 0x0b && partition_type != 0x0c {
                continue;
            }

            let start = read_u32(&sector, offset + 8);
            let length = read_u32(&sector, offset + 12);
            if start == 0 || length == 0 {
                return Err(FatError::InvalidFilesystem);
            }
            start
                .checked_add(length)
                .ok_or(FatError::InvalidFilesystem)?;
            selected = Some((start, length));
            break;
        }

        let (partition_start, partition_sectors) = match selected {
            Some(selected) => selected,
            None if saw_nonempty => return Err(FatError::Unsupported),
            None => return Err(first_error.into_fat_error()),
        };

        reader
            .read_sector(partition_start, &mut sector)
            .map_err(FatError::Read)?;
        let geometry = parse_boot_sector(&sector, partition_start, Some(partition_sectors))
            .map_err(ParseError::into_fat_error)?;
        Ok(Self::from_geometry(reader, geometry))
    }

    fn from_geometry(reader: R, geometry: Geometry) -> Self {
        Self {
            reader,
            partition_start: geometry.partition_start,
            volume_sectors: geometry.volume_sectors,
            fat_start: geometry.fat_start,
            fat_sectors: geometry.fat_sectors,
            data_start: geometry.data_start,
            sectors_per_cluster: geometry.sectors_per_cluster,
            root_cluster: geometry.root_cluster,
            data_cluster_count: geometry.data_cluster_count,
        }
    }

    /// root directoryのentryを順に報告する。RV32のshellはこのroot専用
    /// 経路だけを使い、path解決の機構をIMEMへ載せない。
    pub fn for_each_root_entry(
        &mut self,
        mut visit: impl FnMut(&DirEntry),
    ) -> Result<(), FatError<R::Error>> {
        let mut scratch = [0; 512];
        self.walk_dir(self.root_cluster, &mut scratch, |entry, _loc| {
            visit(entry);
            false
        })
    }

    /// rootの`name`を先頭からstreamする。`/`を含む名前は8.3として
    /// 不正なため`InvalidName`を返す。
    pub fn read_root_file(
        &mut self,
        name: &str,
        write: impl FnMut(&[u8]),
    ) -> Result<(), FatError<R::Error>> {
        normalize_input_name(name).ok_or(FatError::InvalidName)?;
        let mut scratch = [0; 512];
        let (entry, _loc) = self
            .find_entry_in(self.root_cluster, name, &mut scratch)?
            .ok_or(FatError::NotFound)?;
        if entry.directory {
            return Err(FatError::IsDirectory);
        }
        self.stream_file(&entry, &mut scratch, write)
    }

    /// `path`が参照するdirectoryのentryを順に報告する。`""`はrootを意味し、
    /// それ以外は`/`区切りの要素をrootから順に解決する。
    /// path解決はRV32のIMEM予算に乗らないため、このAPIはRV32以外で
    /// だけ提供する。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn for_each_entry(
        &mut self,
        path: &str,
        mut visit: impl FnMut(&DirEntry),
    ) -> Result<(), FatError<R::Error>> {
        let mut scratch = [0; 512];
        let dir_cluster = if path.is_empty() {
            self.root_cluster
        } else {
            let (entry, _loc) = self.resolve_path(path, &mut scratch)?;
            if !entry.directory {
                return Err(FatError::NotDirectory);
            }
            entry.first_cluster
        };
        self.walk_dir(dir_cluster, &mut scratch, |entry, _loc| {
            visit(entry);
            false
        })
    }

    /// `path`が参照するfileを先頭からstreamする。directoryを指すpathは
    /// `IsDirectory`、fileの途中に潜るpathは`NotDirectory`を返す。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn read_file(
        &mut self,
        path: &str,
        write: impl FnMut(&[u8]),
    ) -> Result<(), FatError<R::Error>> {
        let mut scratch = [0; 512];
        let (entry, _loc) = self.resolve_path(path, &mut scratch)?;
        if entry.directory {
            return Err(FatError::IsDirectory);
        }
        self.stream_file(&entry, &mut scratch, write)
    }

    /// `path`を解決してfileを開き、`read_range`へ渡す記述子を返す。
    /// directoryを指すpathは`IsDirectory`を返す。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn open_file(&mut self, path: &str) -> Result<FileDesc, FatError<R::Error>> {
        let mut scratch = [0; 512];
        let (entry, loc) = self.resolve_path(path, &mut scratch)?;
        if entry.directory {
            return Err(FatError::IsDirectory);
        }
        Ok(FileDesc {
            first_cluster: entry.first_cluster,
            size: entry.size,
            dir_cluster: loc.cluster,
            dir_index: loc.index,
        })
    }

    /// 開いたfileの`offset` byte目から`output`へ最大`output.len()` byte
    /// 読み、書いたbyte数を返す。`offset >= size`ならEOFとして0を返す。
    /// 読み切るまでcluster chainを前方へだけたどる。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn read_range(
        &mut self,
        file: &FileDesc,
        offset: u64,
        output: &mut [u8],
    ) -> Result<usize, FatError<R::Error>> {
        if offset >= file.size as u64 || output.is_empty() {
            return Ok(0);
        }
        if !self.is_data_cluster(file.first_cluster) {
            return Err(FatError::CorruptChain);
        }
        let mut scratch = [0; 512];
        let bytes_per_cluster = self.sectors_per_cluster as u64 * 512;
        // offsetが指すclusterまでchainをたどる。有効なfileでskipがchain長を
        // 超えることはないが、破損したchainは`CorruptChain`として報告する。
        let mut skip = offset / bytes_per_cluster;
        if skip >= self.data_cluster_count as u64 {
            return Err(FatError::CorruptChain);
        }
        let mut cluster = file.first_cluster;
        while skip != 0 {
            cluster = match self.next_cluster(cluster, &mut scratch)? {
                Some(next) => next,
                None => return Err(FatError::CorruptChain),
            };
            skip -= 1;
        }

        let mut intra = (offset % bytes_per_cluster) as usize;
        let mut remaining = (file.size as u64 - offset).min(output.len() as u64);
        let mut written = 0usize;
        let mut clusters_read = 0;
        while remaining != 0 {
            if !self.is_data_cluster(cluster) || clusters_read >= self.data_cluster_count {
                return Err(FatError::CorruptChain);
            }
            let mut sector = (intra / 512) as u32;
            intra %= 512;
            while sector < self.sectors_per_cluster.into() && remaining != 0 {
                let lba = self.data_sector_lba(cluster, sector)?;
                self.reader
                    .read_sector(lba, &mut scratch)
                    .map_err(FatError::Read)?;
                let take = (512 - intra).min(remaining as usize);
                output[written..written + take].copy_from_slice(&scratch[intra..intra + take]);
                written += take;
                remaining -= take as u64;
                intra = 0;
                sector += 1;
            }
            if remaining == 0 {
                break;
            }
            clusters_read += 1;
            cluster = match self.next_cluster(cluster, &mut scratch)? {
                Some(next) => next,
                None => return Err(FatError::CorruptChain),
            };
        }
        Ok(written)
    }

    /// 解決済みのfile entryを先頭からstreamする。`size`が0のfileは
    /// data clusterを持たないため、ここで早期に完了させる。
    fn stream_file(
        &mut self,
        entry: &DirEntry,
        scratch: &mut [u8; 512],
        mut write: impl FnMut(&[u8]),
    ) -> Result<(), FatError<R::Error>> {
        if entry.size == 0 {
            return Ok(());
        }
        if !self.is_data_cluster(entry.first_cluster) {
            return Err(FatError::CorruptChain);
        }

        let mut remaining = entry.size;
        let mut cluster = entry.first_cluster;
        let mut clusters_read = 0;
        while remaining != 0 {
            if !self.is_data_cluster(cluster) || clusters_read >= self.data_cluster_count {
                return Err(FatError::CorruptChain);
            }

            let mut sector_in_cluster = 0;
            while sector_in_cluster < self.sectors_per_cluster && remaining != 0 {
                let lba = self.data_sector_lba(cluster, sector_in_cluster as u32)?;
                self.reader
                    .read_sector(lba, scratch)
                    .map_err(FatError::Read)?;
                let bytes = if remaining < 512 {
                    remaining as usize
                } else {
                    512
                };
                write(&scratch[..bytes]);
                remaining -= bytes as u32;
                sector_in_cluster += 1;
            }
            if remaining == 0 {
                return Ok(());
            }

            clusters_read += 1;
            cluster = match self.next_cluster(cluster, scratch)? {
                Some(next) => next,
                None => return Err(FatError::CorruptChain),
            };
        }
        Ok(())
    }

    /// `path`をrootから順に解決し、最終要素のentryと物理位置を返す。
    /// 途中の要素はdirectory必須、空要素と`.`/`..`は`InvalidName`として
    /// 拒否する。
    #[cfg(not(target_arch = "riscv32"))]
    fn resolve_path(
        &mut self,
        path: &str,
        scratch: &mut [u8; 512],
    ) -> Result<(DirEntry, DirLoc), FatError<R::Error>> {
        // diskへ触れる前に全要素を検査し、不正なpathは存在の有無に
        // 関係なく`InvalidName`で拒否する。要素は8.3に限らず、LFNが
        // 許す印字可能ASCIIを受理する。
        for part in path.split('/') {
            if !is_valid_component(part) {
                return Err(FatError::InvalidName);
            }
        }
        let mut dir_cluster = self.root_cluster;
        let mut parts = path.split('/').peekable();
        while let Some(part) = parts.next() {
            let (entry, loc) = self
                .find_entry_in(dir_cluster, part, scratch)?
                .ok_or(FatError::NotFound)?;
            if parts.peek().is_none() {
                return Ok((entry, loc));
            }
            if !entry.directory {
                return Err(FatError::NotDirectory);
            }
            dir_cluster = entry.first_cluster;
        }
        // `split`は空文字列でも1要素を返すため、ここへは到達しない。
        Err(FatError::InvalidName)
    }

    fn find_entry_in(
        &mut self,
        dir_cluster: u32,
        part: &str,
        scratch: &mut [u8; 512],
    ) -> Result<Option<(DirEntry, DirLoc)>, FatError<R::Error>> {
        let mut found = None;
        self.walk_dir(dir_cluster, scratch, |entry, loc| {
            if matches_component(entry, part) {
                found = Some((*entry, loc));
                true
            } else {
                false
            }
        })?;
        Ok(found)
    }

    fn walk_dir<F>(
        &mut self,
        dir_cluster: u32,
        scratch: &mut [u8; 512],
        mut visit: F,
    ) -> Result<(), FatError<R::Error>>
    where
        F: FnMut(&DirEntry, DirLoc) -> bool,
    {
        let mut cluster = dir_cluster;
        let mut clusters_read = 0;
        let mut lfn = PendingLfn::new();
        loop {
            if !self.is_data_cluster(cluster) || clusters_read >= self.data_cluster_count {
                return Err(FatError::CorruptChain);
            }

            let mut sector_in_cluster = 0;
            while sector_in_cluster < self.sectors_per_cluster {
                let lba = self.data_sector_lba(cluster, sector_in_cluster as u32)?;
                self.reader
                    .read_sector(lba, scratch)
                    .map_err(FatError::Read)?;
                for record in 0..16 {
                    let offset = record * 32;
                    if scratch[offset] == 0 {
                        return Ok(());
                    }
                    let record = &scratch[offset..offset + 32];
                    if record[11] == 0x0f {
                        // LFN chunk。直後のshort entryへ名を引き継ぐ。
                        lfn.push(record);
                        continue;
                    }
                    if record[0] == 0xe5 {
                        // deleted entryはLFNとshort entryの対応を切る。
                        lfn.reset();
                        continue;
                    }
                    if let Some(mut entry) = parse_dir_entry(record) {
                        lfn.apply(&mut entry, record);
                        let loc = DirLoc {
                            dir_head: dir_cluster,
                            cluster,
                            index: sector_in_cluster as u32 * 16 + offset as u32 / 32,
                        };
                        if visit(&entry, loc) {
                            return Ok(());
                        }
                    }
                }
                sector_in_cluster += 1;
            }

            clusters_read += 1;
            cluster = match self.next_cluster(cluster, scratch)? {
                Some(next) => next,
                None => return Ok(()),
            };
        }
    }

    fn is_data_cluster(&self, cluster: u32) -> bool {
        match self.data_cluster_count.checked_add(2) {
            Some(limit) => cluster >= 2 && cluster < limit,
            None => false,
        }
    }

    fn data_sector_lba(
        &self,
        cluster: u32,
        sector_in_cluster: u32,
    ) -> Result<u32, FatError<R::Error>> {
        if !self.is_data_cluster(cluster) || sector_in_cluster >= self.sectors_per_cluster as u32 {
            return Err(FatError::CorruptChain);
        }
        let cluster_offset = cluster
            .checked_sub(2)
            .and_then(|value| value.checked_mul(self.sectors_per_cluster as u32))
            .and_then(|value| value.checked_add(sector_in_cluster))
            .ok_or(FatError::InvalidFilesystem)?;
        let lba = self
            .data_start
            .checked_add(cluster_offset)
            .ok_or(FatError::InvalidFilesystem)?;
        let volume_end = self
            .partition_start
            .checked_add(self.volume_sectors)
            .ok_or(FatError::InvalidFilesystem)?;
        if lba >= volume_end {
            return Err(FatError::InvalidFilesystem);
        }
        Ok(lba)
    }

    fn next_cluster(
        &mut self,
        cluster: u32,
        scratch: &mut [u8; 512],
    ) -> Result<Option<u32>, FatError<R::Error>> {
        let entry_offset = cluster.checked_mul(4).ok_or(FatError::CorruptChain)?;
        let fat_sector_offset = entry_offset / 512;
        let byte_offset = (entry_offset % 512) as usize;
        let lba = self
            .fat_start
            .checked_add(fat_sector_offset)
            .ok_or(FatError::InvalidFilesystem)?;
        let fat_end = self
            .fat_start
            .checked_add(self.fat_sectors)
            .ok_or(FatError::InvalidFilesystem)?;
        let volume_end = self
            .partition_start
            .checked_add(self.volume_sectors)
            .ok_or(FatError::InvalidFilesystem)?;
        if lba >= fat_end || lba >= volume_end {
            return Err(FatError::InvalidFilesystem);
        }
        self.reader
            .read_sector(lba, scratch)
            .map_err(FatError::Read)?;
        let value = read_u32(scratch, byte_offset) & 0x0fff_ffff;
        if value >= 0x0fff_fff8 {
            return Ok(None);
        }
        if value < 2 || (0x0fff_fff0..0x0fff_fff8).contains(&value) {
            return Err(FatError::CorruptChain);
        }
        if !self.is_data_cluster(value) {
            return Err(FatError::CorruptChain);
        }
        Ok(Some(value))
    }
}

/// FAT chainの終端印。`0x0fff_fff8`以上はEOCとして扱われる。
#[cfg(not(target_arch = "riscv32"))]
const FAT_EOC: u32 = 0x0fff_ffff;

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

/// `Fat32`のwrite API。block deviceがsector書き込みを提供する場合だけ
/// 使える。`SectorWriter`自体がRV32へ公開されないため、このimpl blockは
/// RV32ではそもそも構成されない。
#[cfg(not(target_arch = "riscv32"))]
impl<R: SectorReader + SectorWriter> Fat32<R> {
    /// `path`が示すfileを作成またはwritableに開く。fileが既にあれば
    /// 内容を保ったまま記述子を返す。無ければ親directoryへ8.3 entryを
    /// 追加し、空のfileとして返す。最終要素は8.3へ正規化できる名前のみ
    /// 受理し（小文字は大文字化）、LFN名での作成は`InvalidName`とする。
    pub fn create_file(&mut self, path: &str) -> Result<FileDesc, FatError<R::Error>> {
        for part in path.split('/') {
            if !is_valid_component(part) {
                return Err(FatError::InvalidName);
            }
        }
        let (parent, name) = match path.rfind('/') {
            Some(index) => (&path[..index], &path[index + 1..]),
            None => ("", path),
        };
        let Some(short) = to_short_name(name) else {
            return Err(FatError::InvalidName);
        };

        let mut scratch = [0; 512];
        let dir_cluster = if parent.is_empty() {
            self.root_cluster
        } else {
            let (entry, _loc) = self.resolve_path(parent, &mut scratch)?;
            if !entry.directory {
                return Err(FatError::NotDirectory);
            }
            entry.first_cluster
        };

        // 正規化済みのraw 8.3名で照合する。小文字で作ったfileは既存の
        // 大文字entryと一致し、LFNを持つ無関係なfileとは衝突しない。
        let mut found = None;
        self.walk_dir(dir_cluster, &mut scratch, |entry, loc| {
            if entry.short_name == short {
                found = Some((*entry, loc));
                true
            } else {
                false
            }
        })?;
        if let Some((entry, loc)) = found {
            if entry.directory {
                return Err(FatError::IsDirectory);
            }
            return Ok(FileDesc {
                first_cluster: entry.first_cluster,
                size: entry.size,
                dir_cluster: loc.cluster,
                dir_index: loc.index,
            });
        }

        let (cluster, index, terminator) = self.find_free_dir_slot(dir_cluster, &mut scratch)?;
        self.place_dir_entry(cluster, index, terminator, &mut scratch, |record| {
            record[..11].copy_from_slice(&short);
            record[11] = DIR_ATTR_FILE;
        })?;
        Ok(FileDesc {
            first_cluster: 0,
            size: 0,
            dir_cluster: cluster,
            dir_index: index,
        })
    }

    /// `cluster`内のslot `index`へ32 byte recordを書き、`init`でnameと
    /// attributeを埋める。`terminator`なら終端印を次recordへ移す：
    /// sector途中なら次recordをzero化し、sector末尾なら同cluster内の
    /// 次sectorをzero-fill、cluster末尾ならchainの次clusterを
    /// zero-fillするか新規clusterを繋ぐ。
    fn place_dir_entry(
        &mut self,
        cluster: u32,
        index: u32,
        terminator: bool,
        scratch: &mut [u8; 512],
        init: impl FnOnce(&mut [u8]),
    ) -> Result<(), FatError<R::Error>> {
        let lba = self.data_sector_lba(cluster, index / 16)?;
        self.reader
            .read_sector(lba, scratch)
            .map_err(FatError::Read)?;
        let offset = (index % 16) as usize * 32;
        scratch[offset..offset + 32].fill(0);
        init(&mut scratch[offset..offset + 32]);
        if terminator && index % 16 != 15 {
            // 終端slotを消費したら、同じsector内の次recordへ終端印を置く。
            scratch[offset + 32..offset + 64].fill(0);
        }
        self.reader
            .write_sector(lba, scratch)
            .map_err(FatError::Read)?;
        if terminator && index % 16 == 15 {
            if index + 1 < self.sectors_per_cluster as u32 * 16 {
                let next_lba = self.data_sector_lba(cluster, index / 16 + 1)?;
                self.reader
                    .write_sector(next_lba, &[0; 512])
                    .map_err(FatError::Read)?;
            } else {
                match self.next_cluster(cluster, scratch)? {
                    Some(next) => self.zero_cluster(next)?,
                    None => {
                        let new = self.alloc_cluster(scratch)?;
                        self.set_fat_entry(cluster, new, scratch)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// `path`が示すdirectoryを作成する。新dirへ1 clusterを割り当て、
    /// `.`と`..`を初期化してから親dirへentryを追加する。同名entryが
    /// あればfile/dirを問わず`Exists`で拒否し、最終要素は`create`と
    /// 同じく8.3へ正規化できる名前のみ受理する。
    ///
    /// 新clusterの確保と初期化を親entryの書き込みより先に行うのは、
    /// 逆順で失敗するとcluster 0を指すentryが残るため。親dir側の
    /// 書き込みに失敗した場合は割当てたclusterを解放してから返す。
    pub fn create_dir(&mut self, path: &str) -> Result<(), FatError<R::Error>> {
        for part in path.split('/') {
            if !is_valid_component(part) {
                return Err(FatError::InvalidName);
            }
        }
        let (parent, name) = match path.rfind('/') {
            Some(index) => (&path[..index], &path[index + 1..]),
            None => ("", path),
        };
        let Some(short) = to_short_name(name) else {
            return Err(FatError::InvalidName);
        };

        let mut scratch = [0; 512];
        let dir_cluster = if parent.is_empty() {
            self.root_cluster
        } else {
            let (entry, _loc) = self.resolve_path(parent, &mut scratch)?;
            if !entry.directory {
                return Err(FatError::NotDirectory);
            }
            entry.first_cluster
        };

        // 正規化済みのraw 8.3名で照合する。`.`/`..`はwalk_dirがyield
        // しないため、ここでは実entryとの衝突だけを見る。
        let mut found = false;
        self.walk_dir(dir_cluster, &mut scratch, |entry, _loc| {
            if entry.short_name == short {
                found = true;
            }
            found
        })?;
        if found {
            return Err(FatError::Exists);
        }

        // `.`は自己、`..`は親のfirst_clusterを指す。FAT32規約で親が
        // rootの`..`は0を格納する。
        let new_cluster = self.alloc_cluster(&mut scratch)?;
        let dotdot_cluster = if dir_cluster == self.root_cluster {
            0
        } else {
            dir_cluster
        };
        let result = (|fs: &mut Self| -> Result<(), FatError<R::Error>> {
            let lba = fs.data_sector_lba(new_cluster, 0)?;
            // 新規確保clusterはalloc_clusterがzero-fill済みだが、その
            // 契約へ依存せず`.`/`..`以外が必ず終端として読めるよう
            // buffer側を明示的にzero化してから書き戻す。
            scratch.fill(0);
            scratch[..11].copy_from_slice(&DOT_SHORT_NAME);
            scratch[11] = DIR_ATTR_DIR;
            write_u16(&mut scratch, 20, (new_cluster >> 16) as u16);
            write_u16(&mut scratch, 26, new_cluster as u16);
            scratch[32..43].copy_from_slice(&DOT_DOT_SHORT_NAME);
            scratch[43] = DIR_ATTR_DIR;
            write_u16(&mut scratch, 52, (dotdot_cluster >> 16) as u16);
            write_u16(&mut scratch, 58, dotdot_cluster as u16);
            fs.reader
                .write_sector(lba, &scratch)
                .map_err(FatError::Read)?;

            let (cluster, index, terminator) = fs.find_free_dir_slot(dir_cluster, &mut scratch)?;
            fs.place_dir_entry(cluster, index, terminator, &mut scratch, |record| {
                record[..11].copy_from_slice(&short);
                record[11] = DIR_ATTR_DIR;
                write_u16(record, 20, (new_cluster >> 16) as u16);
                write_u16(record, 26, new_cluster as u16);
            })
        })(self);
        if result.is_err() {
            self.free_chain(new_cluster, &mut scratch)?;
        }
        result
    }

    /// `path`が示す空のdirectoryを削除する。`.`/`..`以外のentryを残す
    /// dirは`NotEmpty`で拒否し、fileは`NotDirectory`で拒否する。削除は
    /// `unlink_file`と同じ手順で、削除したentryの物理位置を返す。
    /// dirはfdを持てないためcaller側の失効はno-opになるが、戻り値の
    /// 形は揃えておく。
    pub fn remove_dir(&mut self, path: &str) -> Result<(u32, u32), FatError<R::Error>> {
        let mut scratch = [0; 512];
        let (entry, loc) = self.resolve_path(path, &mut scratch)?;
        if !entry.directory {
            return Err(FatError::NotDirectory);
        }

        // `.`/`..`はwalk_dirがyieldしないため、実entryが1つでもあれば
        // 空ではない。
        let mut occupied = false;
        self.walk_dir(entry.first_cluster, &mut scratch, |_entry, _loc| {
            occupied = true;
            true
        })?;
        if occupied {
            return Err(FatError::NotEmpty);
        }
        // 解放前にchain全体を検証し、破損していれば何も書かずに失敗する。
        self.chain_tail(entry.first_cluster, &mut scratch)?;

        let (start, _) = self.lfn_run_bounds(&loc, &mut scratch)?;
        self.mark_deleted_range(loc.dir_head, start, (loc.cluster, loc.index), &mut scratch)?;
        self.free_chain(entry.first_cluster, &mut scratch)?;
        Ok((loc.cluster, loc.index))
    }

    /// 開いたfileの`offset` byte目へ`data`を書き、`desc.size`を更新して
    /// dir entryへwrite-backする。`offset > size`の穴あきwriteは
    /// `InvalidOffset`として拒否する。
    pub fn write_range(
        &mut self,
        file: &mut FileDesc,
        offset: u64,
        data: &[u8],
    ) -> Result<usize, FatError<R::Error>> {
        if data.is_empty() {
            return Ok(0);
        }
        if offset > file.size as u64 {
            return Err(FatError::InvalidOffset);
        }
        // file sizeはu32 fieldなので、4 GiBをまたぐ延長は受理しない。
        let end = offset
            .checked_add(data.len() as u64)
            .filter(|end| *end <= u64::from(u32::MAX))
            .ok_or(FatError::InvalidOffset)?;

        let mut scratch = [0; 512];
        self.ensure_capacity(file, end, &mut scratch)?;

        let bytes_per_cluster = self.sectors_per_cluster as u64 * 512;
        let mut cluster = file.first_cluster;
        let mut skip = offset / bytes_per_cluster;
        while skip != 0 {
            cluster = match self.next_cluster(cluster, &mut scratch)? {
                Some(next) => next,
                None => return Err(FatError::CorruptChain),
            };
            skip -= 1;
        }

        let mut intra = (offset % bytes_per_cluster) as usize;
        let mut written = 0usize;
        while written < data.len() {
            let mut sector = (intra / 512) as u32;
            intra %= 512;
            while sector < self.sectors_per_cluster.into() && written < data.len() {
                let lba = self.data_sector_lba(cluster, sector)?;
                let take = (512 - intra).min(data.len() - written);
                if intra == 0 && take == 512 {
                    // sector全体を覆うwriteはread-modify-writeを省ける。
                    let block: &[u8; 512] = data[written..written + 512]
                        .try_into()
                        .expect("full sector slice");
                    self.reader
                        .write_sector(lba, block)
                        .map_err(FatError::Read)?;
                } else {
                    self.reader
                        .read_sector(lba, &mut scratch)
                        .map_err(FatError::Read)?;
                    scratch[intra..intra + take].copy_from_slice(&data[written..written + take]);
                    self.reader
                        .write_sector(lba, &scratch)
                        .map_err(FatError::Read)?;
                }
                written += take;
                intra = 0;
                sector += 1;
            }
            if written == data.len() {
                break;
            }
            cluster = match self.next_cluster(cluster, &mut scratch)? {
                Some(next) => next,
                None => return Err(FatError::CorruptChain),
            };
        }

        if end > file.size as u64 {
            file.size = end as u32;
        }
        self.sync_dir_entry(file, &mut scratch)?;
        Ok(written)
    }

    /// `file`のcluster chainを`end` byteが収まるまで延ばす。空fileは
    /// `first_cluster`を確保し、末尾はfree clusterをFAT検査で割当てる。
    fn ensure_capacity(
        &mut self,
        file: &mut FileDesc,
        end: u64,
        scratch: &mut [u8; 512],
    ) -> Result<(), FatError<R::Error>> {
        let bytes_per_cluster = self.sectors_per_cluster as u64 * 512;
        let needed = end.div_ceil(bytes_per_cluster);
        if needed == 0 {
            return Ok(());
        }
        let (mut last, mut count) = if file.first_cluster == 0 {
            let first = self.alloc_cluster(scratch)?;
            file.first_cluster = first;
            (first, 1u64)
        } else {
            if !self.is_data_cluster(file.first_cluster) {
                return Err(FatError::CorruptChain);
            }
            self.chain_tail(file.first_cluster, scratch)?
        };
        while count < needed {
            let new = self.alloc_cluster(scratch)?;
            self.set_fat_entry(last, new, scratch)?;
            last = new;
            count += 1;
        }
        Ok(())
    }

    /// chainの末尾clusterと長さを返す。loopするchainは`CorruptChain`。
    fn chain_tail(
        &mut self,
        first: u32,
        scratch: &mut [u8; 512],
    ) -> Result<(u32, u64), FatError<R::Error>> {
        let mut cluster = first;
        let mut count = 1u64;
        loop {
            match self.next_cluster(cluster, scratch)? {
                Some(next) => {
                    cluster = next;
                    count += 1;
                    if count > self.data_cluster_count as u64 {
                        return Err(FatError::CorruptChain);
                    }
                }
                None => return Ok((cluster, count)),
            }
        }
    }

    /// freeなclusterを1つ割り当て、EOCを印してzero-fillして返す。
    /// FAT sector単位で走査し、cluster単位の読み直しを避ける。
    fn alloc_cluster(&mut self, scratch: &mut [u8; 512]) -> Result<u32, FatError<R::Error>> {
        for fat_offset in 0..self.fat_sectors {
            let lba = self
                .fat_start
                .checked_add(fat_offset)
                .ok_or(FatError::InvalidFilesystem)?;
            self.reader
                .read_sector(lba, scratch)
                .map_err(FatError::Read)?;
            for index in 0..128u32 {
                let cluster = fat_offset * 128 + index;
                if !self.is_data_cluster(cluster) {
                    continue;
                }
                if read_u32(scratch, index as usize * 4) & 0x0fff_ffff == 0 {
                    self.set_fat_entry(cluster, FAT_EOC, scratch)?;
                    self.zero_cluster(cluster)?;
                    return Ok(cluster);
                }
            }
        }
        Err(FatError::NoSpace)
    }

    /// FATの`cluster`番目のentryへ`value`を書く。上位4 bitの予約位は
    /// 保持する。
    fn set_fat_entry(
        &mut self,
        cluster: u32,
        value: u32,
        scratch: &mut [u8; 512],
    ) -> Result<(), FatError<R::Error>> {
        let entry_offset = cluster.checked_mul(4).ok_or(FatError::CorruptChain)?;
        let fat_sector = entry_offset / 512;
        let byte_offset = (entry_offset % 512) as usize;
        let lba = self
            .fat_start
            .checked_add(fat_sector)
            .ok_or(FatError::InvalidFilesystem)?;
        let fat_end = self
            .fat_start
            .checked_add(self.fat_sectors)
            .ok_or(FatError::InvalidFilesystem)?;
        if lba >= fat_end {
            return Err(FatError::InvalidFilesystem);
        }
        self.reader
            .read_sector(lba, scratch)
            .map_err(FatError::Read)?;
        let old = read_u32(scratch, byte_offset);
        write_u32(
            scratch,
            byte_offset,
            (old & 0xf000_0000) | (value & 0x0fff_ffff),
        );
        self.reader
            .write_sector(lba, scratch)
            .map_err(FatError::Read)
    }

    /// clusterの全sectorをzero-fillする。新規割当てclusterの未定義byteと
    /// dir拡張時のstale entryを排除する。
    fn zero_cluster(&mut self, cluster: u32) -> Result<(), FatError<R::Error>> {
        let zero = [0u8; 512];
        for sector in 0..self.sectors_per_cluster as u32 {
            let lba = self.data_sector_lba(cluster, sector)?;
            self.reader
                .write_sector(lba, &zero)
                .map_err(FatError::Read)?;
        }
        Ok(())
    }

    /// `dir_cluster`のchainから書き込み可能なentry slotを返す。
    /// `0xe5`の削除跡を優先し、`0x00`の終端slotを取る場合は
    /// `terminator=true`でcallerへ終端の後始末を委ねる。全clusterが
    /// 満杯なら末尾へ新規clusterを繋いでその先頭を返す。
    fn find_free_dir_slot(
        &mut self,
        dir_cluster: u32,
        scratch: &mut [u8; 512],
    ) -> Result<(u32, u32, bool), FatError<R::Error>> {
        let mut cluster = dir_cluster;
        let mut clusters_read = 0;
        loop {
            if !self.is_data_cluster(cluster) || clusters_read >= self.data_cluster_count {
                return Err(FatError::CorruptChain);
            }
            for sector in 0..self.sectors_per_cluster as u32 {
                let lba = self.data_sector_lba(cluster, sector)?;
                self.reader
                    .read_sector(lba, scratch)
                    .map_err(FatError::Read)?;
                for record in 0..16u32 {
                    let index = sector * 16 + record;
                    match scratch[record as usize * 32] {
                        0xe5 => return Ok((cluster, index, false)),
                        0x00 => return Ok((cluster, index, true)),
                        _ => {}
                    }
                }
            }
            clusters_read += 1;
            match self.next_cluster(cluster, scratch)? {
                Some(next) => cluster = next,
                None => break,
            }
        }
        // 全slotが埋まっていたので、末尾へ新規clusterを繋ぐ。
        // `alloc_cluster`はzero-fill済みのため、残りがそのまま終端になる。
        let new = self.alloc_cluster(scratch)?;
        self.set_fat_entry(cluster, new, scratch)?;
        Ok((new, 0, false))
    }

    /// `file`の`first_cluster`と`size`をdir entryへ書き戻す。
    fn sync_dir_entry(
        &mut self,
        file: &FileDesc,
        scratch: &mut [u8; 512],
    ) -> Result<(), FatError<R::Error>> {
        let sector = file.dir_index / 16;
        let record = (file.dir_index % 16) as usize;
        let lba = self.data_sector_lba(file.dir_cluster, sector)?;
        self.reader
            .read_sector(lba, scratch)
            .map_err(FatError::Read)?;
        let offset = record * 32;
        // offset 20はfirst_clusterの上位16 bit、26は下位16 bit。
        write_u16(scratch, offset + 20, (file.first_cluster >> 16) as u16);
        write_u16(scratch, offset + 26, (file.first_cluster & 0xffff) as u16);
        write_u32(scratch, offset + 28, file.size);
        self.reader
            .write_sector(lba, scratch)
            .map_err(FatError::Read)
    }

    /// `path`が示すfileを削除する。dir entryとその直前の連続LFN record列を
    /// `0xe5`へ書き換え、fileのcluster chainを解放する。directoryは
    /// `IsDirectory`として拒否し、cluster chainが破損していれば何も
    /// 書かずに`CorruptChain`で失敗する。
    /// 戻り値は削除したentryの物理位置 `(dir_cluster, dir_index)` で、
    /// callerはこのentryを指すfdの失効照合へ使う。
    pub fn unlink_file(&mut self, path: &str) -> Result<(u32, u32), FatError<R::Error>> {
        let mut scratch = [0; 512];
        let (entry, loc) = self.resolve_path(path, &mut scratch)?;
        if entry.directory {
            return Err(FatError::IsDirectory);
        }
        // chainが破損しているなら、entryもFATも触らずに失敗する。
        if entry.first_cluster != 0 {
            if !self.is_data_cluster(entry.first_cluster) {
                return Err(FatError::CorruptChain);
            }
            self.chain_tail(entry.first_cluster, &mut scratch)?;
        }
        // entryの直前にある連続LFN record列から消し始める。`0xe5`は
        // `find_free_dir_slot`が再利用するため、slotはすぐ回収される。
        let (start, _) = self.lfn_run_bounds(&loc, &mut scratch)?;
        self.mark_deleted_range(loc.dir_head, start, (loc.cluster, loc.index), &mut scratch)?;
        self.free_chain(entry.first_cluster, &mut scratch)?;
        Ok((loc.cluster, loc.index))
    }

    /// 同一directory内で`old_path`のfileの名を`new_path`の最終要素へ
    /// 書き換える。sourceのdir entryは同じ物理位置に残るため、sourceを
    /// 指すfdは有効のまま（POSIXのrename不変条件）。既存の同名fileは
    /// POSIX置換規約で削除してから書き換え、その物理位置を返す。
    ///
    /// 新旧の親directoryが異なる場合は`CrossDirectory`（dir間移動は
    /// 非対応）、sourceやtargetがdirectoryなら`IsDirectory`、新名が
    /// 8.3へ正規化できなければ`InvalidName`として拒否する。全検証を
    /// 書き込み前に済ませるのは`unlink_file`と同じ規約である。
    pub fn rename_file(
        &mut self,
        old_path: &str,
        new_path: &str,
    ) -> Result<Option<(u32, u32)>, FatError<R::Error>> {
        for part in old_path.split('/').chain(new_path.split('/')) {
            if !is_valid_component(part) {
                return Err(FatError::InvalidName);
            }
        }
        let (new_parent, new_name) = match new_path.rfind('/') {
            Some(index) => (&new_path[..index], &new_path[index + 1..]),
            None => ("", new_path),
        };
        let Some(short) = to_short_name(new_name) else {
            return Err(FatError::InvalidName);
        };

        let mut scratch = [0; 512];
        let (entry, loc) = self.resolve_path(old_path, &mut scratch)?;
        if entry.directory {
            return Err(FatError::IsDirectory);
        }
        let new_dir = if new_parent.is_empty() {
            self.root_cluster
        } else {
            let (parent, _ploc) = self.resolve_path(new_parent, &mut scratch)?;
            if !parent.directory {
                return Err(FatError::NotDirectory);
            }
            parent.first_cluster
        };
        if new_dir != loc.dir_head {
            return Err(FatError::CrossDirectory);
        }

        // 正規化済みのraw 8.3名で照合する（create_fileと同じ規約）。
        let mut found = None;
        self.walk_dir(new_dir, &mut scratch, |candidate, cloc| {
            if candidate.short_name == short {
                found = Some((*candidate, cloc));
                true
            } else {
                false
            }
        })?;
        let mut replaced = None;
        if let Some((target, tloc)) = found {
            if tloc.cluster == loc.cluster && tloc.index == loc.index {
                // 同じentryへのrenameはno-op成功。
                return Ok(None);
            }
            if target.directory {
                return Err(FatError::IsDirectory);
            }
            // 置換対象のchainが破損しているなら、何も触らずに失敗する。
            if target.first_cluster != 0 {
                if !self.is_data_cluster(target.first_cluster) {
                    return Err(FatError::CorruptChain);
                }
                self.chain_tail(target.first_cluster, &mut scratch)?;
            }
            replaced = Some((target, tloc));
        }

        // ここから書き込み。sourceのLFN runを消し、置換対象を削除して
        // から、entryのname byteを新8.3名へ書き換える。
        let (run_start, run_end) = self.lfn_run_bounds(&loc, &mut scratch)?;
        if run_start != (loc.cluster, loc.index) {
            self.mark_deleted_range(loc.dir_head, run_start, run_end, &mut scratch)?;
        }
        if let Some((target, tloc)) = replaced {
            let (tstart, _) = self.lfn_run_bounds(&tloc, &mut scratch)?;
            self.mark_deleted_range(
                tloc.dir_head,
                tstart,
                (tloc.cluster, tloc.index),
                &mut scratch,
            )?;
            self.free_chain(target.first_cluster, &mut scratch)?;
        }
        let lba = self.data_sector_lba(loc.cluster, loc.index / 16)?;
        self.reader
            .read_sector(lba, &mut scratch)
            .map_err(FatError::Read)?;
        let offset = (loc.index % 16) as usize * 32;
        scratch[offset..offset + 11].copy_from_slice(&short);
        self.reader
            .write_sector(lba, &scratch)
            .map_err(FatError::Read)?;
        Ok(replaced.map(|(_, tloc)| (tloc.cluster, tloc.index)))
    }

    /// `loc`のentry直前に並ぶ連続LFN record列の先頭と末尾の位置を返す。
    /// LFN recordが無ければ両端ともentry自身の位置を返す。LFN runは
    /// clusterをまたぎ得るため、entryを含むdir chainの先頭からrecordを
    /// 辿る。
    fn lfn_run_bounds(
        &mut self,
        loc: &DirLoc,
        scratch: &mut [u8; 512],
    ) -> Result<(DirRecordPos, DirRecordPos), FatError<R::Error>> {
        let mut cluster = loc.dir_head;
        let mut run: Option<(DirRecordPos, DirRecordPos)> = None;
        let mut clusters_read = 0;
        loop {
            if !self.is_data_cluster(cluster) || clusters_read >= self.data_cluster_count {
                return Err(FatError::CorruptChain);
            }
            for sector in 0..self.sectors_per_cluster as u32 {
                let lba = self.data_sector_lba(cluster, sector)?;
                self.reader
                    .read_sector(lba, scratch)
                    .map_err(FatError::Read)?;
                for record in 0..16u32 {
                    let index = sector * 16 + record;
                    if cluster == loc.cluster && index == loc.index {
                        let here = (cluster, index);
                        return Ok(run.unwrap_or((here, here)));
                    }
                    let offset = record as usize * 32;
                    match scratch[offset] {
                        // resolve_pathが見つけたentryの手前に終端があるのは破損。
                        0x00 => return Err(FatError::CorruptChain),
                        // deleted recordは`walk_dir`と同じくLFN対応を切る。
                        0xe5 => run = None,
                        _ => {
                            if scratch[offset + 11] == 0x0f {
                                let pos = (cluster, index);
                                match run {
                                    Some((start, _)) => run = Some((start, pos)),
                                    None => run = Some((pos, pos)),
                                }
                            } else {
                                run = None;
                            }
                        }
                    }
                }
            }
            clusters_read += 1;
            cluster = match self.next_cluster(cluster, scratch)? {
                Some(next) => next,
                None => return Err(FatError::CorruptChain),
            };
        }
    }

    /// dir chain順で`start`から`end`（両端含む）までの全recordの先頭
    /// byteを`0xe5`へ書き換える。`start`/`end`は`(cluster, index)`で、
    /// indexはcluster内のrecord番号である。
    fn mark_deleted_range(
        &mut self,
        dir_head: u32,
        start: (u32, u32),
        end: (u32, u32),
        scratch: &mut [u8; 512],
    ) -> Result<(), FatError<R::Error>> {
        let mut cluster = dir_head;
        let mut marking = false;
        let mut clusters_read = 0;
        loop {
            if !self.is_data_cluster(cluster) || clusters_read >= self.data_cluster_count {
                return Err(FatError::CorruptChain);
            }
            for sector in 0..self.sectors_per_cluster as u32 {
                let lba = self.data_sector_lba(cluster, sector)?;
                self.reader
                    .read_sector(lba, scratch)
                    .map_err(FatError::Read)?;
                let mut dirty = false;
                for record in 0..16u32 {
                    let index = sector * 16 + record;
                    if cluster == start.0 && index == start.1 {
                        marking = true;
                    }
                    if marking {
                        scratch[record as usize * 32] = 0xe5;
                        dirty = true;
                        if cluster == end.0 && index == end.1 {
                            self.reader
                                .write_sector(lba, scratch)
                                .map_err(FatError::Read)?;
                            return Ok(());
                        }
                    }
                }
                if dirty {
                    self.reader
                        .write_sector(lba, scratch)
                        .map_err(FatError::Read)?;
                }
            }
            clusters_read += 1;
            cluster = match self.next_cluster(cluster, scratch)? {
                Some(next) => next,
                None => return Err(FatError::CorruptChain),
            };
        }
    }

    /// `first`から続くchainの全FAT entryを0（free）へ戻す。
    /// `first == 0`（空file）は何もしない。callerは`chain_tail`等で
    /// chain健全性を先に検証しておくこと。
    fn free_chain(
        &mut self,
        first: u32,
        scratch: &mut [u8; 512],
    ) -> Result<(), FatError<R::Error>> {
        if first == 0 {
            return Ok(());
        }
        let mut cluster = first;
        let mut freed = 0u64;
        loop {
            // 次を読んでから0を書く。0はfree印なので書いた後では辿れない。
            let next = self.next_cluster(cluster, scratch)?;
            self.set_fat_entry(cluster, 0, scratch)?;
            freed += 1;
            if freed > self.data_cluster_count as u64 {
                return Err(FatError::CorruptChain);
            }
            match next {
                Some(next) => cluster = next,
                None => return Ok(()),
            }
        }
    }
}

impl ParseError {
    fn into_fat_error<E>(self) -> FatError<E> {
        match self {
            Self::Unsupported => FatError::Unsupported,
            Self::InvalidFilesystem => FatError::InvalidFilesystem,
        }
    }
}

fn uppercase_ascii(byte: u8) -> u8 {
    if byte.is_ascii_lowercase() {
        byte - b'a' + b'A'
    } else {
        byte
    }
}

fn normalize_input_name(name: &str) -> Option<[u8; 11]> {
    let bytes = name.as_bytes();
    let mut dot = None;
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'.' {
            if dot.is_some() {
                return None;
            }
            dot = Some(index);
        } else if !(0x21..=0x7e).contains(&byte) || byte == b'/' || byte == b'\\' {
            return None;
        }
    }

    let base_len = dot.unwrap_or(bytes.len());
    let extension_len = dot.map_or(0, |index| bytes.len() - index - 1);
    if base_len == 0 || base_len > 8 || extension_len > 3 {
        return None;
    }

    let mut normalized = [b' '; 11];
    for index in 0..base_len {
        normalized[index] = uppercase_ascii(bytes[index]);
    }
    if let Some(dot) = dot {
        for index in 0..extension_len {
            normalized[8 + index] = uppercase_ascii(bytes[dot + 1 + index]);
        }
    }
    Some(normalized)
}

fn parse_dir_entry(record: &[u8]) -> Option<DirEntry> {
    if record[0] == 0 || record[0] == 0xe5 {
        return None;
    }
    let attributes = record[11];
    if attributes == 0x0f || attributes & 0x08 != 0 {
        return None;
    }

    let mut base_len = 8;
    while base_len > 0 && record[base_len - 1] == b' ' {
        base_len -= 1;
    }
    let mut extension_len = 3;
    while extension_len > 0 && record[8 + extension_len - 1] == b' ' {
        extension_len -= 1;
    }
    if base_len == 0 {
        return None;
    }
    for (index, &byte) in record[..8].iter().enumerate() {
        if matches!(byte, b'.' | b'/' | b'\\')
            || (byte == b' ' && index < base_len)
            || !(0x21..=0x7e).contains(&byte) && byte != b' '
        {
            return None;
        }
    }
    for (index, &byte) in record[8..11].iter().enumerate() {
        if matches!(byte, b'.' | b'/' | b'\\')
            || (byte == b' ' && index < extension_len)
            || !(0x21..=0x7e).contains(&byte) && byte != b' '
        {
            return None;
        }
    }

    let mut name = [0; 255];
    let mut name_len = 0;
    for &byte in &record[..base_len] {
        name[name_len] = uppercase_ascii(byte);
        name_len += 1;
    }
    if extension_len != 0 {
        name[name_len] = b'.';
        name_len += 1;
        for &byte in &record[8..8 + extension_len] {
            name[name_len] = uppercase_ascii(byte);
            name_len += 1;
        }
    }

    let high = u16::from_le_bytes([record[20], record[21]]) as u32;
    let low = u16::from_le_bytes([record[26], record[27]]) as u32;
    Some(DirEntry {
        name,
        name_len: name_len as u8,
        short_name: record[..11].try_into().expect("11-byte FAT name field"),
        first_cluster: (high << 16) | low,
        size: u32::from_le_bytes([record[28], record[29], record[30], record[31]]),
        directory: attributes & 0x10 != 0,
    })
}

/// path要素がentryへ一致するか。LFN表示名のASCII大小文字無視比較と、
/// 8.3 aliasへの正規化比較の両方を試す。
fn matches_component(entry: &DirEntry, part: &str) -> bool {
    part.eq_ignore_ascii_case(entry.name())
        || normalize_input_name(part).is_some_and(|wanted| wanted == entry.short_name)
}

/// path要素として受理する文字種。LFNは空白を含む印字可能ASCIIを許すが、
/// 区切りと衝突する`/`と`\`、`.`と`..`そのものは拒否する。
#[cfg(not(target_arch = "riscv32"))]
fn is_valid_component(part: &str) -> bool {
    !part.is_empty()
        && part != "."
        && part != ".."
        && part.len() <= 255
        && part
            .bytes()
            .all(|byte| (0x20..=0x7e).contains(&byte) && byte != b'\\')
}

/// raw 8.3名に対するLFN checksum。LFN record列がどのshort entryへ
/// 属するかを照合するために使う。
#[cfg(not(target_arch = "riscv32"))]
fn lfn_checksum(short_name: &[u8; 11]) -> u8 {
    let mut sum = 0u8;
    for &byte in short_name {
        sum = ((sum & 1) << 7).wrapping_add(sum >> 1).wrapping_add(byte);
    }
    sum
}

/// walk中に集めたLFN chunk列。RV32ではZSTのno-opとして実装し、
/// IMEMへ機構を載せない。
#[cfg(not(target_arch = "riscv32"))]
struct PendingLfn {
    /// chunk番号順にdecodeしたASCII byte。chunk `s`は`(s-1)*13`から始まる。
    name: [u8; 255],
    /// 連続性を検査する次に来るべきseq。0は未収集または中断。
    expected_seq: u8,
    /// 最終chunk（disk上では先頭）が持つchunk総数。
    chunk_count: u8,
    checksum: u8,
    /// seq 1まで揃ったか。
    complete: bool,
    /// 非ASCIIやbuf超過など、decode不能にした印。
    invalid: bool,
}

#[cfg(not(target_arch = "riscv32"))]
impl PendingLfn {
    const fn new() -> Self {
        Self {
            name: [0; 255],
            expected_seq: 0,
            chunk_count: 0,
            checksum: 0,
            complete: false,
            invalid: false,
        }
    }

    fn reset(&mut self) {
        self.expected_seq = 0;
        self.complete = false;
    }

    /// 1 record分のLFN chunkをdecodeして蓄える。`0x40`flag付きは
    /// 新しい列の開始として状態を張り直す。
    fn push(&mut self, record: &[u8]) {
        const CHAR_OFFSETS: [usize; 13] = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30];
        let seq = record[0] & 0x1f;
        if record[0] & 0x40 != 0 {
            self.reset();
            self.chunk_count = seq;
            self.checksum = record[13];
            self.expected_seq = seq;
            self.invalid = seq == 0 || seq > 20;
        }
        if self.invalid || seq == 0 || seq != self.expected_seq {
            self.reset();
            return;
        }
        let start = (seq as usize - 1) * 13;
        if start + 13 > self.name.len() {
            self.invalid = true;
            self.reset();
            return;
        }
        for (index, &offset) in CHAR_OFFSETS.iter().enumerate() {
            let ch = u16::from_le_bytes([record[offset], record[offset + 1]]);
            let decoded = match ch {
                0x0000 | 0xffff => 0,
                0x20..=0x7e => ch as u8,
                _ => {
                    self.invalid = true;
                    0
                }
            };
            self.name[start + index] = decoded;
        }
        self.expected_seq = seq - 1;
        if seq == 1 {
            self.complete = true;
        }
    }

    /// `record`が引き受けるLFN名を`entry`へ適用する。checksumまたは
    /// 収集条件が合わなければ8.3名のままにし、どちらでも状態をresetする。
    fn apply(&mut self, entry: &mut DirEntry, record: &[u8]) {
        if self.complete
            && !self.invalid
            && self.checksum == lfn_checksum(record[..11].try_into().unwrap())
        {
            let end = self.chunk_count as usize * 13;
            let name_len = self.name[..end]
                .iter()
                .position(|&byte| byte == 0)
                .unwrap_or(end);
            entry.name[..name_len].copy_from_slice(&self.name[..name_len]);
            entry.name_len = name_len as u8;
        }
        self.reset();
    }
}

/// RV32では状態を持たず、全ての操作をinlinedなno-opへ畳む。
#[cfg(target_arch = "riscv32")]
struct PendingLfn;

#[cfg(target_arch = "riscv32")]
impl PendingLfn {
    const fn new() -> Self {
        Self
    }

    fn reset(&mut self) {}

    fn push(&mut self, _record: &[u8]) {}

    fn apply(&mut self, _entry: &mut DirEntry, _record: &[u8]) {}
}

fn is_extended_partition(partition_type: u8) -> bool {
    matches!(partition_type, 0x05 | 0x0f | 0x85)
}

fn read_u16(sector: &[u8; 512], offset: usize) -> u16 {
    u16::from_le_bytes([sector[offset], sector[offset + 1]])
}

#[cfg(not(target_arch = "riscv32"))]
fn write_u16(sector: &mut [u8], offset: usize, value: u16) {
    sector[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}

#[cfg(not(target_arch = "riscv32"))]
fn write_u32(sector: &mut [u8], offset: usize, value: u32) {
    sector[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

/// `name`をFATのraw 8.3名へ正規化する。小文字ASCIIは大文字化し、
/// base 1..=8文字・ext 0..=3文字の`BASE.EXT`形だけを受理する。
/// 変換できない名前（空白・`.`重複・長すぎる要素など）は`None`。
#[cfg(not(target_arch = "riscv32"))]
fn to_short_name(name: &str) -> Option<[u8; 11]> {
    fn is_83_byte(byte: u8) -> bool {
        byte.is_ascii_uppercase()
            || byte.is_ascii_digit()
            || matches!(
                byte,
                b'!' | b'#'
                    | b'$'
                    | b'%'
                    | b'&'
                    | b'\''
                    | b'('
                    | b')'
                    | b'-'
                    | b'@'
                    | b'^'
                    | b'_'
                    | b'`'
                    | b'{'
                    | b'}'
                    | b'~'
            )
    }

    let (base, ext) = match name.split_once('.') {
        Some((base, ext)) => (base, ext),
        None => (name, ""),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 || ext.contains('.') {
        return None;
    }
    let mut short = [b' '; 11];
    for (index, byte) in base.bytes().enumerate() {
        let byte = byte.to_ascii_uppercase();
        if !is_83_byte(byte) {
            return None;
        }
        short[index] = byte;
    }
    for (index, byte) in ext.bytes().enumerate() {
        let byte = byte.to_ascii_uppercase();
        if !is_83_byte(byte) {
            return None;
        }
        short[8 + index] = byte;
    }
    Some(short)
}

fn read_u32(sector: &[u8; 512], offset: usize) -> u32 {
    u32::from_le_bytes([
        sector[offset],
        sector[offset + 1],
        sector[offset + 2],
        sector[offset + 3],
    ])
}

fn parse_boot_sector(
    sector: &[u8; 512],
    partition_start: u32,
    partition_sectors: Option<u32>,
) -> Result<Geometry, ParseError> {
    if sector[3..11] == *b"EXFAT   " {
        return Err(ParseError::Unsupported);
    }
    if sector[510..512] != [0x55, 0xaa] {
        return Err(ParseError::InvalidFilesystem);
    }

    let bytes_per_sector = read_u16(sector, 11);
    if bytes_per_sector == 0 {
        return Err(ParseError::InvalidFilesystem);
    }
    if bytes_per_sector != 512 {
        return Err(ParseError::Unsupported);
    }

    let sectors_per_cluster = sector[13];
    if sectors_per_cluster == 0 || !sectors_per_cluster.is_power_of_two() {
        return Err(ParseError::InvalidFilesystem);
    }
    if sectors_per_cluster > 128 {
        return Err(ParseError::Unsupported);
    }

    let reserved_sectors = read_u16(sector, 14) as u32;
    if reserved_sectors == 0 {
        return Err(ParseError::InvalidFilesystem);
    }

    let fat_count = sector[16];
    if fat_count == 0 {
        return Err(ParseError::InvalidFilesystem);
    }
    if fat_count > 2 {
        return Err(ParseError::Unsupported);
    }

    let root_entries = read_u16(sector, 17);
    let fat_size_16 = read_u16(sector, 22);
    if root_entries != 0 || fat_size_16 != 0 {
        return Err(ParseError::Unsupported);
    }

    let volume_sectors_16 = read_u16(sector, 19) as u32;
    let volume_sectors_32 = read_u32(sector, 32);
    let volume_sectors = if volume_sectors_16 != 0 {
        volume_sectors_16
    } else {
        volume_sectors_32
    };
    if volume_sectors == 0 {
        return Err(ParseError::InvalidFilesystem);
    }
    if let Some(partition_sectors) = partition_sectors
        && volume_sectors > partition_sectors
    {
        return Err(ParseError::InvalidFilesystem);
    }

    let fat_sectors = read_u32(sector, 36);
    if fat_sectors == 0 {
        return Err(ParseError::InvalidFilesystem);
    }
    let root_cluster = read_u32(sector, 44);
    if root_cluster < 2 {
        return Err(ParseError::InvalidFilesystem);
    }

    let fat_total_sectors = (fat_count as u32)
        .checked_mul(fat_sectors)
        .ok_or(ParseError::InvalidFilesystem)?;
    let data_start_relative = reserved_sectors
        .checked_add(fat_total_sectors)
        .ok_or(ParseError::InvalidFilesystem)?;
    let fat_end_relative = reserved_sectors
        .checked_add(fat_sectors)
        .ok_or(ParseError::InvalidFilesystem)?;
    if fat_end_relative > volume_sectors || data_start_relative > volume_sectors {
        return Err(ParseError::InvalidFilesystem);
    }

    let data_sectors = volume_sectors
        .checked_sub(data_start_relative)
        .ok_or(ParseError::InvalidFilesystem)?;
    let data_cluster_count = data_sectors / sectors_per_cluster as u32;
    if data_cluster_count < 65_525 {
        return Err(ParseError::Unsupported);
    }
    let data_span = data_cluster_count
        .checked_mul(sectors_per_cluster as u32)
        .ok_or(ParseError::InvalidFilesystem)?;
    let data_end_relative = data_start_relative
        .checked_add(data_span)
        .ok_or(ParseError::InvalidFilesystem)?;
    if data_end_relative > volume_sectors {
        return Err(ParseError::InvalidFilesystem);
    }
    let max_cluster = data_cluster_count
        .checked_add(1)
        .ok_or(ParseError::InvalidFilesystem)?;
    if root_cluster > max_cluster {
        return Err(ParseError::InvalidFilesystem);
    }

    let volume_end = partition_start
        .checked_add(volume_sectors)
        .ok_or(ParseError::InvalidFilesystem)?;
    let fat_start = partition_start
        .checked_add(reserved_sectors)
        .ok_or(ParseError::InvalidFilesystem)?;
    let fat_end = fat_start
        .checked_add(fat_sectors)
        .ok_or(ParseError::InvalidFilesystem)?;
    if fat_end > volume_end {
        return Err(ParseError::InvalidFilesystem);
    }
    let data_start = partition_start
        .checked_add(data_start_relative)
        .ok_or(ParseError::InvalidFilesystem)?;
    let data_end = data_start
        .checked_add(data_span)
        .ok_or(ParseError::InvalidFilesystem)?;
    if data_end > volume_end {
        return Err(ParseError::InvalidFilesystem);
    }

    Ok(Geometry {
        partition_start,
        volume_sectors,
        fat_start,
        fat_sectors,
        data_start,
        sectors_per_cluster,
        root_cluster,
        data_cluster_count,
    })
}

#[cfg(test)]
impl<R> Fat32<R> {
    pub fn partition_start(&self) -> u32 {
        self.partition_start
    }

    pub fn root_cluster(&self) -> u32 {
        self.root_cluster
    }
}

#[cfg(test)]
mod tests {
    extern crate std;

    use super::{
        DirEntry, Fat32, FatError, FileDesc, lfn_checksum, read_u16, read_u32, to_short_name,
    };
    use crate::storage::{SectorReader, SectorWriter};
    use std::{string::String, vec::Vec};

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum ReadError {
        MissingSector(u32),
    }

    struct MemoryReader<const N: usize> {
        sectors: [(u32, [u8; 512]); N],
    }

    impl<const N: usize> MemoryReader<N> {
        fn with_sectors(sectors: [(u32, [u8; 512]); N]) -> Self {
            Self { sectors }
        }
    }

    impl MemoryReader<1> {
        fn with_sector(lba: u32, sector: [u8; 512]) -> Self {
            Self::with_sectors([(lba, sector)])
        }
    }

    impl MemoryReader<2> {
        fn with_mbr_volume(start: u32, length: u32, boot: [u8; 512]) -> Self {
            let mut mbr = [0; 512];
            write_partition(&mut mbr, 0, 0x0c, start, length);
            mbr[510..512].copy_from_slice(&[0x55, 0xaa]);
            Self::with_sectors([(0, mbr), (start, boot)])
        }
    }

    impl<const N: usize> SectorReader for MemoryReader<N> {
        type Error = ReadError;

        fn read_sector(
            &mut self,
            lba: u32,
            destination: &mut [u8; 512],
        ) -> Result<(), Self::Error> {
            for &(sector_lba, ref sector) in &self.sectors {
                if sector_lba == lba {
                    destination.copy_from_slice(sector);
                    return Ok(());
                }
            }
            Err(ReadError::MissingSector(lba))
        }
    }

    /// fixtureに無いlbaへのwriteは`MissingSector`で失敗させ、volume外の
    /// 書き込みをテストで検出できるようにする。
    impl<const N: usize> SectorWriter for MemoryReader<N> {
        fn write_sector(&mut self, lba: u32, source: &[u8; 512]) -> Result<(), Self::Error> {
            for &mut (sector_lba, ref mut sector) in &mut self.sectors {
                if sector_lba == lba {
                    sector.copy_from_slice(source);
                    return Ok(());
                }
            }
            Err(ReadError::MissingSector(lba))
        }
    }

    fn write_u16(sector: &mut [u8; 512], offset: usize, value: u16) {
        sector[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
    }

    fn write_u32(sector: &mut [u8; 512], offset: usize, value: u32) {
        sector[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }

    fn write_partition(
        mbr: &mut [u8; 512],
        index: usize,
        partition_type: u8,
        start: u32,
        length: u32,
    ) {
        let offset = 446 + 16 * index;
        mbr[offset + 4] = partition_type;
        write_u32(mbr, offset + 8, start);
        write_u32(mbr, offset + 12, length);
    }

    fn valid_boot_sector() -> [u8; 512] {
        let mut boot = [0; 512];
        write_u16(&mut boot, 11, 512);
        boot[13] = 1;
        write_u16(&mut boot, 14, 32);
        boot[16] = 2;
        write_u16(&mut boot, 17, 0);
        write_u16(&mut boot, 19, 0);
        write_u16(&mut boot, 22, 0);
        write_u32(&mut boot, 32, 70_000);
        write_u32(&mut boot, 36, 128);
        write_u32(&mut boot, 44, 2);
        boot[510..512].copy_from_slice(&[0x55, 0xaa]);
        boot
    }

    fn write_fat_entry(sector: &mut [u8; 512], cluster: u32, value: u32) {
        write_u32(sector, (cluster * 4) as usize, value);
    }

    fn write_directory_entry(
        sector: &mut [u8; 512],
        index: usize,
        name: &[u8; 11],
        attributes: u8,
        first_cluster: u32,
        size: u32,
    ) {
        let offset = index * 32;
        sector[offset..offset + 11].copy_from_slice(name);
        sector[offset + 11] = attributes;
        write_u16(sector, offset + 20, (first_cluster >> 16) as u16);
        write_u16(sector, offset + 26, first_cluster as u16);
        write_u32(sector, offset + 28, size);
    }

    fn directory_fat() -> [u8; 512] {
        let mut fat = [0; 512];
        write_fat_entry(&mut fat, 2, 3);
        write_fat_entry(&mut fat, 3, 0x0fff_ffff);
        fat
    }

    fn mounted_directory_fixture() -> Fat32<MemoryReader<4>> {
        let mut first_directory_cluster = [0; 512];
        for index in 0..16 {
            first_directory_cluster[index * 32] = 0xe5;
        }
        write_directory_entry(&mut first_directory_cluster, 1, b"LFN     TXT", 0x0f, 0, 0);
        write_directory_entry(&mut first_directory_cluster, 2, b"LABEL      ", 0x08, 0, 0);

        let mut second_directory_cluster = [0; 512];
        write_directory_entry(&mut second_directory_cluster, 0, b"HELLO   TXT", 0x20, 4, 0);
        write_directory_entry(&mut second_directory_cluster, 1, b"SUBDIR     ", 0x10, 5, 0);
        Fat32::mount(MemoryReader::with_sectors([
            (0, valid_boot_sector()),
            (32, directory_fat()),
            (288, first_directory_cluster),
            (289, second_directory_cluster),
        ]))
        .unwrap()
    }

    /// rootに`SUBDIR`（cluster 5）を持ち、その中に`NOTE.TXT`（cluster 6）を
    /// 持つfixture。`.`と`..`は列挙から除外されることを確認するために入れる。
    fn mounted_nested_fixture() -> Fat32<MemoryReader<5>> {
        let mut fat = [0; 512];
        write_fat_entry(&mut fat, 2, 0x0fff_ffff);
        write_fat_entry(&mut fat, 5, 0x0fff_ffff);
        write_fat_entry(&mut fat, 6, 0x0fff_ffff);

        let mut root_cluster = [0; 512];
        write_directory_entry(&mut root_cluster, 0, b"SUBDIR     ", 0x10, 5, 0);
        write_directory_entry(&mut root_cluster, 1, b"HELLO   TXT", 0x20, 4, 0);

        let mut subdir = [0; 512];
        write_directory_entry(&mut subdir, 0, b".          ", 0x10, 5, 0);
        write_directory_entry(&mut subdir, 1, b"..         ", 0x10, 2, 0);
        write_directory_entry(&mut subdir, 2, b"NOTE    TXT", 0x20, 6, 11);

        let mut data = [0; 512];
        data[..11].copy_from_slice(b"nested note");

        Fat32::mount(MemoryReader::with_sectors([
            (0, valid_boot_sector()),
            (32, fat),
            (288, root_cluster),
            (291, subdir),
            (292, data),
        ]))
        .unwrap()
    }

    /// `short_name`に対するLFN checksum record列を`sector`の`index`以降へ
    /// 書く。disk上は最終chunkから順に並ぶ。
    fn write_lfn_records(sector: &mut [u8; 512], index: usize, name: &str, short_name: &[u8; 11]) {
        let chars: Vec<u16> = name.encode_utf16().collect();
        let chunks = chars.len().div_ceil(13);
        let checksum = lfn_checksum(short_name);
        for chunk in 0..chunks {
            // disk上は末尾chunk（seq=N, `0x40`flag付き）から並ぶ。seq `s`の
            // recordはnameの`(s-1)*13`文字目からの13文字を保持する。
            let seq = (chunks - chunk) as u8;
            let record = &mut sector[(index + chunk) * 32..(index + chunk) * 32 + 32];
            record[0] = if chunk == 0 { seq | 0x40 } else { seq };
            record[11] = 0x0f;
            record[13] = checksum;
            for position in 0..13 {
                let ch_index = (seq as usize - 1) * 13 + position;
                // name直後の`0x0000`終端、その後は`0xffff`で埋める。
                let ch = if ch_index == chars.len() {
                    0x0000
                } else {
                    chars.get(ch_index).copied().unwrap_or(0xffff)
                };
                let offset = [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30][position];
                record[offset..offset + 2].copy_from_slice(&ch.to_le_bytes());
            }
        }
    }

    /// rootに`Long File Name.txt`（alias `LONGFI~1.TXT`）を持つfixture。
    /// `corrupt`を立てるとchecksumを壊して8.3名へfallbackさせる。
    fn mounted_lfn_fixture(corrupt: bool) -> Fat32<MemoryReader<4>> {
        let mut fat = [0; 512];
        write_fat_entry(&mut fat, 2, 0x0fff_ffff);
        write_fat_entry(&mut fat, 4, 0x0fff_ffff);

        let mut root = [0; 512];
        write_lfn_records(&mut root, 0, "Long File Name.txt", b"LONGFI~1TXT");
        if corrupt {
            // `0x40`flag付きの末尾chunkが持つchecksumを壊す。
            root[13] ^= 0xff;
        }
        write_directory_entry(&mut root, 2, b"LONGFI~1TXT", 0x20, 4, 8);

        let mut data = [0; 512];
        data[..8].copy_from_slice(b"lfn data");

        Fat32::mount(MemoryReader::with_sectors([
            (0, valid_boot_sector()),
            (32, fat),
            (288, root),
            (290, data),
        ]))
        .unwrap()
    }

    /// LFN record列とshort entryの間にdeleted entryが挟まるfixture。
    /// 対応が切れて8.3名へfallbackすることを確認する。
    fn mounted_severed_lfn_fixture() -> Fat32<MemoryReader<3>> {
        let mut fat = [0; 512];
        write_fat_entry(&mut fat, 2, 0x0fff_ffff);

        let mut root = [0; 512];
        write_lfn_records(&mut root, 0, "Long File Name.txt", b"LONGFI~1TXT");
        // LFN列の直後をdeleted entryで区切り、対応を断つ。
        root[64] = 0xe5;
        write_directory_entry(&mut root, 3, b"LONGFI~1TXT", 0x20, 0, 0);

        Fat32::mount(MemoryReader::with_sectors([
            (0, valid_boot_sector()),
            (32, fat),
            (288, root),
        ]))
        .unwrap()
    }

    fn mounted_multicluster_file_fixture(content: &[u8]) -> Fat32<MemoryReader<5>> {
        let mut fat = [0; 512];
        write_fat_entry(&mut fat, 2, 0x0fff_ffff);
        write_fat_entry(&mut fat, 4, 5);
        write_fat_entry(&mut fat, 5, 0x0fff_ffff);

        let mut root = [0; 512];
        write_directory_entry(&mut root, 0, b"HELLO   TXT", 0x20, 4, content.len() as u32);

        assert!(content.len() <= 1024);
        let first_len = if content.len() < 512 {
            content.len()
        } else {
            512
        };
        let mut data = [0; 512];
        data[..first_len].copy_from_slice(&content[..first_len]);
        let mut next_data = [0xa5; 512];
        if content.len() > first_len {
            next_data[..content.len() - first_len].copy_from_slice(&content[first_len..]);
        }
        Fat32::mount(MemoryReader::with_sectors([
            (0, valid_boot_sector()),
            (32, fat),
            (288, root),
            (290, data),
            (291, next_data),
        ]))
        .unwrap()
    }

    fn mounted_empty_file_fixture() -> Fat32<MemoryReader<3>> {
        let mut fat = [0; 512];
        write_fat_entry(&mut fat, 2, 0x0fff_ffff);

        let mut root = [0; 512];
        write_directory_entry(&mut root, 0, b"EMPTY   TXT", 0x20, 0, 0);
        Fat32::mount(MemoryReader::with_sectors([
            (0, valid_boot_sector()),
            (32, fat),
            (288, root),
        ]))
        .unwrap()
    }

    fn mounted_cyclic_file_fixture() -> Fat32<MemoryReader<4>> {
        let mut fat = [0; 512];
        write_fat_entry(&mut fat, 2, 0x0fff_ffff);
        write_fat_entry(&mut fat, 4, 4);

        let mut root = [0; 512];
        write_directory_entry(&mut root, 0, b"LOOP    BIN", 0x20, 4, u32::MAX);
        Fat32::mount(MemoryReader::with_sectors([
            (0, valid_boot_sector()),
            (32, fat),
            (288, root),
            (290, [0x5a; 512]),
        ]))
        .unwrap()
    }

    fn mounted_malformed_name_fixture() -> Fat32<MemoryReader<3>> {
        let mut fat = [0; 512];
        write_fat_entry(&mut fat, 2, 0x0fff_ffff);
        let mut root = [0; 512];
        write_directory_entry(&mut root, 0, b"BAD.NAME   ", 0x20, 4, 0);
        write_directory_entry(&mut root, 1, b"GOOD    TXT", 0x20, 4, 0);
        Fat32::mount(MemoryReader::with_sectors([
            (0, valid_boot_sector()),
            (32, fat),
            (288, root),
        ]))
        .unwrap()
    }

    #[test]
    fn root_listing_filters_metadata_and_crosses_a_cluster_boundary() {
        let mut volume = mounted_directory_fixture();
        let mut names = Vec::new();
        volume
            .for_each_entry("", |entry| names.push(String::from(entry.name())))
            .unwrap();
        assert_eq!(names, ["HELLO.TXT", "SUBDIR"]);
    }

    #[test]
    fn file_lookup_is_ascii_case_insensitive_and_streams_exact_size() {
        let mut volume = mounted_multicluster_file_fixture(b"hello from sd\n");
        let mut output = Vec::new();
        volume
            .read_file("hello.txt", |bytes| output.extend_from_slice(bytes))
            .unwrap();
        assert_eq!(output, b"hello from sd\n");
    }

    #[test]
    fn empty_file_reads_no_data_cluster() {
        let mut volume = mounted_empty_file_fixture();
        let mut called = false;
        volume.read_file("EMPTY.TXT", |_| called = true).unwrap();
        assert!(!called);
    }

    #[test]
    fn cyclic_chain_is_rejected_before_the_traversal_bound_is_exceeded() {
        let mut volume = mounted_cyclic_file_fixture();
        assert_eq!(
            volume.read_file("LOOP.BIN", |_| {}),
            Err(FatError::CorruptChain)
        );
    }

    #[test]
    fn root_listing_stops_at_the_directory_end_marker() {
        let mut volume = mounted_multicluster_file_fixture(b"contents");
        let mut entries = Vec::<DirEntry>::new();
        volume
            .for_each_entry("", |entry| entries.push(*entry))
            .unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name(), "HELLO.TXT");
    }

    #[test]
    fn root_listing_ignores_dots_inside_raw_short_name_fields() {
        let mut volume = mounted_malformed_name_fixture();
        let mut names = Vec::new();
        volume
            .for_each_entry("", |entry| names.push(String::from(entry.name())))
            .unwrap();
        assert_eq!(names, ["GOOD.TXT"]);
    }

    #[test]
    fn file_lookup_rejects_missing_directory_and_invalid_names() {
        let mut volume = mounted_directory_fixture();
        assert_eq!(
            volume.read_file("MISSING.TXT", |_| {}),
            Err(FatError::NotFound)
        );
        assert_eq!(
            volume.read_file("SUBDIR", |_| {}),
            Err(FatError::IsDirectory)
        );
        // `A`がrootに存在しないため、path解決は`NotFound`で止まる。
        // `.TXT`や`A..TXT`のように8.3では不正でもLFNでは有効な要素は
        // 同じくlookupへ進み`NotFound`となる。
        for name in ["A/B.TXT", ".TXT", "TOO-LONG9.TXT", "A..TXT"] {
            assert_eq!(
                volume.read_file(name, |_| {}),
                Err(FatError::NotFound),
                "{name} unexpectedly accepted"
            );
        }
        for name in [
            "",
            "A\\\\B.TXT",
            "A//B.TXT",
            "/HELLO.TXT",
            "HELLO.TXT/",
            "./HELLO.TXT",
            "../HELLO.TXT",
        ] {
            assert_eq!(
                volume.read_file(name, |_| {}),
                Err(FatError::InvalidName),
                "{name} unexpectedly accepted"
            );
        }
    }

    #[test]
    fn subdirectory_listing_filters_dot_entries() {
        let mut volume = mounted_nested_fixture();
        let mut names = Vec::new();
        volume
            .for_each_entry("SUBDIR", |entry| names.push(String::from(entry.name())))
            .unwrap();
        assert_eq!(names, ["NOTE.TXT"]);
    }

    #[test]
    fn nested_file_reads_and_matches_ascii_case_insensitively() {
        let mut volume = mounted_nested_fixture();
        let mut output = Vec::new();
        volume
            .read_file("subdir/note.txt", |bytes| output.extend_from_slice(bytes))
            .unwrap();
        assert_eq!(output, b"nested note");
    }

    #[test]
    fn nested_lookup_reports_missing_files_and_non_directories() {
        let mut volume = mounted_nested_fixture();
        assert_eq!(
            volume.read_file("SUBDIR/MISSING.TXT", |_| {}),
            Err(FatError::NotFound)
        );
        assert_eq!(
            volume.read_file("SUBDIR", |_| {}),
            Err(FatError::IsDirectory)
        );
        // fileをdirectoryとして潜るpathと、file自体を列挙するpath。
        assert_eq!(
            volume.read_file("HELLO.TXT/NOTE.TXT", |_| {}),
            Err(FatError::NotDirectory)
        );
        let mut visited = false;
        assert_eq!(
            volume.for_each_entry("HELLO.TXT", |_| visited = true),
            Err(FatError::NotDirectory)
        );
        assert!(!visited);
    }

    #[test]
    fn lfn_name_appears_in_listing_and_resolves_for_reads() {
        let mut volume = mounted_lfn_fixture(false);
        let mut names = Vec::new();
        volume
            .for_each_entry("", |entry| names.push(String::from(entry.name())))
            .unwrap();
        assert_eq!(names, ["Long File Name.txt"]);

        // LFN名でも8.3 aliasでも読める。大小文字は区別しない。
        for name in ["Long File Name.txt", "long file name.txt", "LONGFI~1.TXT"] {
            let mut output = Vec::new();
            volume
                .read_file(name, |bytes| output.extend_from_slice(bytes))
                .unwrap_or_else(|error| panic!("{name} must resolve: {error:?}"));
            assert_eq!(output, b"lfn data");
        }
    }

    #[test]
    fn lfn_falls_back_to_the_short_name_on_a_bad_checksum() {
        let mut volume = mounted_lfn_fixture(true);
        let mut names = Vec::new();
        volume
            .for_each_entry("", |entry| names.push(String::from(entry.name())))
            .unwrap();
        assert_eq!(names, ["LONGFI~1.TXT"]);
        assert_eq!(
            volume.read_file("Long File Name.txt", |_| {}),
            Err(FatError::NotFound)
        );
    }

    #[test]
    fn lfn_falls_back_when_a_deleted_entry_severs_the_association() {
        let mut volume = mounted_severed_lfn_fixture();
        let mut names = Vec::new();
        volume
            .for_each_entry("", |entry| names.push(String::from(entry.name())))
            .unwrap();
        assert_eq!(names, ["LONGFI~1.TXT"]);
    }

    #[test]
    fn file_streaming_emits_only_the_declared_final_partial_sector() {
        let mut volume = mounted_multicluster_file_fixture(&[0x11; 513]);
        let mut output = Vec::new();
        volume
            .read_file("HELLO.TXT", |bytes| output.extend_from_slice(bytes))
            .unwrap();
        assert_eq!(output.len(), 513);
        assert_eq!(output[512], 0x11);
    }

    #[test]
    fn file_streaming_rejects_free_bad_reserved_and_out_of_range_clusters() {
        for value in [0, 1, 0x0fff_fff0, 0x0fff_fff7, 0x0fff_fff8, 69_714] {
            let mut fat = [0; 512];
            write_fat_entry(&mut fat, 2, 0x0fff_ffff);
            write_fat_entry(&mut fat, 4, value);
            let mut root = [0; 512];
            write_directory_entry(&mut root, 0, b"BAD     BIN", 0x20, 4, 513);
            let mut volume = Fat32::mount(MemoryReader::with_sectors([
                (0, valid_boot_sector()),
                (32, fat),
                (288, root),
                (290, [0x22; 512]),
            ]))
            .unwrap();
            assert_eq!(
                volume.read_file("BAD.BIN", |_| {}),
                Err(FatError::CorruptChain),
                "FAT value {value:#x} unexpectedly accepted"
            );
        }
    }

    #[test]
    fn mounts_a_valid_fat32_superfloppy_at_lba_zero() {
        let reader = MemoryReader::with_sector(0, valid_boot_sector());
        let volume = Fat32::mount(reader).unwrap();
        assert_eq!(volume.partition_start(), 0);
        assert_eq!(volume.root_cluster(), 2);
    }

    #[test]
    fn scans_primary_mbr_entries_and_mounts_the_first_fat32_partition() {
        let mut mbr = [0; 512];
        write_partition(&mut mbr, 0, 0x83, 1, 10);
        write_partition(&mut mbr, 1, 0x0c, 2048, 70_000);
        mbr[510..512].copy_from_slice(&[0x55, 0xaa]);
        let reader = MemoryReader::with_sectors([(0, mbr), (2048, valid_boot_sector())]);
        assert_eq!(Fat32::mount(reader).unwrap().partition_start(), 2048);
    }

    #[test]
    fn mounts_the_first_fat32_partition_without_inspecting_later_entries() {
        let mut mbr = [0; 512];
        write_partition(&mut mbr, 0, 0x0c, 2048, 70_000);
        write_partition(&mut mbr, 1, 0xee, u32::MAX, u32::MAX);
        mbr[510..512].copy_from_slice(&[0x55, 0xaa]);
        let reader = MemoryReader::with_sectors([(0, mbr), (2048, valid_boot_sector())]);
        assert_eq!(Fat32::mount(reader).unwrap().partition_start(), 2048);
    }

    #[test]
    fn rejects_a_bpb_whose_data_range_exceeds_the_partition() {
        let reader = MemoryReader::with_mbr_volume(2048, 100, valid_boot_sector());
        assert!(matches!(
            Fat32::mount(reader),
            Err(FatError::InvalidFilesystem)
        ));
    }

    #[test]
    fn rejects_malformed_or_unsupported_bpb_values() {
        let cases = [
            ("bytes per sector", 11, 1024u32),
            ("reserved sectors", 14, 0),
            ("root entries", 17, 1),
            ("fat16 size", 22, 1),
            ("fat32 size", 36, 0),
            ("root cluster", 44, 1),
        ];
        for &(name, offset, value) in &cases {
            let mut boot = valid_boot_sector();
            if offset == 44 || offset == 36 {
                write_u32(&mut boot, offset, value);
            } else {
                write_u16(&mut boot, offset, value as u16);
            }
            assert!(
                Fat32::mount(MemoryReader::with_sector(0, boot)).is_err(),
                "case {name} unexpectedly mounted"
            );
        }

        let mut boot = valid_boot_sector();
        boot[13] = 0;
        assert!(Fat32::mount(MemoryReader::with_sector(0, boot)).is_err());
        let mut boot = valid_boot_sector();
        boot[13] = 3;
        assert!(Fat32::mount(MemoryReader::with_sector(0, boot)).is_err());
        let mut boot = valid_boot_sector();
        boot[16] = 0;
        assert!(Fat32::mount(MemoryReader::with_sector(0, boot)).is_err());
        let mut boot = valid_boot_sector();
        boot[16] = 3;
        assert!(Fat32::mount(MemoryReader::with_sector(0, boot)).is_err());
    }

    #[test]
    fn rejects_checked_arithmetic_overflow() {
        let mut boot = valid_boot_sector();
        write_u32(&mut boot, 32, u32::MAX);
        write_u32(&mut boot, 36, u32::MAX);
        let reader = MemoryReader::with_mbr_volume(1, u32::MAX, boot);
        assert!(matches!(
            Fat32::mount(reader),
            Err(FatError::InvalidFilesystem)
        ));
    }

    #[test]
    fn rejects_gpt_protective_mbr() {
        let mut mbr = [0; 512];
        write_partition(&mut mbr, 0, 0xee, 1, u32::MAX);
        mbr[510..512].copy_from_slice(&[0x55, 0xaa]);
        assert!(matches!(
            Fat32::mount(MemoryReader::with_sector(0, mbr)),
            Err(FatError::Unsupported)
        ));
    }

    #[test]
    fn rejects_missing_boot_signature() {
        let mut boot = valid_boot_sector();
        boot[510..512].copy_from_slice(&[0, 0]);
        assert!(Fat32::mount(MemoryReader::with_sector(0, boot)).is_err());
    }

    #[test]
    fn rejects_an_exfat_oem_signature_as_unsupported() {
        let mut boot = [0; 512];
        boot[3..11].copy_from_slice(b"EXFAT   ");
        assert!(matches!(
            Fat32::mount(MemoryReader::with_sector(0, boot)),
            Err(FatError::Unsupported)
        ));
    }

    // Catches open_file not resolving paths, accepting directories, or losing
    // the entry's cluster and size.
    #[test]
    fn open_file_returns_a_descriptor_and_rejects_directories() {
        let mut fs = mounted_nested_fixture();
        let desc = fs.open_file("SUBDIR/NOTE.TXT").unwrap();
        assert_eq!(desc.size, 11);

        assert!(matches!(fs.open_file("SUBDIR"), Err(FatError::IsDirectory)));
        assert!(matches!(fs.open_file("MISSING"), Err(FatError::NotFound)));
        assert!(matches!(fs.open_file("A//B"), Err(FatError::InvalidName)));
    }

    // Catches read_range skipping the wrong clusters, ignoring the output
    // bound, or not reporting EOF.
    #[test]
    fn read_range_streams_from_any_offset_within_the_chain() {
        let mut content = [0x30u8; 700];
        for (index, byte) in content.iter_mut().enumerate() {
            *byte = b'0' + (index % 10) as u8;
        }
        let mut fs = mounted_multicluster_file_fixture(&content);
        let desc = fs.open_file("HELLO.TXT").unwrap();

        let mut output = [0; 700];
        assert_eq!(fs.read_range(&desc, 0, &mut output).unwrap(), 700);
        assert_eq!(output, content);

        // cluster境界をまたぐoffset: 1 byte目がcluster 5の先頭。
        let mut tail = [0; 700];
        assert_eq!(fs.read_range(&desc, 500, &mut tail).unwrap(), 200);
        assert_eq!(tail[..200], content[500..]);

        // file末尾を超える要求はsizeで打ち切る。
        let mut end = [0; 700];
        assert_eq!(fs.read_range(&desc, 690, &mut end).unwrap(), 10);
        assert_eq!(end[..10], content[690..]);

        assert_eq!(fs.read_range(&desc, 700, &mut end).unwrap(), 0);
        assert_eq!(fs.read_range(&desc, 9000, &mut end).unwrap(), 0);
        assert_eq!(fs.read_range(&desc, 0, &mut []).unwrap(), 0);
    }

    // Catches read_range treating an empty file's cluster 0 as a chain start
    // or following a chain past the mounted bound.
    #[test]
    fn read_range_handles_empty_files_and_corrupt_chains() {
        let mut fs = mounted_empty_file_fixture();
        let desc = fs.open_file("EMPTY.TXT").unwrap();
        let mut output = [0xaa; 8];
        assert_eq!(fs.read_range(&desc, 0, &mut output).unwrap(), 0);

        // cluster 0を指す破損descはchainへ入る前にCorruptChain。
        let bad = FileDesc {
            first_cluster: 0,
            size: 10,
            dir_cluster: 2,
            dir_index: 0,
        };
        assert!(matches!(
            fs.read_range(&bad, 0, &mut output),
            Err(FatError::CorruptChain)
        ));

        // offsetがcluster boundを超える場合も同じくCorruptChain。
        let mut cyclic = mounted_cyclic_file_fixture();
        let desc = cyclic.open_file("LOOP.BIN").unwrap();
        assert!(matches!(
            cyclic.read_range(&desc, u64::from(u32::MAX) - 512, &mut output),
            Err(FatError::CorruptChain)
        ));
    }

    /// write検証用の最小volume。`fat_count=1`、`fat_sectors=1`なので
    /// data領域はlba 33から始まり、cluster `n`はlba `31 + n`。
    /// fatはentry 0..=127だけを記述し、2(root)/4(HELLO)/5(SUBDIR)を
    /// 使用済みとしてマークする。
    fn writable_boot_sector() -> [u8; 512] {
        let mut boot = valid_boot_sector();
        write_u16(&mut boot, 14, 32);
        boot[16] = 1;
        write_u32(&mut boot, 36, 1);
        write_u32(&mut boot, 32, 66_000);
        boot
    }

    fn writable_fat() -> [u8; 512] {
        let mut fat = [0; 512];
        write_fat_entry(&mut fat, 2, 0x0fff_ffff);
        write_fat_entry(&mut fat, 4, 0x0fff_ffff);
        write_fat_entry(&mut fat, 5, 0x0fff_ffff);
        fat
    }

    /// root: [0]=HELLO.TXT(cluster 4, 11B), [1]=SUBDIR(cluster 5),
    /// [2]=終端。cluster 3と6以降はfree。
    fn mounted_writable_fixture() -> Fat32<MemoryReader<10>> {
        let mut root = [0; 512];
        write_directory_entry(&mut root, 0, b"HELLO   TXT", 0x20, 4, 11);
        write_directory_entry(&mut root, 1, b"SUBDIR     ", 0x10, 5, 0);

        let mut hello = [0; 512];
        hello[..11].copy_from_slice(b"hello world");

        let mut subdir = [0; 512];
        write_directory_entry(&mut subdir, 0, b".          ", 0x10, 5, 0);
        write_directory_entry(&mut subdir, 1, b"..         ", 0x10, 2, 0);

        Fat32::mount(MemoryReader::with_sectors([
            (0, writable_boot_sector()),
            (32, writable_fat()),
            (33, root),
            (34, [0; 512]),
            (35, hello),
            (36, subdir),
            (37, [0; 512]),
            (38, [0; 512]),
            (39, [0; 512]),
            (40, [0; 512]),
        ]))
        .unwrap()
    }

    fn sector_of<const N: usize>(fs: &Fat32<MemoryReader<N>>, lba: u32) -> &[u8; 512] {
        &fs.reader
            .sectors
            .iter()
            .find(|(sector_lba, _)| *sector_lba == lba)
            .unwrap()
            .1
    }

    #[test]
    fn short_name_normalizes_and_rejects_unconvertible_names() {
        assert_eq!(to_short_name("guest.txt"), Some(*b"GUEST   TXT"));
        assert_eq!(to_short_name("A"), Some(*b"A          "));
        assert_eq!(to_short_name("A.B"), Some(*b"A       B  "));
        assert_eq!(to_short_name("MYFILE~1.DAT"), Some(*b"MYFILE~1DAT"));
        assert!(to_short_name("long file.txt").is_none());
        assert!(to_short_name("TOOLONGNM.TXT").is_none());
        assert!(to_short_name("A.TXTEX").is_none());
        assert!(to_short_name("A.B.C").is_none());
        assert!(to_short_name(".hidden").is_none());
    }

    // Catches create_file writing a malformed entry, skipping the
    // terminator, or failing to match an existing normalized name.
    #[test]
    fn create_file_writes_entry_and_reuses_existing() {
        let mut fs = mounted_writable_fixture();
        let desc = fs.create_file("new.txt").unwrap();
        assert_eq!((desc.first_cluster, desc.size), (0, 0));
        assert_eq!((desc.dir_cluster, desc.dir_index), (2, 2));

        // 書かれたentryの名前・属性と、次slotの終端印を確認する。
        let root = sector_of(&fs, 33);
        assert_eq!(&root[64..75], b"NEW     TXT");
        assert_eq!(root[75], 0x20);
        assert_eq!(root[96], 0x00);

        let mut names = Vec::new();
        fs.for_each_root_entry(|entry| names.push(String::from(entry.name())));
        assert!(names.contains(&String::from("NEW.TXT")));

        // 大文字で作り直してもLFNではなく同じentryを返し、subdirの
        // directory名や8.3に直せない名前は拒否する。
        let again = fs.create_file("NEW.TXT").unwrap();
        assert_eq!((again.dir_cluster, again.dir_index), (2, 2));
        assert!(matches!(
            fs.create_file("subdir"),
            Err(FatError::IsDirectory)
        ));
        assert!(matches!(
            fs.create_file("long file.txt"),
            Err(FatError::InvalidName)
        ));
        assert!(matches!(
            fs.create_file("HELLO.TXT/CHILD.TXT"),
            Err(FatError::NotDirectory)
        ));
    }

    // Catches write_range skipping cluster allocation, writing the wrong
    // LBA, or forgetting the dir entry write-back.
    #[test]
    fn write_range_allocates_clusters_and_syncs_dir_entry() {
        let mut fs = mounted_writable_fixture();
        let mut desc = fs.create_file("NEW.TXT").unwrap();
        assert_eq!(fs.write_range(&mut desc, 0, b"hi").unwrap(), 2);
        assert_eq!((desc.first_cluster, desc.size), (3, 2));

        // data sectorへ内容が書かれ、残りは割当時のzero-fillのまま。
        let data = sector_of(&fs, 34);
        assert_eq!(&data[..4], b"hi\0\0");
        // dir entryがcluster 3・size 2へ更新されている。
        let root = sector_of(&fs, 33);
        assert_eq!(read_u32(root, 64 + 28), 2);
        assert_eq!(read_u16(root, 64 + 26), 3);

        // 開き直して同じ内容が読める。
        let reopened = fs.open_file("NEW.TXT").unwrap();
        let mut buf = [0; 8];
        assert_eq!(fs.read_range(&reopened, 0, &mut buf).unwrap(), 2);
        assert_eq!(&buf[..2], b"hi");
    }

    // Catches write_range breaking at a cluster boundary, corrupting the
    // FAT chain, or accepting a sparse offset.
    #[test]
    fn write_range_extends_across_clusters_and_rejects_sparse_offsets() {
        let mut fs = mounted_writable_fixture();
        let mut desc = fs.create_file("NEW.TXT").unwrap();
        let data = [0xab; 700];
        assert_eq!(fs.write_range(&mut desc, 0, &data).unwrap(), 700);
        assert_eq!(desc.size, 700);

        // spc=1なのでcluster 3と4を繋ぐはずだったが、4はHELLO.TXT使用中。
        // 実際は3と6が割り当てられ、fat[3]=6・fat[6]=EOCになる。
        let fat = sector_of(&fs, 32);
        assert_eq!(read_u32(fat, 3 * 4) & 0x0fff_ffff, 6);
        assert_eq!(read_u32(fat, 6 * 4) & 0x0fff_ffff, 0x0fff_ffff);
        assert_eq!(&sector_of(&fs, 34)[..4], &[0xab; 4]);
        assert_eq!(&sector_of(&fs, 37)[..4], &[0xab; 4]);
        // 2 cluster目は188 byteだけ書かれ、残りは割当時のzero-fillのまま。
        assert_eq!(
            &sector_of(&fs, 37)[184..192],
            &[0xab, 0xab, 0xab, 0xab, 0, 0, 0, 0]
        );

        // cluster境界をまたぐ部分を読み戻す。
        let reopened = fs.open_file("NEW.TXT").unwrap();
        let mut buf = [0; 700];
        assert_eq!(fs.read_range(&reopened, 0, &mut buf).unwrap(), 700);
        assert_eq!(buf, data);

        // sizeを越えるoffsetは穴あきwriteになるため拒否する。
        let mut desc = fs.create_file("NEW.TXT").unwrap();
        assert!(matches!(
            fs.write_range(&mut desc, 701, b"x"),
            Err(FatError::InvalidOffset)
        ));
        // 末尾への追記は受理され、sizeが延びる。
        assert_eq!(fs.write_range(&mut desc, 700, b"zz").unwrap(), 2);
        assert_eq!(desc.size, 702);
    }

    // Catches find_free_dir_slot failing to extend a full directory's
    // cluster chain.
    #[test]
    fn create_file_extends_a_full_directory() {
        let mut root = [0; 512];
        for index in 0..16u32 {
            let mut name = [b' '; 11];
            name[..6].copy_from_slice(b"FILE00");
            name[4] = b'0' + (index / 10) as u8;
            name[5] = b'0' + (index % 10) as u8;
            write_directory_entry(&mut root, index as usize, &name, 0x20, 10 + index, 0);
        }
        let mut fs = Fat32::mount(MemoryReader::with_sectors([
            (0, writable_boot_sector()),
            (32, writable_fat()),
            (33, root),
            (34, [0; 512]),
            (37, [0; 512]),
        ]))
        .unwrap();

        let desc = fs.create_file("NEW.TXT").unwrap();
        // root chainがcluster 3へ延び、先頭slotへentryが作られる。
        assert_eq!((desc.dir_cluster, desc.dir_index), (3, 0));
        let fat = sector_of(&fs, 32);
        assert_eq!(read_u32(fat, 2 * 4) & 0x0fff_ffff, 3);
        assert_eq!(read_u32(fat, 3 * 4) & 0x0fff_ffff, 0x0fff_ffff);
        let extended = sector_of(&fs, 34);
        assert_eq!(&extended[..11], b"NEW     TXT");
        assert_eq!(extended[32], 0x00);
    }

    // Catches create_file writing the terminator one slot past a
    // sector-boundary slot into the same scratch buffer (which would
    // overflow) instead of zeroing the following sector.
    #[test]
    fn create_file_moves_a_sector_boundary_terminator_to_the_next_sector() {
        // spc=2のvolume: cluster 2はlba 33と34、終端はslot 15（sector 0の
        // 末尾）にあり、次の終端はsector 1の先頭へ移るはずである。
        let mut boot = writable_boot_sector();
        boot[13] = 2;
        write_u32(&mut boot, 32, 131_100);

        let mut root_first = [0; 512];
        for index in 0..15u32 {
            let mut name = [b' '; 11];
            name[..6].copy_from_slice(b"FILE00");
            name[4] = b'0' + (index / 10) as u8;
            name[5] = b'0' + (index % 10) as u8;
            write_directory_entry(&mut root_first, index as usize, &name, 0x20, 10 + index, 0);
        }
        let mut root_second = [0; 512];
        root_second[7] = 0xaa; // stale garbage: 終端の次sectorは上書きで消す

        let mut fs = Fat32::mount(MemoryReader::with_sectors([
            (0, boot),
            (32, writable_fat()),
            (33, root_first),
            (34, root_second),
            (35, [0; 512]),
            (36, [0; 512]),
        ]))
        .unwrap();

        let desc = fs.create_file("NEW.TXT").unwrap();
        assert_eq!((desc.dir_cluster, desc.dir_index), (2, 15));
        // entryはsector 0の末尾へ、終端はsector 1の先頭へ書かれる。
        let first = sector_of(&fs, 33);
        assert_eq!(&first[480..491], b"NEW     TXT");
        assert!(sector_of(&fs, 34).iter().all(|byte| *byte == 0));
    }

    // Catches alloc_cluster ignoring a fully-used FAT or set_fat_entry
    // clobbering the reserved high nibble.
    #[test]
    fn exhausted_fat_reports_no_space() {
        let mut fat = [0; 512];
        for cluster in 0..128u32 {
            write_fat_entry(&mut fat, cluster, 0x0fff_ffff);
        }
        let mut fs = Fat32::mount(MemoryReader::with_sectors([
            (0, writable_boot_sector()),
            (32, fat),
            (33, [0; 512]),
        ]))
        .unwrap();

        let mut desc = fs.create_file("NEW.TXT").unwrap();
        assert!(matches!(
            fs.write_range(&mut desc, 0, b"x"),
            Err(FatError::NoSpace)
        ));
    }

    /// rootに`Long File Name.txt`（chain 4→5）を持つwritable fixture。
    /// LFN record 2個とshort entryがindex 0..=2に並び、index 3は終端。
    /// cluster 6以降はfree。
    fn mounted_unlink_lfn_fixture() -> Fat32<MemoryReader<7>> {
        let mut fat = writable_fat();
        write_fat_entry(&mut fat, 4, 5);

        let mut root = [0; 512];
        write_lfn_records(&mut root, 0, "Long File Name.txt", b"LONGFI~1TXT");
        write_directory_entry(&mut root, 2, b"LONGFI~1TXT", 0x20, 4, 13);

        let mut data_a = [0; 512];
        data_a[..7].copy_from_slice(b"long da");
        let mut data_b = [0; 512];
        data_b[..6].copy_from_slice(b"ta end");

        Fat32::mount(MemoryReader::with_sectors([
            (0, writable_boot_sector()),
            (32, fat),
            (33, root),
            (34, [0; 512]),
            (35, data_a),
            (36, data_b),
            (37, [0; 512]),
        ]))
        .unwrap()
    }

    // Catches unlink failing to delete the LFN records, leaking the
    // cluster chain, or leaving the deleted name resolvable.
    #[test]
    fn unlink_marks_the_entry_and_lfn_records_deleted_and_frees_the_chain() {
        let mut fs = mounted_unlink_lfn_fixture();
        assert_eq!(fs.unlink_file("Long File Name.txt").unwrap(), (2, 2));

        // LFN record 2個とshort entryが全て`0xe5`、終端はそのまま残る。
        let root = sector_of(&fs, 33);
        assert_eq!(root[0], 0xe5);
        assert_eq!(root[32], 0xe5);
        assert_eq!(root[64], 0xe5);
        assert_eq!(root[96], 0x00);

        // chain 4→5は解放され、rootのcluster 2は使われたまま。
        let fat = sector_of(&fs, 32);
        assert_eq!(read_u32(fat, 4 * 4) & 0x0fff_ffff, 0);
        assert_eq!(read_u32(fat, 5 * 4) & 0x0fff_ffff, 0);
        assert_eq!(read_u32(fat, 2 * 4) & 0x0fff_ffff, 0x0fff_ffff);

        // listingから消え、LFN名と8.3 aliasの両方が解決不能になる。
        let mut names = Vec::new();
        fs.for_each_root_entry(|entry| names.push(String::from(entry.name())))
            .unwrap();
        assert!(names.is_empty());
        assert!(matches!(
            fs.open_file("LONGFI~1.TXT"),
            Err(FatError::NotFound)
        ));
        assert!(matches!(
            fs.unlink_file("Long File Name.txt"),
            Err(FatError::NotFound)
        ));
    }

    // Catches unlink leaving the freed slot or clusters unavailable to a
    // later create.
    #[test]
    fn unlink_frees_the_chain_and_the_slot_is_reused() {
        let mut fs = mounted_writable_fixture();
        let mut desc = fs.create_file("NEW.TXT").unwrap();
        assert_eq!(fs.write_range(&mut desc, 0, &[0xab; 700]).unwrap(), 700);
        // write_rangeの検証と同じく、chainは3→6へ割り当てられた。
        assert_eq!(fs.unlink_file("new.txt").unwrap(), (2, 2));
        let fat = sector_of(&fs, 32);
        assert_eq!(read_u32(fat, 3 * 4) & 0x0fff_ffff, 0);
        assert_eq!(read_u32(fat, 6 * 4) & 0x0fff_ffff, 0);

        // 同じ名で作り直すと削除跡slotを再利用し、解放されたclusterが
        // 再び割り当てられる。
        let mut recreated = fs.create_file("NEW.TXT").unwrap();
        assert_eq!(recreated.dir_location(), (2, 2));
        assert_eq!(fs.write_range(&mut recreated, 0, b"ok").unwrap(), 2);
        assert_eq!(recreated.first_cluster, 3);
        let mut buf = [0; 4];
        let read_back = fs.open_file("NEW.TXT").unwrap();
        assert_eq!(fs.read_range(&read_back, 0, &mut buf).unwrap(), 2);
        assert_eq!(&buf[..2], b"ok");
    }

    // Catches unlink deleting a directory, reporting success on a
    // missing path, or marking the entry before validating the chain.
    #[test]
    fn unlink_rejects_directories_and_corrupt_chains_before_writing() {
        let mut fs = mounted_writable_fixture();
        assert!(matches!(
            fs.unlink_file("SUBDIR"),
            Err(FatError::IsDirectory)
        ));
        assert!(matches!(
            fs.unlink_file("MISSING.TXT"),
            Err(FatError::NotFound)
        ));
        // 拒否はdiskを変えない。
        assert_eq!(sector_of(&fs, 33)[0], b'H');

        // chainが自身へloopするfileは、entryを残したままCorruptChainで
        // 失敗する。
        let mut fat = writable_fat();
        write_fat_entry(&mut fat, 4, 4);
        let mut root = [0; 512];
        write_directory_entry(&mut root, 0, b"LOOP    TXT", 0x20, 4, 5);
        let mut fs = Fat32::mount(MemoryReader::with_sectors([
            (0, writable_boot_sector()),
            (32, fat),
            (33, root),
        ]))
        .unwrap();
        assert!(matches!(
            fs.unlink_file("LOOP.TXT"),
            Err(FatError::CorruptChain)
        ));
        assert_eq!(sector_of(&fs, 33)[0], b'L');
    }

    // Catches unlink treating an empty file's cluster 0 as a chain start.
    #[test]
    fn unlink_removes_an_empty_file_without_touching_the_fat() {
        let mut fs = mounted_writable_fixture();
        fs.create_file("NEW.TXT").unwrap();
        assert_eq!(fs.unlink_file("NEW.TXT").unwrap(), (2, 2));
        assert!(matches!(fs.open_file("NEW.TXT"), Err(FatError::NotFound)));
        // 空fileはclusterを持たないのでFATは無変更のまま。
        let fat = sector_of(&fs, 32);
        assert_eq!(read_u32(fat, 3 * 4) & 0x0fff_ffff, 0);
    }

    // Catches unlink stopping at the cluster boundary and leaving an LFN
    // record reachable again through the short name.
    #[test]
    fn unlink_deletes_an_lfn_run_spanning_a_cluster_boundary() {
        let mut fat = writable_fat();
        write_fat_entry(&mut fat, 2, 3);
        write_fat_entry(&mut fat, 3, 0x0fff_ffff);

        // "Long File Name.txt"のLFN recordはdisk上[seq2|0x40, seq1]の順。
        let mut lfn_scratch = [0; 512];
        write_lfn_records(&mut lfn_scratch, 0, "Long File Name.txt", b"LONGFI~1TXT");

        // cluster 2はfiller 15個とseq2 record、cluster 3はseq1 recordと
        // short entryと終端。LFN runはclusterをまたぐ。
        let mut first = [0; 512];
        for index in 0..15u32 {
            let mut name = [b' '; 11];
            name[..6].copy_from_slice(b"FILE00");
            name[4] = b'0' + (index / 10) as u8;
            name[5] = b'0' + (index % 10) as u8;
            write_directory_entry(&mut first, index as usize, &name, 0x20, 10 + index, 0);
        }
        first[15 * 32..16 * 32].copy_from_slice(&lfn_scratch[..32]);

        let mut second = [0; 512];
        second[..32].copy_from_slice(&lfn_scratch[32..64]);
        write_directory_entry(&mut second, 1, b"LONGFI~1TXT", 0x20, 4, 13);

        let mut fs = Fat32::mount(MemoryReader::with_sectors([
            (0, writable_boot_sector()),
            (32, fat),
            (33, first),
            (34, second),
            (35, [0; 512]),
        ]))
        .unwrap();

        assert_eq!(fs.unlink_file("LONGFI~1.TXT").unwrap(), (3, 1));
        // seq2 recordはcluster 2側で消え、filler entryは残る。
        assert_eq!(sector_of(&fs, 33)[15 * 32], 0xe5);
        assert_eq!(sector_of(&fs, 33)[0], b'F');
        // seq1 recordとshort entryはcluster 3側で消える。
        let second = sector_of(&fs, 34);
        assert_eq!(second[0], 0xe5);
        assert_eq!(second[32], 0xe5);
        assert_eq!(second[64], 0x00);
        assert_eq!(read_u32(sector_of(&fs, 32), 4 * 4) & 0x0fff_ffff, 0);
    }

    // Catches rename moving the entry, corrupting the name bytes, or
    // freeing the chain: the entry must stay at its slot with only the
    // 8.3 name rewritten and contents intact.
    #[test]
    fn rename_rewrites_the_entry_name_and_preserves_contents() {
        let mut fs = mounted_writable_fixture();
        assert_eq!(fs.rename_file("HELLO.TXT", "GREET.TXT").unwrap(), None);

        let root = sector_of(&fs, 33);
        assert_eq!(&root[0..11], b"GREET   TXT");
        assert_eq!(root[11], 0x20);
        // 旧名は解決不能、新名は同じdir位置と内容を持つ。
        assert!(matches!(fs.open_file("HELLO.TXT"), Err(FatError::NotFound)));
        let desc = fs.open_file("GREET.TXT").unwrap();
        assert_eq!(desc.dir_location(), (2, 0));
        let mut buf = [0; 11];
        assert_eq!(fs.read_range(&desc, 0, &mut buf).unwrap(), 11);
        assert_eq!(&buf, b"hello world");
    }

    // Catches rename duplicating the entry or failing on a same-name
    // rename: renaming an entry to its own name must be a no-op success.
    #[test]
    fn rename_to_the_same_name_is_a_noop() {
        let mut fs = mounted_writable_fixture();
        assert_eq!(fs.rename_file("HELLO.TXT", "hello.txt").unwrap(), None);
        assert_eq!(&sector_of(&fs, 33)[0..11], b"HELLO   TXT");
        let desc = fs.open_file("HELLO.TXT").unwrap();
        assert_eq!(desc.size, 11);
    }

    // Catches rename leaving the replaced file reachable or leaking its
    // chain: the target's entry must be deleted, its clusters freed, and
    // the caller must learn the deleted slot for fd revocation.
    #[test]
    fn rename_replaces_an_existing_file_and_reports_its_slot() {
        let mut fs = mounted_writable_fixture();
        let mut desc = fs.create_file("OLD.TXT").unwrap();
        assert_eq!(fs.write_range(&mut desc, 0, b"new data").unwrap(), 8);

        assert_eq!(
            fs.rename_file("OLD.TXT", "HELLO.TXT").unwrap(),
            Some((2, 0))
        );

        let root = sector_of(&fs, 33);
        // 旧HELLOのslotは`0xe5`、chain 4は解放され、sourceは旧名を引き継ぐ。
        assert_eq!(root[0], 0xe5);
        assert_eq!(&root[2 * 32..2 * 32 + 11], b"HELLO   TXT");
        let fat = sector_of(&fs, 32);
        assert_eq!(read_u32(fat, 4 * 4) & 0x0fff_ffff, 0);
        assert_eq!(read_u32(fat, 3 * 4) & 0x0fff_ffff, 0x0fff_ffff);

        let desc = fs.open_file("HELLO.TXT").unwrap();
        assert_eq!(desc.dir_location(), (2, 2));
        let mut buf = [0; 8];
        assert_eq!(fs.read_range(&desc, 0, &mut buf).unwrap(), 8);
        assert_eq!(&buf, b"new data");
    }

    // Catches rename deleting the entry along with its LFN records: only
    // the LFN run may be deleted; the renamed entry keeps its slot and
    // cluster chain.
    #[test]
    fn rename_drops_the_lfn_run_but_keeps_the_entry() {
        let mut fs = mounted_unlink_lfn_fixture();
        assert_eq!(
            fs.rename_file("Long File Name.txt", "NEW.TXT").unwrap(),
            None
        );

        let root = sector_of(&fs, 33);
        assert_eq!(root[0], 0xe5);
        assert_eq!(root[32], 0xe5);
        assert_eq!(&root[64..75], b"NEW     TXT");
        // chain 4→5はfileが生きているので解放されない。
        let fat = sector_of(&fs, 32);
        assert_eq!(read_u32(fat, 4 * 4) & 0x0fff_ffff, 5);
        assert_eq!(read_u32(fat, 5 * 4) & 0x0fff_ffff, 0x0fff_ffff);

        assert!(matches!(
            fs.open_file("LONGFI~1.TXT"),
            Err(FatError::NotFound)
        ));
        let desc = fs.open_file("NEW.TXT").unwrap();
        assert_eq!(desc.size, 13);
        assert_eq!(desc.dir_location(), (2, 2));
        let mut buf = [0; 7];
        assert_eq!(fs.read_range(&desc, 0, &mut buf).unwrap(), 7);
        assert_eq!(&buf, b"long da");
    }

    // Catches rename accepting a directory, crossing directories, or
    // writing before validating: every rejection must leave the disk
    // untouched.
    #[test]
    fn rename_rejects_directories_cross_dir_and_bad_names_before_writing() {
        let mut fs = mounted_writable_fixture();
        assert!(matches!(
            fs.rename_file("SUBDIR", "X.TXT"),
            Err(FatError::IsDirectory)
        ));
        assert!(matches!(
            fs.rename_file("HELLO.TXT", "SUBDIR/X.TXT"),
            Err(FatError::CrossDirectory)
        ));
        assert!(matches!(
            fs.rename_file("HELLO.TXT", "SUBDIR"),
            Err(FatError::IsDirectory)
        ));
        assert!(matches!(
            fs.rename_file("HELLO.TXT", "long name.txt"),
            Err(FatError::InvalidName)
        ));
        assert!(matches!(
            fs.rename_file("MISSING.TXT", "X.TXT"),
            Err(FatError::NotFound)
        ));
        // 拒否はdiskを変えない。
        let root = sector_of(&fs, 33);
        assert_eq!(&root[0..11], b"HELLO   TXT");
        assert_eq!(&root[32..43], b"SUBDIR     ");
    }

    // Catches the parent-directory check comparing the wrong thing:
    // rename inside a subdirectory must be allowed (same dir_head) while
    // moving the same entry to root must report CrossDirectory.
    #[test]
    fn rename_inside_a_subdirectory_is_allowed() {
        let mut fs = mounted_nested_fixture();
        assert!(matches!(
            fs.rename_file("SUBDIR/NOTE.TXT", "NEW.TXT"),
            Err(FatError::CrossDirectory)
        ));
        assert_eq!(
            fs.rename_file("SUBDIR/NOTE.TXT", "SUBDIR/NEW.TXT").unwrap(),
            None
        );

        let subdir = sector_of(&fs, 291);
        assert_eq!(&subdir[64..75], b"NEW     TXT");
        assert!(matches!(
            fs.open_file("SUBDIR/NOTE.TXT"),
            Err(FatError::NotFound)
        ));
        let desc = fs.open_file("SUBDIR/NEW.TXT").unwrap();
        assert_eq!(desc.size, 11);
        assert_eq!(desc.dir_location(), (5, 2));
    }

    /// rootにLFN名のdirectory（cluster 4、空）を持つfixture。
    /// `remove_dir`がLFN runごとentryを削除することを確認するために使う。
    fn mounted_lfn_dir_fixture() -> Fat32<MemoryReader<6>> {
        let mut fat = writable_fat();
        write_fat_entry(&mut fat, 4, 0x0fff_ffff);

        let mut root = [0; 512];
        write_lfn_records(&mut root, 0, "My Very Long Dir", b"MYVERY~1   ");
        write_directory_entry(&mut root, 2, b"MYVERY~1   ", 0x10, 4, 0);

        let mut dir = [0; 512];
        write_directory_entry(&mut dir, 0, b".          ", 0x10, 4, 0);
        write_directory_entry(&mut dir, 1, b"..         ", 0x10, 0, 0);

        Fat32::mount(MemoryReader::with_sectors([
            (0, writable_boot_sector()),
            (32, fat),
            (33, root),
            (34, [0; 512]),
            (35, dir),
            (36, [0; 512]),
        ]))
        .unwrap()
    }

    // Catches create_dir forgetting the dot entries, mislinking `..`, or
    // failing to create a resolvable directory children can be placed in.
    #[test]
    fn create_dir_makes_a_resolvable_empty_directory() {
        let mut fs = mounted_writable_fixture();
        fs.create_dir("newdir").unwrap();

        // 親dir（root）のslot 2へ`NEWDIR` entry、attr 0x10、新cluster 3。
        let root = sector_of(&fs, 33);
        assert_eq!(&root[64..75], b"NEWDIR     ");
        assert_eq!(root[75], 0x10);
        assert_eq!(read_u16(root, 64 + 26), 3);
        assert_eq!(read_u32(root, 64 + 28), 0);

        // 新cluster先頭は`.`（自己）と`..`（root親なら規約どおり0）。
        let dir = sector_of(&fs, 34);
        assert_eq!(&dir[0..11], b".          ");
        assert_eq!(dir[11], 0x10);
        assert_eq!(read_u16(dir, 26), 3);
        assert_eq!(&dir[32..43], b"..         ");
        assert_eq!(dir[43], 0x10);
        assert_eq!(read_u16(dir, 58), 0);
        assert_eq!(dir[64], 0x00);

        // pathとして解決でき、その中へfileを作れる。
        let desc = fs.create_file("NEWDIR/F.TXT").unwrap();
        assert_eq!((desc.dir_cluster, desc.dir_index), (3, 2));
        let dir = sector_of(&fs, 34);
        assert_eq!(&dir[64..75], b"F       TXT");
        assert_eq!(dir[75], 0x20);
        assert_eq!(dir[96], 0x00);
    }

    // Catches create_dir overwriting an existing entry or leaving its
    // allocated cluster behind when the parent directory has no slot.
    #[test]
    fn create_dir_rejects_existing_names_and_rolls_back_on_full_parent() {
        let mut fs = mounted_writable_fixture();
        fs.create_dir("NEWDIR").unwrap();
        assert!(matches!(fs.create_dir("NEWDIR"), Err(FatError::Exists)));
        assert!(matches!(fs.create_dir("hello.txt"), Err(FatError::Exists)));
        assert!(matches!(fs.create_dir("SUBDIR"), Err(FatError::Exists)));
        assert!(matches!(
            fs.create_dir("HELLO.TXT/SUB"),
            Err(FatError::NotDirectory)
        ));
        assert!(matches!(
            fs.create_dir("long dir"),
            Err(FatError::InvalidName)
        ));
        assert!(matches!(fs.create_dir("."), Err(FatError::InvalidName)));

        // 親dir満杯・FATにclusterが1つだけfreeのvolumeで、slot探索の
        // 失敗が新dir用clusterをfreeへ戻すことを確認する。
        let mut fat = [0; 512];
        for cluster in 2..128u32 {
            write_fat_entry(&mut fat, cluster, 0x0fff_ffff);
        }
        write_fat_entry(&mut fat, 3, 0);
        let mut root = [0; 512];
        for index in 0..16 {
            write_directory_entry(&mut root, index, b"F       TXT", 0x20, 4, 0);
        }
        let mut fs = Fat32::mount(MemoryReader::with_sectors([
            (0, writable_boot_sector()),
            (32, fat),
            (33, root),
            (34, [0; 512]),
        ]))
        .unwrap();
        assert!(matches!(fs.create_dir("ZZZ"), Err(FatError::NoSpace)));
        assert_eq!(read_u32(sector_of(&fs, 32), 3 * 4) & 0x0fff_ffff, 0);
        assert_eq!(&sector_of(&fs, 33)[0..11], b"F       TXT");
    }

    // Catches remove_dir leaking the chain, leaving the entry live, or
    // accepting a non-empty directory, a file, or a missing path.
    #[test]
    fn remove_dir_deletes_empty_dirs_and_rejects_nonempty_ones() {
        let mut fs = mounted_writable_fixture();
        fs.create_dir("NEWDIR").unwrap();
        // fixtureのSUBDIR（`.`/`..`のみ）も空として削除できる。
        assert_eq!(fs.remove_dir("SUBDIR").unwrap(), (2, 1));
        assert_eq!(fs.remove_dir("NEWDIR").unwrap(), (2, 2));

        let root = sector_of(&fs, 33);
        assert_eq!(root[32], 0xe5);
        assert_eq!(root[64], 0xe5);
        let fat = sector_of(&fs, 32);
        assert_eq!(read_u32(fat, 5 * 4) & 0x0fff_ffff, 0);
        assert_eq!(read_u32(fat, 3 * 4) & 0x0fff_ffff, 0);
        assert!(matches!(fs.open_file("NEWDIR"), Err(FatError::NotFound)));

        // 削除跡slotは次のcreateへ再利用される。
        let desc = fs.create_file("NEW.TXT").unwrap();
        assert_eq!((desc.dir_cluster, desc.dir_index), (2, 1));
    }

    // Catches remove_dir touching a file, a missing path, a directory
    // that still holds entries, or an LFN-named directory's records.
    #[test]
    fn remove_dir_rejects_files_missing_paths_and_lfn_dir_runs() {
        let mut fs = mounted_nested_fixture();
        // SUBDIRにはNOTE.TXTが残っている。
        assert!(matches!(fs.remove_dir("SUBDIR"), Err(FatError::NotEmpty)));
        assert!(matches!(
            fs.remove_dir("HELLO.TXT"),
            Err(FatError::NotDirectory)
        ));
        assert!(matches!(fs.remove_dir("MISSING"), Err(FatError::NotFound)));
        // 拒否はdiskを変えない。
        assert_eq!(&sector_of(&fs, 291)[0..11], b".          ");

        // LFN名のdirはrecord列ごと消える。
        let mut fs = mounted_lfn_dir_fixture();
        assert_eq!(fs.remove_dir("My Very Long Dir").unwrap(), (2, 2));
        let root = sector_of(&fs, 33);
        assert_eq!(root[0], 0xe5);
        assert_eq!(root[32], 0xe5);
        assert_eq!(root[64], 0xe5);
        assert_eq!(read_u32(sector_of(&fs, 32), 4 * 4) & 0x0fff_ffff, 0);
    }
}
