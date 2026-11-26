//! Local time formatting and calendar boundaries backed by the Win32 calendar API.

use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Time::{
    FileTimeToSystemTime, SystemTimeToFileTime, SystemTimeToTzSpecificLocalTime, TzSpecificLocalTimeToSystemTime,
};

pub fn format_local(unix_secs: u64) -> String {
    // FILETIME: 100ns ticks since 1601-01-01, unix epoch is 11644473600s later.
    let ticks = (unix_secs as u128 + 11_644_473_600) * 10_000_000;
    let ft = FILETIME { dwLowDateTime: (ticks & 0xFFFF_FFFF) as u32, dwHighDateTime: (ticks >> 32) as u32 };
    let mut st = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    unsafe {
        if FileTimeToSystemTime(&ft, &mut st).is_err() {
            return String::new();
        }
        if SystemTimeToTzSpecificLocalTime(None, &st, &mut local).is_err() {
            return String::new();
        }
    }
    format!("{:04}/{:02}/{:02} {:02}:{:02}", local.wYear, local.wMonth, local.wDay, local.wHour, local.wMinute)
}

/// Calendar grouping thresholds in UNIX seconds computed from local wall clock.
#[derive(Clone, Copy)]
pub struct DateBuckets {
    pub today: u64,
    pub yesterday: u64,
    pub this_week: u64,
    pub last_week: u64,
    pub this_month: u64,
    pub this_year: u64,
}

fn local_ymd_unix(year: u16, month: u16, day: u16) -> u64 {
    let local = SYSTEMTIME { wYear: year, wMonth: month, wDay: day, ..Default::default() };
    let mut utc = SYSTEMTIME::default();
    let mut ft = FILETIME::default();
    unsafe {
        if TzSpecificLocalTimeToSystemTime(None, &local, &mut utc).is_err() {
            return 0;
        }
        if SystemTimeToFileTime(&utc, &mut ft).is_err() {
            return 0;
        }
    }
    let ticks = ((ft.dwHighDateTime as u64) << 32) | ft.dwLowDateTime as u64;
    (ticks / 10_000_000).saturating_sub(11_644_473_600)
}

pub fn local_date_buckets() -> DateBuckets {
    let now = unsafe { GetLocalTime() };
    let today = local_ymd_unix(now.wYear, now.wMonth, now.wDay);
    let yesterday = today.saturating_sub(86_400);
    // Monday-based week start (SYSTEMTIME.wDayOfWeek: 0=Sun, 1=Mon..6=Sat).
    let days_since_mon = if now.wDayOfWeek == 0 { 6 } else { now.wDayOfWeek - 1 } as u64;
    let this_week = today.saturating_sub(days_since_mon * 86_400);
    let last_week = this_week.saturating_sub(7 * 86_400);
    let this_month = local_ymd_unix(now.wYear, now.wMonth, 1);
    let this_year = local_ymd_unix(now.wYear, 1, 1);
    DateBuckets { today, yesterday, this_week, last_week, this_month, this_year }
}
