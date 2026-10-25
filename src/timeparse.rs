//! Parses the time expressions a human (or a model) might write in chat
//! into something concrete.
//!
//! Supported forms:
//!   "in 20m", "in 2 hours", "in 1h30m"       one-shot, relative to now
//!   "at 18:30", "18:30"                       one-shot, today (or tomorrow if past)
//!   "tomorrow", "tomorrow 09:00"              one-shot
//!   "2026-10-26 09:00", RFC 3339              one-shot, absolute
//!   "daily 08:00", "every 30m", "every 2 h"   recurring (stored as cron)
//!   "*/10 * * * *" (classic 5-field crontab)  recurring
//!
//! All times without a timezone are interpreted in the machine's local time.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Local, NaiveDate, NaiveDateTime, TimeDelta, TimeZone, Timelike, Utc};

/// What a time expression parsed into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParsedTime {
    /// Fires once at this moment (UTC).
    Once(DateTime<Utc>),
    /// Fires repeatedly; a classic 5-field cron expression like "*/10 * * * *".
    Recurring(String),
}

/// Parse a time expression. `now` is passed in so tests can be deterministic.
pub fn parse(text: &str, now: DateTime<Local>) -> Result<ParsedTime> {
    let raw = text.trim();
    let lower = raw.to_lowercase();

    // --- recurring forms first -----------------------------------------
    if let Some(rest) = lower.strip_prefix("every ")
        && let Some(cron) = every_to_cron(rest)
    {
        next_run(&cron)?; // validate now, so bad input is reported early
        return Ok(ParsedTime::Recurring(cron));
    }
    if lower == "daily" || lower.starts_with("daily ") {
        let (hour, minute) = match lower.strip_prefix("daily").unwrap_or("").trim() {
            "" => (9, 0), // bare "daily" defaults to 09:00
            clock => parse_hhmm(clock)?,
        };
        return Ok(ParsedTime::Recurring(format!("{minute} {hour} * * *")));
    }
    if looks_like_cron(raw) {
        next_run(raw)?; // validate
        return Ok(ParsedTime::Recurring(raw.to_string()));
    }

    // --- one-shot forms --------------------------------------------------
    if let Some(rest) = lower.strip_prefix("in ")
        && let Some(delta) = parse_duration(rest)
    {
        return Ok(ParsedTime::Once((now + delta).with_timezone(&Utc)));
    }
    if lower == "tomorrow" || lower.starts_with("tomorrow ") {
        let (hour, minute) = match lower.strip_prefix("tomorrow").unwrap_or("").trim() {
            "" => (now.hour(), now.minute()), // bare "tomorrow" = same clock time
            clock => parse_hhmm(clock)?,
        };
        let date = now.date_naive().succ_opt().context("date out of range")?;
        return Ok(ParsedTime::Once(local_at(date, hour, minute)?));
    }
    let maybe_clock = lower.strip_prefix("at ").map(str::trim).unwrap_or(&lower);
    if looks_like_clock(maybe_clock) {
        let (hour, minute) = parse_hhmm(maybe_clock)?;
        let today = local_at(now.date_naive(), hour, minute)?;
        let now_utc = now.with_timezone(&Utc);
        // A clock time already past today means the next occurrence: tomorrow.
        let at = if today <= now_utc {
            let date = now.date_naive().succ_opt().context("date out of range")?;
            local_at(date, hour, minute)?
        } else {
            today
        };
        return Ok(ParsedTime::Once(at));
    }
    if let Ok(parsed) = DateTime::parse_from_rfc3339(raw) {
        return Ok(ParsedTime::Once(parsed.with_timezone(&Utc)));
    }
    if let Ok(naive) = NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M")
        && let Some(local) = Local.from_local_datetime(&naive).earliest()
    {
        return Ok(ParsedTime::Once(local.with_timezone(&Utc)));
    }

    bail!("could not understand the time '{raw}'")
}

/// "HH:MM".
fn parse_hhmm(text: &str) -> Result<(u32, u32)> {
    let (hour, minute) = text
        .split_once(':')
        .with_context(|| format!("expected HH:MM, got '{text}'"))?;
    let hour: u32 = hour.trim().parse().context("bad hour")?;
    let minute: u32 = minute.trim().parse().context("bad minute")?;
    if hour > 23 || minute > 59 {
        bail!("'{text}' is not a valid clock time");
    }
    Ok((hour, minute))
}

