use super::*;

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

/// `mounted_writable_fixture`と同じ内容を`fat_count=2`で置いたvolume。
/// FATはlba 32と33の2面で、data領域はlba 34から始まり、cluster `n`は
/// lba `32 + n`。index 2がFAT第2面である。
fn mirrored_sectors() -> [(u32, [u8; 512]); 12] {
    let mut boot = writable_boot_sector();
    boot[16] = 2;

    let mut root = [0; 512];
    write_directory_entry(&mut root, 0, b"HELLO   TXT", 0x20, 4, 11);
    write_directory_entry(&mut root, 1, b"SUBDIR     ", 0x10, 5, 0);
    let mut hello = [0; 512];
    hello[..11].copy_from_slice(b"hello world");
    let mut subdir = [0; 512];
    write_directory_entry(&mut subdir, 0, b".          ", 0x10, 5, 0);
    write_directory_entry(&mut subdir, 1, b"..         ", 0x10, 2, 0);

    let mut sectors = [(0, [0; 512]); 12];
    sectors[0] = (0, boot);
    sectors[1] = (32, writable_fat());
    sectors[2] = (33, writable_fat());
    sectors[3] = (34, root);
    sectors[5] = (36, hello);
    sectors[6] = (37, subdir);
    for (index, sector) in sectors.iter_mut().enumerate().skip(4) {
        sector.0 = 31 + index as u32;
    }
    sectors
}

fn mounted_mirrored_fixture() -> Fat32<MemoryReader<12>> {
    Fat32::mount(MemoryReader::with_sectors(mirrored_sectors())).unwrap()
}

fn assert_fats_mirrored(fs: &Fat32<MemoryReader<12>>) {
    assert_eq!(sector_of(fs, 32), sector_of(fs, 33));
}

// Catches set_fat_entry updating only the first FAT: allocation by
// create+write and freeing by unlink must reach both copies.
#[test]
fn mirrored_fats_match_after_write_and_unlink() {
    let mut fs = mounted_mirrored_fixture();
    let mut desc = fs.create_file("NEW.TXT").unwrap();
    assert_eq!(fs.write_range(&mut desc, 0, &[0xab; 700]).unwrap(), 700);
    assert_eq!(read_u32(sector_of(&fs, 32), 3 * 4) & 0x0fff_ffff, 6);
    assert_fats_mirrored(&fs);

    fs.unlink_file("NEW.TXT").unwrap();
    assert_eq!(read_u32(sector_of(&fs, 32), 3 * 4) & 0x0fff_ffff, 0);
    assert_eq!(sector_of(&fs, 32), &writable_fat());
    assert_fats_mirrored(&fs);
}

#[test]
fn mirrored_fats_match_after_create_dir() {
    let mut fs = mounted_mirrored_fixture();
    fs.create_dir("NEWDIR").unwrap();
    assert_eq!(
        read_u32(sector_of(&fs, 32), 3 * 4) & 0x0fff_ffff,
        0x0fff_ffff
    );
    assert_fats_mirrored(&fs);
}

// rename over an existing file frees the replaced chain (cluster 4).
#[test]
fn mirrored_fats_match_after_rename_replace() {
    let mut fs = mounted_mirrored_fixture();
    let mut desc = fs.create_file("OLD.TXT").unwrap();
    assert_eq!(fs.write_range(&mut desc, 0, b"new data").unwrap(), 8);
    fs.rename("OLD.TXT", "HELLO.TXT").unwrap();
    assert_eq!(read_u32(sector_of(&fs, 32), 4 * 4) & 0x0fff_ffff, 0);
    assert_fats_mirrored(&fs);
}

// Catches the first FAT mutation leaving a stale-but-valid FSInfo
// untouched: the kernel never reads it, so it must be normalized to the
// spec-legal "unknown" instead of publishing old counts forever.
#[test]
fn first_fat_mutation_marks_stale_fsinfo_unknown() {
    let mut fs = mounted_fsinfo_fixture(fsinfo_sector(false));
    fs.create_dir("NEWDIR").unwrap();
    let fsinfo = sector_of(&fs, 1);
    assert_eq!(read_u32(fsinfo, 0), 0x4161_5252);
    assert_eq!(read_u32(fsinfo, 484), 0x6141_7272);
    assert_eq!(read_u32(fsinfo, 488), 0xffff_ffff);
    assert_eq!(read_u32(fsinfo, 492), 0xffff_ffff);
    assert_eq!(read_u32(fsinfo, 508), 0xaa55_0000);
}

