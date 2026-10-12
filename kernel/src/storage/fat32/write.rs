use crate::storage::{SectorReader, SectorWriter};

use super::parse::{is_valid_component, read_u16, read_u32, to_short_name, write_u16, write_u32};
use super::{
    DIR_ATTR_DIR, DIR_ATTR_FILE, DOT_DOT_SHORT_NAME, DOT_SHORT_NAME, DirEntry, DirLoc,
    DirRecordPos, FAT_EOC, FSINFO_LEAD_SIG, FSINFO_STRUCT_SIG, FSINFO_TRAIL_SIG, FSINFO_UNKNOWN,
    Fat32, FatError, FileDesc, RenameOutcome,
};

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
    pub(super) fn alloc_cluster(
        &mut self,
        scratch: &mut [u8; 512],
    ) -> Result<u32, FatError<R::Error>> {
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

    /// BPBのFSInfo欄がvolume内を指すなら、そのsectorを「free/next不明」の
    /// 正しいFSInfoへ一度だけ揃える。このdriverはFSInfoを一切読まずfree
    /// clusterをFAT走査で決めるため、実値を維持するcostを払わない。
    /// 古い実測値を残すと他のreaderや`fsck`へ誤情報を配り続けるため、
    /// 最初のFAT更新でunknownへ揃える。signatureが欠けるsector（専用領域の
    /// 未初期化を含む）へは正しいFSInfoをまるごと書く。
    fn mark_fsinfo_unknown(&mut self, scratch: &mut [u8; 512]) -> Result<(), FatError<R::Error>> {
        if self.fsinfo_marked || self.fsinfo_lba == 0 {
            return Ok(());
        }
        if let Ok(()) = self.reader.read_sector(self.fsinfo_lba, scratch)
            && read_u32(scratch, 0) == FSINFO_LEAD_SIG
            && read_u32(scratch, 484) == FSINFO_STRUCT_SIG
            && read_u32(scratch, 488) == FSINFO_UNKNOWN
            && read_u32(scratch, 492) == FSINFO_UNKNOWN
            && read_u32(scratch, 508) == FSINFO_TRAIL_SIG
        {
            self.fsinfo_marked = true;
            return Ok(());
        }
        scratch.fill(0);
        write_u32(scratch, 0, FSINFO_LEAD_SIG);
        write_u32(scratch, 484, FSINFO_STRUCT_SIG);
        write_u32(scratch, 488, FSINFO_UNKNOWN);
        write_u32(scratch, 492, FSINFO_UNKNOWN);
        write_u32(scratch, 508, FSINFO_TRAIL_SIG);
        self.reader
            .write_sector(self.fsinfo_lba, scratch)
            .map_err(FatError::Read)?;
        self.fsinfo_marked = true;
        Ok(())
    }

    /// FATの`cluster`番目のentryへ`value`を書く。上位4 bitの予約位は
    /// 保持する。第1 FATのsectorを読んで書き換え、同じsectorを全FAT面の
    /// 同位置へ書く。第1面の書き込み後に第2面で失敗するとエラーを返すが、
    /// 第1面は戻さないため、その時点でFAT面は食い違ったまま残る。
    pub(super) fn set_fat_entry(
        &mut self,
        cluster: u32,
        value: u32,
        scratch: &mut [u8; 512],
    ) -> Result<(), FatError<R::Error>> {
        self.mark_fsinfo_unknown(scratch)?;
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
        for copy in 0..self.fat_count {
            let copy_lba = copy
                .checked_mul(self.fat_sectors)
                .and_then(|offset| lba.checked_add(offset))
                .ok_or(FatError::InvalidFilesystem)?;
            self.reader
                .write_sector(copy_lba, scratch)
                .map_err(FatError::Read)?;
        }
        Ok(())
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
    pub(super) fn find_free_dir_slot(
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

    /// `old_path`のentryを`new_path`へ移す。fileとdirectoryのどちらに
    /// も使え、同一directory内ではin-place改名、別directoryへの指定は
    /// 移動になる。in-place改名ではentryの物理位置が不変なのでfileを
    /// 指すfdは有効のまま（POSIXのrename不変条件）、移動では`moved`
    /// へ新旧のlocを返すためcallerがfdのwrite-back先を追従させる。
    /// 既存の同名entryはPOSIX置換規約で削除してから書き換え、その
    /// 物理位置を`replaced`へ返す。
    ///
    /// file→fileは無条件に置換、file→dirは`IsDirectory`、dir→fileは
    /// `NotDirectory`、dir→dirはtargetが空のときのみ置換（非空は
    /// `NotEmpty`）。dirを自身または子孫の中へ移す指定は
    /// `MoveIntoItself`で拒否する。移動したdirの`..`は新しい親の
    /// cluster番号（root親なら0）へ更新する。新名が8.3へ正規化
    /// できなければ`InvalidName`として拒否する。全検証を書き込み前に
    /// 済ませるのは`unlink_file`と同じ規約である。
    pub fn rename(
        &mut self,
        old_path: &str,
        new_path: &str,
    ) -> Result<RenameOutcome, FatError<R::Error>> {
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
        let new_dir = if new_parent.is_empty() {
            self.root_cluster
        } else {
            let (parent, _ploc) = self.resolve_path(new_parent, &mut scratch)?;
            if !parent.directory {
                return Err(FatError::NotDirectory);
            }
            parent.first_cluster
        };
        if new_dir == loc.dir_head {
            return self.rename_in_place(&loc, &short, entry.directory, &mut scratch);
        }

        // 移すdirの中に自分を入れると`..`chainが閉じるため拒否する。
        if entry.directory {
            self.check_move_not_into_self(entry.first_cluster, new_dir, &mut scratch)?;
        }
        let replaced = self.check_rename_target(
            entry.directory,
            new_dir,
            &short,
            (loc.cluster, loc.index),
            &mut scratch,
        )?;

        // target dirのfree slotを特定する。chain拡張が起きても空の
        // 終端済みclusterが残るだけなので、後続の検証失敗を許容できる。
        let (tcluster, tindex, terminator) = self.find_free_dir_slot(new_dir, &mut scratch)?;

        // sourceのLFN runを確定し、recordの32 byteを確保する。dirの
        // `..`recordが期待位置にあることも書き込み前に検証する。
        let (run_start, run_end) = self.lfn_run_bounds(&loc, &mut scratch)?;
        let mut record = [0u8; 32];
        {
            let lba = self.data_sector_lba(loc.cluster, loc.index / 16)?;
            self.reader
                .read_sector(lba, &mut scratch)
                .map_err(FatError::Read)?;
            let offset = (loc.index % 16) as usize * 32;
            record.copy_from_slice(&scratch[offset..offset + 32]);
        }
        if entry.directory {
            self.read_dotdot(entry.first_cluster, &mut scratch)?;
        }

        // ここから書き込み。source runを消し、置換対象を削除し、target
        // slotへ複写したrecordを置き、dirなら`..`を新parentへ更新する。
        self.mark_deleted_range(loc.dir_head, run_start, run_end, &mut scratch)?;
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
        self.place_dir_entry(tcluster, tindex, terminator, &mut scratch, |slot| {
            slot.copy_from_slice(&record);
            slot[..11].copy_from_slice(&short);
        })?;
        if entry.directory {
            let parent = if new_dir == self.root_cluster {
                0
            } else {
                new_dir
            };
            self.update_dotdot(entry.first_cluster, parent, &mut scratch)?;
        }
        Ok(RenameOutcome {
            moved: Some(((loc.cluster, loc.index), (tcluster, tindex))),
            replaced: replaced.map(|(_, tloc)| (tloc.cluster, tloc.index)),
        })
    }

    /// 同一dir内のin-place改名。entryの物理位置は不変なので`moved`は
    /// 返さず、置換があれば`replaced`のみ返す。
    fn rename_in_place(
        &mut self,
        loc: &DirLoc,
        short: &[u8; 11],
        source_is_dir: bool,
        scratch: &mut [u8; 512],
    ) -> Result<RenameOutcome, FatError<R::Error>> {
        let replaced = self.check_rename_target(
            source_is_dir,
            loc.dir_head,
            short,
            (loc.cluster, loc.index),
            scratch,
        )?;

        // sourceのLFN runを消し、置換対象を削除してから、entryのname
        // byteを新8.3名へ書き換える。
        let (run_start, run_end) = self.lfn_run_bounds(loc, scratch)?;
        if run_start != (loc.cluster, loc.index) {
            self.mark_deleted_range(loc.dir_head, run_start, run_end, scratch)?;
        }
        if let Some((target, tloc)) = replaced {
            let (tstart, _) = self.lfn_run_bounds(&tloc, scratch)?;
            self.mark_deleted_range(tloc.dir_head, tstart, (tloc.cluster, tloc.index), scratch)?;
            self.free_chain(target.first_cluster, scratch)?;
        }
        let lba = self.data_sector_lba(loc.cluster, loc.index / 16)?;
        self.reader
            .read_sector(lba, scratch)
            .map_err(FatError::Read)?;
        let offset = (loc.index % 16) as usize * 32;
        scratch[offset..offset + 11].copy_from_slice(short);
        self.reader
            .write_sector(lba, scratch)
            .map_err(FatError::Read)?;
        Ok(RenameOutcome {
            moved: None,
            replaced: replaced.map(|(_, tloc)| (tloc.cluster, tloc.index)),
        })
    }

    /// `dir`内で`short`と衝突するentryを探し、dir/file matrixと
    /// 置換chainの検証を行う。衝突がなければ`None`、source自身と同じ
    /// entryならno-opを表す`None`、置換すべき相手は`(entry, loc)`を
    /// 返す。
    fn check_rename_target(
        &mut self,
        source_is_dir: bool,
        dir: u32,
        short: &[u8; 11],
        self_loc: (u32, u32),
        scratch: &mut [u8; 512],
    ) -> Result<Option<(DirEntry, DirLoc)>, FatError<R::Error>> {
        // 正規化済みのraw 8.3名で照合する（create_fileと同じ規約）。
        let mut found = None;
        self.walk_dir(dir, scratch, |candidate, cloc| {
            if candidate.short_name == *short {
                found = Some((*candidate, cloc));
                true
            } else {
                false
            }
        })?;
        let Some((target, tloc)) = found else {
            return Ok(None);
        };
        if (tloc.cluster, tloc.index) == self_loc {
            // 同じentryへのrenameはno-op成功。
            return Ok(None);
        }
        match (source_is_dir, target.directory) {
            // file→dirは上書き不可、dir→fileはPOSIXのENOTDIR。
            (false, true) => return Err(FatError::IsDirectory),
            (true, false) => return Err(FatError::NotDirectory),
            // dir→dirは空dirに限り置換。非空はENOTEMPTY。
            (true, true) => {
                let mut occupied = false;
                self.walk_dir(target.first_cluster, scratch, |_e, _l| {
                    occupied = true;
                    true
                })?;
                if occupied {
                    return Err(FatError::NotEmpty);
                }
            }
            (false, false) => {}
        }
        // 置換対象のchainが破損しているなら、何も触らずに失敗する。
        if target.first_cluster != 0 {
            if !self.is_data_cluster(target.first_cluster) {
                return Err(FatError::CorruptChain);
            }
            self.chain_tail(target.first_cluster, scratch)?;
        }
        Ok(Some((target, tloc)))
    }

    /// 移すdir `moved`のfirst_clusterが、新parent `parent`の`..`chain
    /// 上に現れたら`MoveIntoItself`で拒否する。`mv A A/...`と
    /// `mv A B/A`（BはAの子孫）を防ぐ。`..`が0またはroot clusterを
    /// 指した時点でrootへ到達したと見なし終了する。
    fn check_move_not_into_self(
        &mut self,
        moved_cluster: u32,
        mut parent: u32,
        scratch: &mut [u8; 512],
    ) -> Result<(), FatError<R::Error>> {
        for _ in 0..self.data_cluster_count {
            if parent == moved_cluster {
                return Err(FatError::MoveIntoItself);
            }
            if parent == self.root_cluster {
                return Ok(());
            }
            let next = self.read_dotdot(parent, scratch)?;
            if next == 0 || next == self.root_cluster {
                return Ok(());
            }
            parent = next;
        }
        // `..`chainがrootへ到達しない = 壊れたchain。
        Err(FatError::CorruptChain)
    }

    /// dirのfirst clusterのrecord index 1にある`..`のfirst_clusterを
    /// 読む。record名が`..         `でなければ`CorruptChain`。
    fn read_dotdot(
        &mut self,
        dir_cluster: u32,
        scratch: &mut [u8; 512],
    ) -> Result<u32, FatError<R::Error>> {
        let lba = self.data_sector_lba(dir_cluster, 0)?;
        self.reader
            .read_sector(lba, scratch)
            .map_err(FatError::Read)?;
        if scratch[32..43] != DOT_DOT_SHORT_NAME {
            return Err(FatError::CorruptChain);
        }
        let hi = u32::from(read_u16(scratch, 52));
        let lo = u32::from(read_u16(scratch, 58));
        Ok((hi << 16) | lo)
    }

    /// 移動したdirの`..`recordのfirst_clusterを`parent`へ書き換える。
    /// root親の場合はFAT32規約の0を格納する。
    fn update_dotdot(
        &mut self,
        dir_cluster: u32,
        parent: u32,
        scratch: &mut [u8; 512],
    ) -> Result<(), FatError<R::Error>> {
        let lba = self.data_sector_lba(dir_cluster, 0)?;
        self.reader
            .read_sector(lba, scratch)
            .map_err(FatError::Read)?;
        if scratch[32..43] != DOT_DOT_SHORT_NAME {
            return Err(FatError::CorruptChain);
        }
        write_u16(scratch, 52, (parent >> 16) as u16);
        write_u16(scratch, 58, parent as u16);
        self.reader
            .write_sector(lba, scratch)
            .map_err(FatError::Read)
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
