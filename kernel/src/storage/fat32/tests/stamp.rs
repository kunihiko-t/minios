use std::cell::Cell;

use super::*;
use crate::storage::fat32::stamp::FatStamp;

std::thread_local! {
    /// testごとの時計。各testは自分のthreadで走るため、互いに干渉しない。
    static NOW: Cell<Option<u64>> = const { Cell::new(None) };
}

fn test_clock() -> Option<u64> {
    NOW.with(Cell::get)
}

fn set_now(unix_millis: u64) {
    NOW.with(|now| now.set(Some(unix_millis)));
}

/// 2001-02-03 04:05:06.789 UTC。
const AT_2001: u64 = 981_173_106_789;
/// 2026-11-07 16:00:01.250 UTC。
const AT_2026: u64 = 1_794_067_201_250;

/// FATの日付`(年, 月, 日)`と時刻`(時, 分, 秒)`を組む。秒は2秒単位へ落ちる。
fn fat_date(year: u16, month: u16, day: u16) -> u16 {
    ((year - 1980) << 9) | (month << 5) | day
}

fn fat_time(hour: u16, minute: u16, second: u16) -> u16 {
    (hour << 11) | (minute << 5) | (second / 2)
}

fn u16_at(record: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([record[offset], record[offset + 1]])
}

// Catches the FAT date/time bit layout drifting: the year counts from
// 1980 in 7 bits, seconds are stored halved with the dropped second and
// the milliseconds kept in the 10 ms creation field, and instants FAT
// cannot represent are pinned to its first and last moments.
#[test]
fn unix_milliseconds_encode_to_fat_date_and_time() {
    assert_eq!(
        FatStamp::from_unix_millis(AT_2001),
        FatStamp {
            date: fat_date(2001, 2, 3),
            time: fat_time(4, 5, 6),
            tenths: 78,
        }
    );
    // 奇数秒は2秒単位の時刻から落ち、補正の100（1秒）として残る。
    assert_eq!(
        FatStamp::from_unix_millis(AT_2026),
        FatStamp {
            date: fat_date(2026, 11, 7),
            time: fat_time(16, 0, 0),
            tenths: 125,
        }
    );
    // FATより前の瞬間は1980-01-01 00:00:00、2107年より後は最後の瞬間へ寄せる。
    assert_eq!(
        FatStamp::from_unix_millis(0),
        FatStamp {
            date: fat_date(1980, 1, 1),
            time: 0,
            tenths: 0,
        }
    );
    assert_eq!(
        FatStamp::from_unix_millis(4_354_819_200_000),
        FatStamp {
            date: fat_date(2107, 12, 31),
            time: fat_time(23, 59, 58),
            tenths: 199,
        }
    );
}

// Catches created entries keeping zero (invalid) dates or the clock being
// read for the wrong fields: a new file, its LFN alias, a new directory and
// its `.`/`..` records must all carry the creation instant in the creation,
// last-access and modification fields.
#[test]
fn created_entries_carry_the_clock_instant() {
    set_now(AT_2001);
    let mut fs = mounted_writable_fixture().with_clock(test_clock);
    fs.create_file("notes.txt").unwrap();
    fs.create_file("long name.txt").unwrap();
    fs.create_dir("work").unwrap();

    let root = *sector_of(&fs, 33);
    // slot 2がnotes.txt、3がLFN record、4がそのalias、5がwork。
    for slot in [2usize, 4, 5] {
        let record = &root[slot * 32..slot * 32 + 32];
        assert_eq!(record[13], 78, "slot {slot}");
        assert_eq!(u16_at(record, 14), fat_time(4, 5, 6), "slot {slot}");
        for offset in [16, 18, 24] {
            assert_eq!(u16_at(record, offset), fat_date(2001, 2, 3), "slot {slot}");
        }
        assert_eq!(u16_at(record, 22), fat_time(4, 5, 6), "slot {slot}");
    }
    // workの`.`と`..`も同じ瞬間を持つ。
    let mut work = 0;
    fs.for_each_entry("", |entry| {
        if entry.name() == "work" {
            work = entry.first_cluster;
        }
    })
    .unwrap();
    let dots = *sector_of(&fs, fs.data_sector_lba(work, 0).unwrap());
    for record in [&dots[..32], &dots[32..64]] {
        assert_eq!(u16_at(record, 16), fat_date(2001, 2, 3));
        assert_eq!(u16_at(record, 22), fat_time(4, 5, 6));
    }
}

// Catches writes leaving the modification time at the creation instant or
// rewriting the creation time: after a later write, only the modification
// and last-access fields move to the new instant.
#[test]
fn writing_updates_only_the_modification_time() {
    set_now(AT_2001);
    let mut fs = mounted_writable_fixture().with_clock(test_clock);
    let mut desc = fs.create_file("notes.txt").unwrap();
    set_now(AT_2026);
    assert_eq!(fs.write_range(&mut desc, 0, b"hi"), Ok(2));

    let record = &sector_of(&fs, 33)[64..96];
    assert_eq!(u16_at(record, 14), fat_time(4, 5, 6));
    assert_eq!(u16_at(record, 16), fat_date(2001, 2, 3));
    assert_eq!(u16_at(record, 18), fat_date(2026, 11, 7));
    assert_eq!(u16_at(record, 22), fat_time(16, 0, 0));
    assert_eq!(u16_at(record, 24), fat_date(2026, 11, 7));
}

// Catches stat, readdir and fstat disagreeing on the modification time or
// losing it in the FAT round trip: all three must report the instant the
// clock gave at the last write, truncated to FAT's two-second step, and an
// entry without dates must report 0.
#[test]
fn stat_and_fstat_report_the_modification_time() {
    set_now(AT_2001);
    let mut fs = mounted_writable_fixture().with_clock(test_clock);
    let mut desc = fs.create_file("notes.txt").unwrap();
    set_now(AT_2026);
    assert_eq!(fs.write_range(&mut desc, 0, b"hi"), Ok(2));

    // 16:00:01は2秒単位へ落ちて16:00:00になる。
    let expected = (AT_2026 / 1_000) - 1;
    assert_eq!(fs.stat("notes.txt").unwrap().modified, expected);
    assert_eq!(fs.modified_of(&desc), Ok(expected));
    let mut listed = Vec::new();
    fs.for_each_entry("", |entry| {
        listed.push((String::from(entry.name()), entry.modified()))
    })
    .unwrap();
    assert!(listed.contains(&(String::from("notes.txt"), expected)));
    assert!(listed.contains(&(String::from("HELLO.TXT"), 0)));
}

// Catches a missing clock writing a made-up date: without `with_clock`
// the date and time fields of a new entry stay zero as before.
#[test]
fn without_a_clock_the_date_fields_stay_zero() {
    let mut fs = mounted_writable_fixture();
    let mut desc = fs.create_file("notes.txt").unwrap();
    assert_eq!(fs.write_range(&mut desc, 0, b"hi"), Ok(2));
    assert!(
        sector_of(&fs, 33)[64 + 13..64 + 26]
            .iter()
            .all(|&byte| byte == 0)
    );
}