// Catches a BPB that points at a sector never initialized as FSInfo
// (e.g. a freshly formatted image): the first mutation must write a
// complete valid FSInfo, signatures included.
#[test]
fn first_fat_mutation_repairs_an_uninitialized_fsinfo_sector() {
    let mut fs = mounted_fsinfo_fixture([0; 512]);
    fs.create_dir("NEWDIR").unwrap();
    let fsinfo = sector_of(&fs, 1);
    assert_eq!(read_u32(fsinfo, 0), 0x4161_5252);
    assert_eq!(read_u32(fsinfo, 484), 0x6141_7272);
    assert_eq!(read_u32(fsinfo, 488), 0xffff_ffff);
    assert_eq!(read_u32(fsinfo, 508), 0xaa55_0000);
}

// Catches the mark reaching volumes without an FSInfo pointer: writes
// must proceed without touching any extra sector (lba 1 is absent from
// the fixture, so a stray write would fail with MissingSector).
#[test]
fn volume_without_fsinfo_still_writes_normally() {
    let mut fs = mounted_writable_fixture();
    fs.create_dir("NEWDIR").unwrap();
    let mut desc = fs.create_file("NEW.TXT").unwrap();
    assert_eq!(fs.write_range(&mut desc, 0, &[0xab; 600]).unwrap(), 600);
    fs.unlink_file("NEW.TXT").unwrap();
}

// Catches allocation dropping a freed cluster: after unlink, the next
// file's growth must reuse free entries rather than skipping them.
#[test]
fn freed_cluster_is_reused_by_the_next_allocation() {
    let mut fs = mounted_writable_fixture();
    fs.unlink_file("HELLO.TXT").unwrap();
    assert_eq!(read_u32(sector_of(&fs, 32), 4 * 4) & 0x0fff_ffff, 0);
    let mut desc = fs.create_file("NEW.TXT").unwrap();
    assert_eq!(fs.write_range(&mut desc, 0, &[0xab; 600]).unwrap(), 600);
    assert_eq!(read_u32(sector_of(&fs, 32), 3 * 4) & 0x0fff_ffff, 4);
    assert_eq!(
        read_u32(sector_of(&fs, 32), 4 * 4) & 0x0fff_ffff,
        0x0fff_ffff
    );
}

// Catches a power-loss-mid-mirror image: FAT2 lost its copy while FAT1
// kept it. Mount must read FAT1's view, and the next mutation resyncs
// both copies instead of failing on the divergence.
#[test]
fn divergent_fat_mirror_still_reads_and_resyncs() {
    let mut sectors = mirrored_sectors();
    sectors[2] = (33, [0; 512]);
    let mut fs = Fat32::mount(MemoryReader::with_sectors(sectors)).unwrap();
    let mut out = Vec::new();
    fs.read_file("HELLO.TXT", |chunk| out.extend_from_slice(chunk))
        .unwrap();
    assert_eq!(out, b"hello world");
    fs.create_dir("NEWDIR").unwrap();
    assert_fats_mirrored(&fs);
}

// Catches the mirror loop writing a second copy on a fat_count=1
// volume: lba 33 is the root directory there and must keep its entries.
#[test]
fn single_fat_volume_does_not_write_past_its_fat() {
    let mut fs = mounted_writable_fixture();
    let mut desc = fs.create_file("NEW.TXT").unwrap();
    assert_eq!(fs.write_range(&mut desc, 0, &[0xab; 700]).unwrap(), 700);
    fs.create_dir("NEWDIR").unwrap();
    fs.unlink_file("NEW.TXT").unwrap();
    let root = sector_of(&fs, 33);
    assert_eq!(&root[0..11], b"HELLO   TXT");
    assert_eq!(&root[3 * 32..3 * 32 + 11], b"NEWDIR     ");
    assert!(fs.open_file("HELLO.TXT").is_ok());
}

// Catches a failed second-copy write being swallowed: the error must
// reach the caller even though the first copy was already written.
#[test]
fn second_fat_write_failure_is_reported() {
    let mut sectors = mirrored_sectors();
    sectors[2].0 = u32::MAX; // lba 33（FAT第2面）を欠落させる
    let mut fs = Fat32::mount(MemoryReader::with_sectors(sectors)).unwrap();
    let mut desc = fs.create_file("NEW.TXT").unwrap();
    assert!(matches!(
        fs.write_range(&mut desc, 0, b"x"),
        Err(FatError::Read(ReadError::MissingSector(33)))
    ));
    assert_eq!(
        read_u32(sector_of(&fs, 32), 3 * 4) & 0x0fff_ffff,
        0x0fff_ffff
    );
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
