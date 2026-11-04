//! Checked filesystem timestamps. FAT local times never acquire an invented zone.
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileTimestamp {
    pub value: String,
    pub unix_seconds: Option<i64>,
}

fn leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}
fn civil_days(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let y = year - era * 400;
    let m = month + if month > 2 { -3 } else { 9 };
    let d = (153 * m + 2) / 5 + day - 1;
    era * 146097 + y * 365 + y / 4 - y / 100 + d - 719468
}
fn civil_date(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let d = z - era * 146097;
    let y = (d - d / 1460 + d / 36524 - d / 146096) / 365;
    let year = y + era * 400;
    let dy = d - (365 * y + y / 4 - y / 100);
    let m = (5 * dy + 2) / 153;
    let day = dy - (153 * m + 2) / 5 + 1;
    let month = m + if m < 10 { 3 } else { -9 };
    (year + i64::from(month <= 2), month, day)
}
pub(crate) fn filetime(raw: u64) -> Option<FileTimestamp> {
    if raw == 0 {
        return None;
    }
    let seconds = i64::try_from(raw / 10_000_000).ok()? - 11644473600;
    let (y, m, d) = civil_date(seconds.div_euclid(86400));
    if !(1601..=9999).contains(&y) {
        return None;
    }
    let time = seconds.rem_euclid(86400);
    Some(FileTimestamp {
        value: format!(
            "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z",
            time / 3600,
            time / 60 % 60,
            time % 60
        ),
        unix_seconds: Some(seconds),
    })
}
pub(crate) fn dos(
    date: u16,
    time: u16,
    centiseconds: u8,
    zone: Option<u8>,
) -> Option<FileTimestamp> {
    let y = 1980 + i64::from(date >> 9);
    let m = i64::from(date >> 5 & 15);
    let d = i64::from(date & 31);
    let h = i64::from(time >> 11);
    let minute = i64::from(time >> 5 & 63);
    let second = i64::from(time & 31) * 2 + i64::from(centiseconds / 100);
    if !(1..=12).contains(&m)
        || d == 0
        || h > 23
        || minute > 59
        || second > 59
        || centiseconds > 199
    {
        return None;
    }
    let max_day = match m {
        2 => {
            if leap(y) {
                29
            } else {
                28
            }
        }
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    };
    if d > max_day {
        return None;
    }
    let local = civil_days(y, m, d) * 86400 + h * 3600 + minute * 60 + second;
    let offset = zone.filter(|z| z & 128 != 0).map(|z| {
        let v = i64::from(z & 127);
        (if v & 64 != 0 { v - 128 } else { v }) * 15
    });
    let suffix = offset.map_or_else(String::new, |v| {
        if v == 0 {
            "Z".into()
        } else {
            format!(
                "{}{:02}:{:02}",
                if v < 0 { '-' } else { '+' },
                v.abs() / 60,
                v.abs() % 60
            )
        }
    });
    Some(FileTimestamp {
        value: format!("{y:04}-{m:02}-{d:02}T{h:02}:{minute:02}:{second:02}{suffix}"),
        unix_seconds: offset.map(|v| local - v * 60),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checked_filesystem_dates_preserve_timezone_uncertainty() {
        assert_eq!(
            filetime(11644473600 * 10_000_000).unwrap().value,
            "1970-01-01T00:00:00Z"
        );
        assert!(filetime(0).is_none());
        assert!(filetime(u64::MAX).is_none());
        let date = ((2024 - 1980) << 9) | (2 << 5) | 29;
        let local = dos(date, 9 << 11, 0, None).unwrap();
        let zoned = dos(date, 9 << 11, 0, Some(0x80 | 36)).unwrap();
        assert!(local.unix_seconds.is_none());
        assert_eq!(zoned.value, "2024-02-29T09:00:00+09:00");
        assert_eq!(
            filetime(((zoned.unix_seconds.unwrap() + 11644473600) as u64) * 10_000_000)
                .unwrap()
                .value,
            "2024-02-29T00:00:00Z"
        );
        assert!(dos(((2023 - 1980) << 9) | (2 << 5) | 29, 0, 0, None).is_none());
        assert!(dos(date, 0, 200, None).is_none());
    }
}
