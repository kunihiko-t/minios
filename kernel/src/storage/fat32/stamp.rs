//! directory entryの日時field。
//!
//! FATの日時は16 bitの日付と時刻に分けて持つ。日付は1980年からの年（7 bit）、
//! 月（4 bit）、日（5 bit）、時刻は時（5 bit）、分（6 bit）、2秒単位の秒
//! （5 bit）である。作成時刻だけは10 ms単位の補正（0..=199）を別byteで持ち、
//! 2秒単位で落ちた秒と秒未満を補う。FATの日時は本来ローカル時刻だが、
//! MiniOSはtime zoneを持たないためUTCを書く。
//!
//! entry内のoffsetは、作成時刻の補正が13、作成時刻が14、作成日が16、
//! 最終アクセス日が18、更新時刻が22、更新日が24である。

use minios_abi::time::DateTime;

use super::parse::write_u16;

/// entryの更新日`date`と更新時刻`time`をUnix時刻（秒）へ戻す。日付が0
/// （日時を書かないimplementationが作ったentry）か、暦に無い値なら0。
pub(super) fn fat_to_unix_seconds(date: u16, time: u16) -> u64 {
    if date == 0 {
        return 0;
    }
    DateTime {
        year: 1980 + i64::from(date >> 9),
        month: ((date >> 5) & 0x0f) as u8,
        day: (date & 0x1f) as u8,
        hour: (time >> 11) as u8,
        minute: ((time >> 5) & 0x3f) as u8,
        second: ((time & 0x1f) * 2) as u8,
    }
    .to_unix_seconds()
    .unwrap_or(0)
}

/// FATが表せる最初の瞬間（1980-01-01 00:00:00 UTC）のUnix時刻（秒）。
const FAT_EPOCH_SECONDS: u64 = 315_532_800;
/// FATが表せる最後の年。年の7 bitが1980からの127年までを持つ。
const FAT_LAST_YEAR: i64 = 1980 + 127;

/// 1つの瞬間をFATの形へ直した日時。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct FatStamp {
    pub(super) date: u16,
    pub(super) time: u16,
    /// 作成時刻の10 ms単位の補正（0..=199）。
    pub(super) tenths: u8,
}

impl FatStamp {
    /// Unix時刻`unix_millis`をFATの日時へ直す。FATが表せない1980年より前は
    /// 1980-01-01 00:00:00へ、2107年より後は2107-12-31 23:59:59へ寄せる。
    pub(super) fn from_unix_millis(unix_millis: u64) -> Self {
        let seconds = unix_millis / 1_000;
        if seconds < FAT_EPOCH_SECONDS {
            return Self::from_unix_millis(FAT_EPOCH_SECONDS * 1_000);
        }
        let at = DateTime::from_unix_seconds(seconds);
        if at.year > FAT_LAST_YEAR {
            return Self {
                date: (127 << 9) | (12 << 5) | 31,
                time: (23 << 11) | (59 << 5) | 29,
                tenths: 199,
            };
        }
        Self {
            date: (((at.year - 1980) as u16) << 9) | (u16::from(at.month) << 5) | u16::from(at.day),
            time: (u16::from(at.hour) << 11)
                | (u16::from(at.minute) << 5)
                | u16::from(at.second / 2),
            tenths: (at.second % 2) * 100 + ((unix_millis % 1_000) / 10) as u8,
        }
    }

    /// 新しく作るentryの`record`（32 byte）へ、作成・最終アクセス・更新の
    /// 日時をすべてこの瞬間として書く。
    pub(super) fn write_created(&self, record: &mut [u8]) {
        record[13] = self.tenths;
        write_u16(record, 14, self.time);
        write_u16(record, 16, self.date);
        write_u16(record, 18, self.date);
        self.write_modified(record);
    }

    /// 内容を書き換えたentryの`record`へ、更新日時と最終アクセス日を書く。
    pub(super) fn write_modified(&self, record: &mut [u8]) {
        write_u16(record, 18, self.date);
        write_u16(record, 22, self.time);
        write_u16(record, 24, self.date);
    }
}
