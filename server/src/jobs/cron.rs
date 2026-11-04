//! A small 5-field cron (`minute hour day-of-month month day-of-week`, UTC) and next-run
//! computation for schedules. Fields accept `*`, numbers, `a-b` ranges, `,` lists and `/n` steps
//! (`*/15`, `0-30/5`). Day-of-week is 0-6 (Sunday = 0; 7 is also Sunday). As in classic cron, when
//! both day-of-month and day-of-week are restricted a day matches if either does.

#[derive(Debug, Clone, PartialEq)]
pub struct Cron {
    min: u64,   // bit per minute 0-59
    hour: u32,  // 0-23
    dom: u32,   // 1-31
    month: u16, // 1-12
    dow: u8,    // 0-6
    dom_any: bool,
    dow_any: bool,
}

fn field(s: &str, lo: u32, hi: u32, name: &str) -> Result<(u64, bool), String> {
    let mut bits = 0u64;
    let any = s == "*";
    for part in s.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, st)) => (r, st.parse::<u32>().map_err(|_| format!("cron {name}: bad step in {part:?}"))?),
            None => (part, 1),
        };
        if step == 0 {
            return Err(format!("cron {name}: step must be > 0"));
        }
        let (a, b) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            let p = |x: &str| x.parse::<u32>().map_err(|_| format!("cron {name}: bad value {x:?}"));
            (p(a)?, p(b)?)
        } else {
            let v = range.parse::<u32>().map_err(|_| format!("cron {name}: bad value {range:?}"))?;
            (v, if part.contains('/') { hi } else { v })
        };
        if a < lo || b > hi || a > b {
            return Err(format!("cron {name}: {part:?} is outside {lo}-{hi}"));
        }
        let mut v = a;
        while v <= b {
            bits |= 1 << v;
            v += step;
        }
    }
    Ok((bits, any))
}

impl Cron {
    pub fn parse(expr: &str) -> Result<Cron, String> {
        let f: Vec<&str> = expr.split_whitespace().collect();
        if f.len() != 5 {
            return Err("cron must have 5 fields: minute hour day-of-month month day-of-week".into());
        }
        let (min, _) = field(f[0], 0, 59, "minute")?;
        let (hour, _) = field(f[1], 0, 23, "hour")?;
        let (dom, dom_any) = field(f[2], 1, 31, "day-of-month")?;
        let (month, _) = field(f[3], 1, 12, "month")?;
        let (mut dow, dow_any) = field(f[4], 0, 7, "day-of-week")?;
        if dow & (1 << 7) != 0 {
            dow |= 1;
        }
        Ok(Cron { min, hour: hour as u32, dom: dom as u32, month: month as u16, dow: (dow & 0x7f) as u8, dom_any, dow_any })
    }

    fn day_matches(&self, d: u32, m: u32, wd: u32) -> bool {
        if self.month & (1 << m) == 0 {
            return false;
        }
        let dm = self.dom & (1 << d) != 0;
        let wm = self.dow & (1 << wd) != 0;
        match (self.dom_any, self.dow_any) {
            (true, true) => true,
            (false, true) => dm,
            (true, false) => wm,
            (false, false) => dm || wm,
        }
    }

    /// The first matching minute strictly after `after` (unix seconds, UTC). None if there is
    /// none within ~5 years (e.g. `0 0 31 2 *`).
    pub fn next_after(&self, after: i64) -> Option<i64> {
        let mut t = (after.div_euclid(60) + 1) * 60;
        let limit = after + 5 * 366 * 86400;
        while t <= limit {
            let days = t.div_euclid(86400);
            let (y, m, d) = civil(days);
            let _ = y;
            let wd = (days + 4).rem_euclid(7) as u32; // 1970-01-01 was a Thursday
            if !self.day_matches(d, m, wd) {
                t = (days + 1) * 86400;
                continue;
            }
            let sod = t.rem_euclid(86400);
            let h = (sod / 3600) as u32;
            if self.hour & (1 << h) == 0 {
                t = days * 86400 + (h as i64 + 1) * 3600;
                continue;
            }
            let mi = ((sod % 3600) / 60) as u32;
            if self.min & (1 << mi) == 0 {
                t += 60;
                continue;
            }
            return Some(t);
        }
        None
    }
}

/// Days since the epoch -> (year, month 1-12, day 1-31). (Howard Hinnant's algorithm.)
fn civil(z: i64) -> (i64, u32, u32) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;
    // 2026-11-04 10:17:30 UTC (a Wednesday)
    const T: i64 = 1_793_787_450;

    #[test]
    fn parses_and_rejects() {
        assert!(Cron::parse("* * * * *").is_ok());
        assert!(Cron::parse("*/15 0-6,22 1 */2 1-5").is_ok());
        assert!(Cron::parse("60 * * * *").is_err());
        assert!(Cron::parse("* * *").is_err());
        assert!(Cron::parse("*/0 * * * *").is_err());
        assert!(Cron::parse("x * * * *").is_err());
    }

    #[test]
    fn next_runs() {
        assert_eq!(civil(T.div_euclid(86400)), (2026, 11, 4));
        let every = Cron::parse("* * * * *").unwrap();
        assert_eq!(every.next_after(T), Some(T - 30 + 60));
        let q = Cron::parse("*/15 * * * *").unwrap();
        assert_eq!(q.next_after(T), Some(T - 17 * 60 - 30 + 30 * 60)); // 10:30
        let daily = Cron::parse("30 3 * * *").unwrap();
        let midnight = T - T.rem_euclid(86400);
        assert_eq!(daily.next_after(T), Some(midnight + 86400 + 3 * 3600 + 30 * 60));
        // Sundays at 04:00 -> 2026-11-08
        let sun = Cron::parse("0 4 * * 0").unwrap();
        assert_eq!(sun.next_after(T), Some(midnight + 4 * 86400 + 4 * 3600));
        assert_eq!(Cron::parse("0 4 * * 7").unwrap().next_after(T), sun.next_after(T));
        // 1st of the month OR a Monday -> Monday 2026-11-09
        let either = Cron::parse("0 0 1 * 1").unwrap();
        assert_eq!(either.next_after(T), Some(midnight + 5 * 86400));
        assert_eq!(Cron::parse("0 0 31 2 *").unwrap().next_after(T), None);
    }
}
