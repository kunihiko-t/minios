use super::*;
use crate::storage::fat32::RenameOutcome;

// Catches rename moving the entry, corrupting the name bytes, or
// freeing the chain: the entry must stay at its slot with only the
// 8.3 name rewritten and contents intact.
#[test]
fn rename_rewrites_the_entry_name_and_preserves_contents() {
    let mut fs = mounted_writable_fixture();
    assert_eq!(
        fs.rename("HELLO.TXT", "GREET.TXT").unwrap(),
        RenameOutcome {
            moved: None,
            replaced: None
        }
    );

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
    assert_eq!(
        fs.rename("HELLO.TXT", "hello.txt").unwrap(),
        RenameOutcome {
            moved: None,
            replaced: None
        }
    );
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
        fs.rename("OLD.TXT", "HELLO.TXT").unwrap(),
        RenameOutcome {
            moved: None,
            replaced: Some((2, 0))
        }
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
        fs.rename("Long File Name.txt", "NEW.TXT").unwrap(),
        RenameOutcome {
            moved: None,
            replaced: None
        }
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

// Catches rename writing before validating: every rejection must
// leave the disk untouched. `mv dir 自身の中`と`mv dir 子孫の中`は
// `..`chainを閉じるため拒否する。
#[test]
fn rename_rejects_self_moves_and_bad_names_before_writing() {
    let mut fs = mounted_writable_fixture();
    assert!(matches!(
        fs.rename("SUBDIR", "SUBDIR/X"),
        Err(FatError::MoveIntoItself)
    ));
    assert!(matches!(
        fs.rename("HELLO.TXT", "MISSINGDIR/X.TXT"),
        Err(FatError::NotFound)
    ));
    assert!(matches!(
        fs.rename("HELLO.TXT", "SUBDIR"),
        Err(FatError::IsDirectory)
    ));
    assert!(matches!(
        fs.rename("HELLO.TXT", "long name.txt"),
        Err(FatError::InvalidName)
    ));
    assert!(matches!(
        fs.rename("MISSING.TXT", "X.TXT"),
        Err(FatError::NotFound)
    ));
    // 拒否はdiskを変えない。
    let root = sector_of(&fs, 33);
    assert_eq!(&root[0..11], b"HELLO   TXT");
    assert_eq!(&root[32..43], b"SUBDIR     ");
}

// Catches the parent-directory check comparing the wrong thing:
// rename inside a subdirectory must stay in place (same dir_head,
// no `moved`) while a cross-directory rename must move the record.
#[test]
fn rename_inside_a_subdirectory_is_allowed() {
    let mut fs = mounted_nested_fixture();
    assert_eq!(
        fs.rename("SUBDIR/NOTE.TXT", "SUBDIR/NEW.TXT").unwrap(),
        RenameOutcome {
            moved: None,
            replaced: None
        }
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

/// dir rename検証用fixture。root: [0]=HELLO.TXT(cluster 4),
/// [1]=SUBDIR(cluster 5、NOTE.TXT入りで非空), [2]=EMPTYD(cluster 6、
/// 空), [3]=終端。cluster 7はNOTE.TXTのdata。
fn mounted_dir_rename_fixture() -> Fat32<MemoryReader<10>> {
    let mut fat = writable_fat();
    write_fat_entry(&mut fat, 6, 0x0fff_ffff);
    write_fat_entry(&mut fat, 7, 0x0fff_ffff);

    let mut root = [0; 512];
    write_directory_entry(&mut root, 0, b"HELLO   TXT", 0x20, 4, 11);
    write_directory_entry(&mut root, 1, b"SUBDIR     ", 0x10, 5, 0);
    write_directory_entry(&mut root, 2, b"EMPTYD     ", 0x10, 6, 0);

    let mut hello = [0; 512];
    hello[..11].copy_from_slice(b"hello world");

    let mut subdir = [0; 512];
    write_directory_entry(&mut subdir, 0, b".          ", 0x10, 5, 0);
    write_directory_entry(&mut subdir, 1, b"..         ", 0x10, 2, 0);
    write_directory_entry(&mut subdir, 2, b"NOTE    TXT", 0x20, 7, 11);

    let mut emptyd = [0; 512];
    write_directory_entry(&mut emptyd, 0, b".          ", 0x10, 6, 0);
    write_directory_entry(&mut emptyd, 1, b"..         ", 0x10, 0, 0);

    let mut note = [0; 512];
    note[..11].copy_from_slice(b"nested note");

    Fat32::mount(MemoryReader::with_sectors([
        (0, writable_boot_sector()),
        (32, fat),
        (33, root),
        (34, [0; 512]),
        (35, hello),
        (36, subdir),
        (37, emptyd),
        (38, note),
        (39, [0; 512]),
        (40, [0; 512]),
    ]))
    .unwrap()
}

// Catches dir rename writing the wrong entry, losing the dir contents,
// or touching `.`/`..`: the rename must only change the name bytes of
// the source's own entry in the parent.
#[test]
fn rename_moves_a_directory_entry_and_keeps_its_contents() {
    let mut fs = mounted_dir_rename_fixture();
    assert_eq!(
        fs.rename("SUBDIR", "NEWDIR").unwrap(),
        RenameOutcome {
            moved: None,
            replaced: None
        }
    );
    // 同名へのrenameはno-op成功。
    assert_eq!(
        fs.rename("NEWDIR", "NEWDIR").unwrap(),
        RenameOutcome {
            moved: None,
            replaced: None
        }
    );

    let root = sector_of(&fs, 33);
    assert_eq!(&root[32..43], b"NEWDIR     ");
    assert!(matches!(
        fs.resolve_path("SUBDIR", &mut [0; 512]),
        Err(FatError::NotFound)
    ));
    let (entry, _) = fs.resolve_path("NEWDIR", &mut [0; 512]).unwrap();
    assert!(entry.directory);
    assert_eq!(entry.first_cluster, 5);
    // dir内のfileは新しい親名で解決できる。
    let desc = fs.open_file("NEWDIR/NOTE.TXT").unwrap();
    assert_eq!(desc.size, 11);
    // `.`/`..`はrenameが触れない。
    let subdir = sector_of(&fs, 36);
    assert_eq!(&subdir[0..11], b".          ");
    assert_eq!(&subdir[32..43], b"..         ");
}

// Catches the dir/file target matrix collapsing: dir→file is ENOTDIR,
// dir→non-empty-dir is ENOTEMPTY, and both leave the disk untouched.
#[test]
fn rename_rejects_dir_to_file_and_nonempty_dir_targets() {
    let mut fs = mounted_dir_rename_fixture();
    assert!(matches!(
        fs.rename("SUBDIR", "HELLO.TXT"),
        Err(FatError::NotDirectory)
    ));
    assert!(matches!(
        fs.rename("EMPTYD", "SUBDIR"),
        Err(FatError::NotEmpty)
    ));
    let root = sector_of(&fs, 33);
    assert_eq!(&root[32..43], b"SUBDIR     ");
    assert_eq!(&root[64..75], b"EMPTYD     ");
}

// Catches dir→empty-dir replacement failing to free the target's
// chain or deleting the source's entry instead of the target's.
#[test]
fn rename_replaces_an_empty_directory() {
    let mut fs = mounted_dir_rename_fixture();
    // SUBDIR(非空)を空のEMPTYDへrename：EMPTYDのentryとchainが消え、
    // SUBDIRのentryが新名を引き継ぐ。
    assert_eq!(
        fs.rename("SUBDIR", "EMPTYD").unwrap(),
        RenameOutcome {
            moved: None,
            replaced: Some((2, 2))
        }
    );

    let root = sector_of(&fs, 33);
    assert_eq!(&root[32..43], b"EMPTYD     ");
    assert_eq!(root[64], 0xe5);
    // 解放されたcluster 6のFAT entryは0、contentsのcluster 5はEOCのまま。
    let fat = sector_of(&fs, 32);
    assert_eq!(read_u32(fat, 6 * 4) & 0x0fff_ffff, 0);
    assert_eq!(read_u32(fat, 5 * 4) & 0x0fff_ffff, 0x0fff_ffff);

    assert!(matches!(
        fs.resolve_path("SUBDIR", &mut [0; 512]),
        Err(FatError::NotFound)
    ));
    let desc = fs.open_file("EMPTYD/NOTE.TXT").unwrap();
    assert_eq!(desc.size, 11);
}

// Catches a cross-directory move that forgets to delete the source
// record, fails to copy it to the target dir, or misreports the
// relocated fd location.
#[test]
fn rename_moves_a_file_across_directories_and_back() {
    let mut fs = mounted_writable_fixture();
    // SUBDIR(5)は `.`/`..` のあとindex 2が終端 → target slotは(5,2)。
    assert_eq!(
        fs.rename("HELLO.TXT", "SUBDIR/MOVED.TXT").unwrap(),
        RenameOutcome {
            moved: Some(((2, 0), (5, 2))),
            replaced: None,
        }
    );

    let root = sector_of(&fs, 33);
    assert_eq!(root[0], 0xe5);
    let subdir = sector_of(&fs, 36);
    assert_eq!(&subdir[64..75], b"MOVED   TXT");
    assert_eq!(subdir[64 + 11], 0x20);
    // first_cluster/size/attrがsource recordから複写されている。
    assert_eq!(read_u16(subdir, 64 + 26), 4);
    assert_eq!(read_u32(subdir, 64 + 28), 11);
    assert!(matches!(
        fs.resolve_path("HELLO.TXT", &mut [0; 512]),
        Err(FatError::NotFound)
    ));
    let desc = fs.open_file("SUBDIR/MOVED.TXT").unwrap();
    assert_eq!((desc.size, desc.dir_location()), (11, (5, 2)));
    let mut buf = [0; 11];
    assert_eq!(fs.read_range(&desc, 0, &mut buf).unwrap(), 11);
    assert_eq!(&buf, b"hello world");

    // rootへ戻す：解放済みslot (2,0) が再利用される。
    assert_eq!(
        fs.rename("SUBDIR/MOVED.TXT", "HELLO.TXT").unwrap(),
        RenameOutcome {
            moved: Some(((5, 2), (2, 0))),
            replaced: None,
        }
    );
    assert!(matches!(
        fs.resolve_path("SUBDIR/MOVED.TXT", &mut [0; 512]),
        Err(FatError::NotFound)
    ));
    assert_eq!(fs.open_file("HELLO.TXT").unwrap().size, 11);
}

// Catches a directory move that forgets to update `..`, copies the
// wrong cluster fields, or leaves the source entry behind.
#[test]
fn rename_moves_a_directory_and_updates_dotdot() {
    let mut fs = mounted_dir_rename_fixture();
    // SUBDIR(5)をEMPTYD(6)の中へ。EMPTYDの`.`/`..`の次のindex 2が
    // 終端 → target slotは(6,2)。
    assert_eq!(
        fs.rename("SUBDIR", "EMPTYD/SUBDIR").unwrap(),
        RenameOutcome {
            moved: Some(((2, 1), (6, 2))),
            replaced: None,
        }
    );

    let root = sector_of(&fs, 33);
    assert_eq!(root[32], 0xe5);
    let emptyd = sector_of(&fs, 37);
    assert_eq!(&emptyd[64..75], b"SUBDIR     ");
    assert_eq!(emptyd[64 + 11], 0x10);
    // 移したdirの`..`は新parent cluster 6を指す。
    let subdir = sector_of(&fs, 36);
    assert_eq!(read_u16(subdir, 52), 0);
    assert_eq!(read_u16(subdir, 58), 6);

    assert!(matches!(
        fs.resolve_path("SUBDIR", &mut [0; 512]),
        Err(FatError::NotFound)
    ));
    assert_eq!(fs.open_file("EMPTYD/SUBDIR/NOTE.TXT").unwrap().size, 11);

    // rootへ戻す：`..`はFAT32規約の0へ書き戻される。
    assert_eq!(
        fs.rename("EMPTYD/SUBDIR", "SUBDIR2").unwrap(),
        RenameOutcome {
            moved: Some(((6, 2), (2, 1))),
            replaced: None,
        }
    );
    let subdir = sector_of(&fs, 36);
    assert_eq!(read_u16(subdir, 52), 0);
    assert_eq!(read_u16(subdir, 58), 0);
    assert_eq!(fs.open_file("SUBDIR2/NOTE.TXT").unwrap().size, 11);
}

// Catches the `..`-walk missing a cycle: moving a dir into its own
// descendant must be rejected like moving it into itself.
#[test]
fn rename_rejects_moving_a_directory_into_its_descendant() {
    let mut fs = mounted_writable_fixture();
    fs.create_dir("SUBDIR/INNER").unwrap();
    assert!(matches!(
        fs.rename("SUBDIR", "SUBDIR/INNER/X"),
        Err(FatError::MoveIntoItself)
    ));
    // 拒否はdiskを変えない。
    let root = sector_of(&fs, 33);
    assert_eq!(&root[32..43], b"SUBDIR     ");
}

/// rootに`..`をrecord 1へ持たないBADDIR(cluster 5)と空のEMPTYD
/// (cluster 6)を持つfixture。moveが`..`更新を書き込み前に検証する
/// ことを確認するために使う。
fn mounted_bad_dotdot_fixture() -> Fat32<MemoryReader<10>> {
    let mut fat = writable_fat();
    write_fat_entry(&mut fat, 6, 0x0fff_ffff);

    let mut root = [0; 512];
    write_directory_entry(&mut root, 0, b"BADDIR     ", 0x10, 5, 0);
    write_directory_entry(&mut root, 1, b"EMPTYD     ", 0x10, 6, 0);

    let mut baddir = [0; 512];
    write_directory_entry(&mut baddir, 0, b".          ", 0x10, 5, 0);
    write_directory_entry(&mut baddir, 1, b"STRAY   TXT", 0x20, 0, 0);

    let mut emptyd = [0; 512];
    write_directory_entry(&mut emptyd, 0, b".          ", 0x10, 6, 0);
    write_directory_entry(&mut emptyd, 1, b"..         ", 0x10, 0, 0);

    Fat32::mount(MemoryReader::with_sectors([
        (0, writable_boot_sector()),
        (32, fat),
        (33, root),
        (34, [0; 512]),
        (35, [0; 512]),
        (36, baddir),
        (37, emptyd),
        (38, [0; 512]),
        (39, [0; 512]),
        (40, [0; 512]),
    ]))
    .unwrap()
}

#[test]
fn rename_rejects_a_directory_without_a_dotdot_record() {
    let mut fs = mounted_bad_dotdot_fixture();
    assert!(matches!(
        fs.rename("BADDIR", "EMPTYD/BADDIR"),
        Err(FatError::CorruptChain)
    ));
    // 書き込み前に失敗するため、sourceは残っている。
    let root = sector_of(&fs, 33);
    assert_eq!(&root[0..11], b"BADDIR     ");
}

// Catches the cross-dir replacement matrix diverging from the
// in-place one: file→file replaces, dir→empty-dir replaces, and
// file→dir/dir→file/dir→nonempty-dir are rejected.
#[test]
fn rename_move_replaces_and_rejects_across_directories() {
    let mut fs = mounted_dir_rename_fixture();
    // file→file：HELLO.TXTをSUBDIR内のNOTE.TXTへ。target dirの終端は
    // NOTE.TXTの次のindex 3 → movedは(2,0)→(5,3)、replacedは(5,2)。
    assert_eq!(
        fs.rename("HELLO.TXT", "SUBDIR/NOTE.TXT").unwrap(),
        RenameOutcome {
            moved: Some(((2, 0), (5, 3))),
            replaced: Some((5, 2)),
        }
    );
    let subdir = sector_of(&fs, 36);
    assert_eq!(subdir[64], 0xe5);
    assert_eq!(&subdir[96..107], b"NOTE    TXT");
    // entryはHELLO.TXTのcluster 4を指し、NOTE.TXTの旧chain 7は解放。
    assert_eq!(read_u16(subdir, 96 + 26), 4);
    let fat = sector_of(&fs, 32);
    assert_eq!(read_u32(fat, 7 * 4) & 0x0fff_ffff, 0);

    // dir→file / dir→非空dir / file→dirはin-placeと同じerrno。
    fs.create_dir("SUBDIR/FULL").unwrap();
    fs.create_file("SUBDIR/FULL/X.TXT").unwrap();
    assert!(matches!(
        fs.rename("EMPTYD", "SUBDIR/NOTE.TXT"),
        Err(FatError::NotDirectory)
    ));
    assert!(matches!(
        fs.rename("EMPTYD", "SUBDIR/FULL"),
        Err(FatError::NotEmpty)
    ));
    assert!(matches!(
        fs.rename("SUBDIR/NOTE.TXT", "SUBDIR/FULL"),
        Err(FatError::IsDirectory)
    ));

    // dir→空dir：EMPTYDをSUBDIR内の空dir HOMEへ置換。FULL作成時に
    // 解放済みslot(5,2)を再利用したため、HOMEは(5,4)、free slotは(5,5)。
    fs.create_dir("SUBDIR/HOME").unwrap();
    let outcome = fs.rename("EMPTYD", "SUBDIR/HOME").unwrap();
    assert_eq!(outcome.moved, Some(((2, 2), (5, 5))));
    assert_eq!(outcome.replaced, Some((5, 4)));
    // 移したdirの`..`は新parent SUBDIR(5)を指す。
    let emptyd = sector_of(&fs, 37);
    assert_eq!(read_u16(emptyd, 52), 0);
    assert_eq!(read_u16(emptyd, 58), 5);
}

// Catches find_free_dir_slot failing to extend a full target dir:
// the moved record must land in a newly chained cluster.
#[test]
fn rename_move_extends_a_full_target_directory_chain() {
    let mut fs = mounted_writable_fixture();
    // SUBDIRの1 sector（16 records）を `.`,`..`+14 fileで埋める。
    for name in [
        "F0.TXT", "F1.TXT", "F2.TXT", "F3.TXT", "F4.TXT", "F5.TXT", "F6.TXT", "F7.TXT", "F8.TXT",
        "F9.TXT", "FA.TXT", "FB.TXT", "FC.TXT", "FD.TXT",
    ] {
        let mut path = String::from("SUBDIR/");
        path.push_str(name);
        fs.create_file(&path).unwrap();
    }
    let outcome = fs.rename("HELLO.TXT", "SUBDIR/MOVED.TXT").unwrap();
    // 新cluster（3）のindex 0へrecordが置かれる。
    assert_eq!(outcome.moved, Some(((2, 0), (3, 0))));
    assert_eq!(fs.open_file("SUBDIR/MOVED.TXT").unwrap().size, 11);
}
