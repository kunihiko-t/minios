//! `realtime`が返すUnix時刻の解釈。
//!
//! Unix時刻（1970-01-01 00:00:00 UTCからの経過秒、閏秒は数えない）を
//! UTCの暦へ直す。guestの`date`とkernelのFAT32 timestampが同じ変換を使う。

use core::fmt;

/// 1日の秒数。閏秒は数えない（Unix時刻の規約）。
const SECONDS_PER_DAY: u64 = 86_400;
/// 0000-03-01から1970-01-01までの日数。暦の計算を3月始まりへずらす。
const DAYS_FROM_YEAR_0_MARCH: i64 = 719_468;
/// 400年周期の日数。グレゴリオ暦の閏年の規則はこの周期で繰り返す。
const DAYS_PER_400_YEARS: i64 = 146_097;

/// UTCの日時。秒未満は持たない。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: i64,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

impl DateTime {
    /// Unix時刻（秒）をUTCの日時へ直す。
    ///
    /// 日付はHoward Hinnantの`civil_from_days`による。年の始まりを3月へ
    /// ずらすと閏日が年の最後に来るため、月の長さを`(153 * m + 2) / 5`の
    /// 一次式で数えられる。閏年の規則（4年ごと、100年ごとは除き、400年
    /// ごとは含む）は400年周期の中の位置から求める。
    pub fn from_unix_seconds(seconds: u64) -> Self {
        let days = (seconds / SECONDS_PER_DAY) as i64 + DAYS_FROM_YEAR_0_MARCH;
        let in_day = seconds % SECONDS_PER_DAY;

        let era = days.div_euclid(DAYS_PER_400_YEARS);
        // 400年周期の中の日（0..=146096）と年（0..=399）。
        let day_of_era = days.rem_euclid(DAYS_PER_400_YEARS);
        let year_of_era =
            (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        // 3月1日を0とする年内の日（0..=365）と月（0が3月、11が2月）。
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let march_month = (5 * day_of_year + 2) / 153;
        let day = (day_of_year - (153 * march_month + 2) / 5 + 1) as u8;
        let month = if march_month < 10 {
            march_month + 3
        } else {
            march_month - 9
        } as u8;
        // 1月と2月は3月始まりの年では前年の続きなので、暦の年を1つ進める。
        let year = era * 400 + year_of_era + i64::from(month <= 2);

        Self {
            year,
            month,
            day,
            hour: (in_day / 3_600) as u8,
            minute: (in_day / 60 % 60) as u8,
            second: (in_day % 60) as u8,
        }
    }
}

impl fmt::Display for DateTime {
    /// `YYYY-MM-DD HH:MM:SS`の形で書く。
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
            self.year, self.month, self.day, self.hour, self.minute, self.second
        )
    }
}

#[cfg(test)]
mod tests {
    use super::DateTime;

    extern crate std;

    use std::string::{String, ToString};

    fn format(seconds: u64) -> String {
        DateTime::from_unix_seconds(seconds).to_string()
    }

    // Catches the calendar conversion drifting at the boundaries that the
    // leap-year rules decide: the epoch itself, a year divisible by 400
    // (leap), a year divisible by 100 but not 400 (not leap), an ordinary
    // leap day, and the last second before 10000.
    #[test]
    fn unix_seconds_convert_to_the_utc_calendar() {
        assert_eq!(format(0), "1970-01-01 00:00:00");
        assert_eq!(format(981_173_106), "2001-02-03 04:05:06");
        assert_eq!(format(951_782_400), "2000-02-29 00:00:00");
        assert_eq!(format(4_107_542_399), "2100-02-28 23:59:59");
        assert_eq!(format(4_107_542_400), "2100-03-01 00:00:00");
        assert_eq!(format(1_709_251_199), "2024-02-29 23:59:59");
        assert_eq!(format(253_402_300_799), "9999-12-31 23:59:59");
    }

    // Catches the day-by-day walk going wrong anywhere in a 400-year cycle:
    // every next day must be either the next date in the same month or the
    // first of the next month, and Feb 29 must exist exactly in leap years.
    #[test]
    fn consecutive_days_never_skip_or_repeat_a_date() {
        let mut previous = DateTime::from_unix_seconds(0);
        for day in 1..(400 * 366) {
            let next = DateTime::from_unix_seconds(day * 86_400);
            let leap = |year: i64| year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
            let month_len = match previous.month {
                2 if leap(previous.year) => 29,
                2 => 28,
                4 | 6 | 9 | 11 => 30,
                _ => 31,
            };
            if previous.day < month_len {
                assert_eq!(
                    (next.year, next.month, next.day),
                    (previous.year, previous.month, previous.day + 1)
                );
            } else if previous.month < 12 {
                assert_eq!(
                    (next.year, next.month, next.day),
                    (previous.year, previous.month + 1, 1)
                );
            } else {
                assert_eq!((next.year, next.month, next.day), (previous.year + 1, 1, 1));
            }
            previous = next;
        }
    }
}