/// A local date + clock time -> UTC instant. On DST changes, `earliest()`
/// picks the first occurrence of an ambiguous local time.
fn local_at(date: NaiveDate, hour: u32, minute: u32) -> Result<DateTime<Utc>> {
    let naive = date
        .and_hms_opt(hour, minute, 0)
        .context("invalid clock time")?;
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|t| t.with_timezone(&Utc))
        .with_context(|| format!("no such local time: {naive}"))
}

/// "20m", "2 hours", "1h30m", "1 day 12 hours" -> a duration.
/// Returns None when we don't understand the text (the caller then tries
/// other formats or reports an error).
fn parse_duration(text: &str) -> Option<TimeDelta> {
    let mut seconds: i64 = 0;
    let mut number = String::new();
    let mut saw_any = false;
    let mut chars = text.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_ascii_digit() {
            number.push(c);
            chars.next();
        } else if c.is_whitespace() || c == ',' {
            // Separators are skipped; a number seen so far waits for its unit,
            // which may arrive as a word ("2 hours").
            chars.next();
        } else {
            // A run of letters is a unit word: "h", "hours", "min", "mins"...
            let mut word = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_alphabetic() {
                    word.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            let unit = unit_letter(&word)?;
            let value: i64 = number.parse().ok()?;
            number.clear();
            seconds += value * unit;
            saw_any = true;
        }
    }
    // A trailing number without a unit, or nothing parsed at all, is invalid.
    if !number.is_empty() || !saw_any || seconds <= 0 {
        return None;
    }
    TimeDelta::try_seconds(seconds)
}

/// Map a unit word to its length in seconds: d(ay), h(our), m(in), s(econd).
fn unit_letter(word: &str) -> Option<i64> {
    let singular = word.trim_end_matches('s'); // "hours" -> "hour", "mins" -> "min"
    match singular.chars().next()? {
        'd' => Some(86_400),
        'h' => Some(3_600),
        'm' => Some(60),
        's' => Some(1),
        _ => None,
    }
}

/// "every ..." -> a cron expression, for the simple cases ("every 30m",
/// "every 2 hours"). None when the interval has no clean cron form.
fn every_to_cron(rest: &str) -> Option<String> {
    let seconds = parse_duration(rest)?.num_seconds();
    if seconds % 3_600 == 0 && seconds / 3_600 <= 23 {
        Some(format!("0 */{} * * *", seconds / 3_600)) // every N hours, on the hour
    } else if seconds % 60 == 0 && seconds / 60 <= 59 {
        Some(format!("*/{} * * * *", seconds / 60)) // every N minutes
    } else {
        None
    }
}

/// Classic 5-field numeric crontab syntax ("*/10 * * * *"). Weekday/month
/// names are not supported in v1.
fn looks_like_cron(text: &str) -> bool {
    let fields = text.split_whitespace().count();
    (5..=6).contains(&fields)
        && text
            .chars()
            .all(|c| c.is_ascii_digit() || matches!(c, '*' | '-' | ',' | '/' | ' '))
}

/// Something like "18:30" (but not "2026-10-26 18:30", which has a space
/// and more colons context).
fn looks_like_clock(text: &str) -> bool {
    text.len() <= 5 && text.contains(':') && !text.contains(' ')
}

/// The `cron` crate wants a seconds field (6 fields: sec min hour dom mon dow),
/// while humans write classic 5-field crontab entries. We prepend "0 "
/// ("top of the minute") when needed.
pub fn cron_to_six(expr: &str) -> String {
    if expr.split_whitespace().count() == 5 {
        format!("0 {expr}")
    } else {
        expr.to_string()
    }
}

