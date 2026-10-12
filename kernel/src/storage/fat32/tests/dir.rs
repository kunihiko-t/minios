use super::*;

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
