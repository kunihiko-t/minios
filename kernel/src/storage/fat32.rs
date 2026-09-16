use super::SectorReader;

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

/// `Fat32::open_file`が返すfile位置の記述子。fd tableへそのまま格納できる
/// plain dataであり、session自身への参照は持たない。
#[cfg(not(target_arch = "riscv32"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileDesc {
    first_cluster: u32,
    size: u32,
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
        self.walk_dir(self.root_cluster, &mut scratch, |entry| {
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
        let entry = self
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
            let entry = self.resolve_path(path, &mut scratch)?;
            if !entry.directory {
                return Err(FatError::NotDirectory);
            }
            entry.first_cluster
        };
        self.walk_dir(dir_cluster, &mut scratch, |entry| {
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
        let entry = self.resolve_path(path, &mut scratch)?;
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
        let entry = self.resolve_path(path, &mut scratch)?;
        if entry.directory {
            return Err(FatError::IsDirectory);
        }
        Ok(FileDesc {
            first_cluster: entry.first_cluster,
            size: entry.size,
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

    /// `path`をrootから順に解決し、最終要素のentryを返す。途中の要素は
    /// directory必須、空要素と`.`/`..`は`InvalidName`として拒否する。
    #[cfg(not(target_arch = "riscv32"))]
    fn resolve_path(
        &mut self,
        path: &str,
        scratch: &mut [u8; 512],
    ) -> Result<DirEntry, FatError<R::Error>> {
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
            let entry = self
                .find_entry_in(dir_cluster, part, scratch)?
                .ok_or(FatError::NotFound)?;
            if parts.peek().is_none() {
                return Ok(entry);
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
    ) -> Result<Option<DirEntry>, FatError<R::Error>> {
        let mut found = None;
        self.walk_dir(dir_cluster, scratch, |entry| {
            if matches_component(entry, part) {
                found = Some(*entry);
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
        F: FnMut(&DirEntry) -> bool,
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
                        if visit(&entry) {
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

    use super::{DirEntry, Fat32, FatError, FileDesc, lfn_checksum};
    use crate::storage::SectorReader;
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
}