/// The next time a cron expression fires, strictly after now.
pub fn next_run(expr: &str) -> Result<DateTime<Utc>> {
    let schedule: cron::Schedule = cron_to_six(expr)
        .parse()
        .with_context(|| format!("invalid cron expression '{expr}'"))?;
    schedule
        .upcoming(Utc)
        .next()
        .ok_or_else(|| anyhow!("cron expression '{expr}' never fires"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fixed "now" so tests are deterministic: 2026-10-25 10:00 local.
    fn now() -> DateTime<Local> {
        Local.with_ymd_and_hms(2026, 10, 25, 10, 0, 0).unwrap()
    }

    /// Helper: build the expected UTC instant from local parts.
    fn at_local(y: i32, m: u32, d: u32, h: u32, min: u32) -> DateTime<Utc> {
        Local
            .with_ymd_and_hms(y, m, d, h, min, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn relative_times() {
        assert_eq!(
            parse("in 20m", now()).unwrap(),
            ParsedTime::Once(at_local(2026, 10, 25, 10, 20))
        );
        assert_eq!(
            parse("in 1h30m", now()).unwrap(),
            ParsedTime::Once(at_local(2026, 10, 25, 11, 30))
        );
        assert_eq!(
            parse("in 2 hours", now()).unwrap(),
            ParsedTime::Once(at_local(2026, 10, 25, 12, 0))
        );
        let in_90s = (now() + TimeDelta::try_seconds(90).unwrap()).with_timezone(&Utc);
        assert_eq!(
            parse("in 90 seconds", now()).unwrap(),
            ParsedTime::Once(in_90s)
        );
    }

    #[test]
    fn clock_times_today_or_tomorrow() {
        assert_eq!(
            parse("at 18:30", now()).unwrap(),
            ParsedTime::Once(at_local(2026, 10, 25, 18, 30))
        );
        assert_eq!(
            parse("18:30", now()).unwrap(),
            ParsedTime::Once(at_local(2026, 10, 25, 18, 30))
        );
        // 05:00 already passed today -> tomorrow.
        assert_eq!(
            parse("at 05:00", now()).unwrap(),
            ParsedTime::Once(at_local(2026, 10, 26, 5, 0))
        );
    }

    #[test]
    fn tomorrow_forms() {
        assert_eq!(
            parse("tomorrow 09:00", now()).unwrap(),
            ParsedTime::Once(at_local(2026, 10, 26, 9, 0))
        );
        assert_eq!(
            parse("tomorrow", now()).unwrap(),
            ParsedTime::Once(at_local(2026, 10, 26, 10, 0))
        );
    }

    #[test]
    fn absolute_times() {
        assert_eq!(
            parse("2026-10-26 09:00", now()).unwrap(),
            ParsedTime::Once(at_local(2026, 10, 26, 9, 0))
        );
        assert_eq!(
            parse("2026-10-26T09:00:00Z", now()).unwrap(),
            ParsedTime::Once(
                DateTime::parse_from_rfc3339("2026-10-26T09:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc)
            )
        );
    }

    #[test]
    fn recurring_forms() {
        assert_eq!(
            parse("every 30m", now()).unwrap(),
            ParsedTime::Recurring("*/30 * * * *".into())
        );
        assert_eq!(
            parse("every 2 hours", now()).unwrap(),
            ParsedTime::Recurring("0 */2 * * *".into())
        );
        assert_eq!(
            parse("daily 08:00", now()).unwrap(),
            ParsedTime::Recurring("0 8 * * *".into())
        );
        assert_eq!(
            parse("daily", now()).unwrap(),
            ParsedTime::Recurring("0 9 * * *".into())
        );
        assert_eq!(
            parse("*/10 * * * *", now()).unwrap(),
            ParsedTime::Recurring("*/10 * * * *".into())
        );
        // "every 90m" has no clean cron form and no other meaning -> error.
        assert!(parse("every 90m", now()).is_err());
    }

    #[test]
    fn garbage_is_an_error() {
        assert!(parse("when the moon is full", now()).is_err());
        assert!(parse("", now()).is_err());
        assert!(parse("in 20", now()).is_err()); // number without unit
    }

    #[test]
    fn cron_five_fields_get_seconds_field() {
        assert_eq!(cron_to_six("*/10 * * * *"), "0 */10 * * * *");
        assert_eq!(cron_to_six("0 */10 * * * *"), "0 */10 * * * *");

        // And the converted form parses and computes the next run correctly:
        let schedule: cron::Schedule = cron_to_six("*/10 * * * *").parse().unwrap();
        let after = Utc.with_ymd_and_hms(2026, 10, 25, 10, 0, 5).unwrap();
        assert_eq!(
            schedule.after(&after).next().unwrap(),
            Utc.with_ymd_and_hms(2026, 10, 25, 10, 10, 0).unwrap()
        );
    }
}
