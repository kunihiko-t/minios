use crate::storage::SectorReader;

use super::parse::{
    PendingLfn, is_extended_partition, matches_component, normalize_input_name, parse_boot_sector,
    parse_dir_entry, read_u32,
};
#[cfg(not(target_arch = "riscv32"))]
use super::parse::is_valid_component;
use super::{DirEntry, DirLoc, Fat32, FatError, Geometry, ParseError};
#[cfg(not(target_arch = "riscv32"))]
use super::{FileDesc, FileStat};

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
            #[cfg(not(target_arch = "riscv32"))]
            fat_count: geometry.fat_count,
            #[cfg(not(target_arch = "riscv32"))]
            fsinfo_lba: geometry.fsinfo_lba,
            #[cfg(not(target_arch = "riscv32"))]
            fsinfo_marked: false,
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

    /// `path`が参照するdirectoryの`index`番目のentryを返す。`""`は
    /// root、indexが末尾を越えれば`None`。`for_each_entry`と同じ
    /// 解釈で、fileへの指定は`NotDirectory`を返す。`.`/`..`は
    /// 列挙に含まれない。index番目へ到達した時点で走査を打ち切る。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn entry_at(
        &mut self,
        path: &str,
        index: usize,
    ) -> Result<Option<DirEntry>, FatError<R::Error>> {
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
        let mut found = None;
        let mut seen = 0usize;
        self.walk_dir(dir_cluster, &mut scratch, |entry, _loc| {
            if seen == index {
                found = Some(*entry);
                true
            } else {
                seen += 1;
                false
            }
        })?;
        Ok(found)
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

    /// `path`を解決してsizeとdirectory bitを返す。`open_file`と違い
    /// directoryも受理し、file contentは読まない。
    #[cfg(not(target_arch = "riscv32"))]
    pub fn stat(&mut self, path: &str) -> Result<FileStat, FatError<R::Error>> {
        let mut scratch = [0; 512];
        let (entry, _) = self.resolve_path(path, &mut scratch)?;
        Ok(FileStat {
            size: entry.size,
            directory: entry.directory,
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
    pub(super) fn resolve_path(
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

    pub(super) fn walk_dir<F>(
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

    pub(super) fn is_data_cluster(&self, cluster: u32) -> bool {
        match self.data_cluster_count.checked_add(2) {
            Some(limit) => cluster >= 2 && cluster < limit,
            None => false,
        }
    }

    pub(super) fn data_sector_lba(
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

    pub(super) fn next_cluster(
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
