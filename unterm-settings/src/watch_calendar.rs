//! A calendar, read from its private iCalendar address (Google Calendar's
//! "secret address in iCal format", Outlook's published ICS, iCloud's
//! public link): which events start soon.
//!
//! Recurring events are expanded for the common rules -- daily, weekly (on
//! given days), monthly on the same date, yearly -- with their interval,
//! count, end and excluded dates. A rule beyond that counts only its first
//! time. Times with a named zone are read as this computer's local time,
//! which is right for a person's own calendar; UTC times are exact.
//! All-day events have no start to be early for and are left out.

use std::convert::TryFrom;
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Datelike, Duration as Span, Local, NaiveDate, NaiveDateTime, TimeZone, Utc, Weekday};

/// One time an event happens.
#[derive(Clone, Debug, PartialEq)]
pub struct Occurrence {
    /// Stays the same for the same event at the same time.
    pub key: String,
    pub start: DateTime<Utc>,
    pub summary: String,
    pub location: String,
    pub description: String,
}

const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_EXPANSION: usize = 20_000;

/// Fetch the calendar's text, following a few redirects.
pub fn fetch(url: &str) -> Result<String> {
    use http_req::request::{Method, Request};
    use http_req::uri::Uri;
    let mut url = url.trim().to_string();
    if let Some(rest) = url.strip_prefix("webcal://") {
        url = format!("https://{rest}");
    }
    for _ in 0..4 {
        let uri = Uri::try_from(url.as_str()).with_context(|| "not a calendar address".to_string())?;
        let mut body = Vec::new();
        let response = Request::new(&uri)
            .method(Method::GET)
            .header("User-Agent", "calendar-reader/1.0")
            .header("Accept", "text/calendar, */*")
            .connect_timeout(Some(Duration::from_secs(10)))
            .read_timeout(Some(Duration::from_secs(30)))
            .timeout(Duration::from_secs(45))
            .send(&mut body)
            .map_err(|e| anyhow!("could not read the calendar: {e}"))?;
        let status = u16::from(response.status_code());
        if (300..400).contains(&status) {
            let next = response.headers().get("Location").ok_or_else(|| anyhow!("the calendar moved without saying where"))?;
            url = if next.starts_with("http") { next.to_string() } else { bail!("the calendar moved somewhere unreadable") };
            continue;
        }
        if status != 200 {
            bail!("the calendar address answered {status}");
        }
        if body.len() > MAX_BYTES {
            bail!("the calendar is too large to read");
        }
        let text = String::from_utf8_lossy(&body).into_owned();
        if !text.contains("BEGIN:VCALENDAR") {
            bail!("that address is not a calendar (iCal) feed");
        }
        return Ok(text);
    }
    bail!("the calendar address redirects too often")
}

/// Lines of an iCalendar text, unfolded, split into (name, params, value).
fn lines(text: &str) -> Vec<(String, String, String)> {
    let mut unfolded: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let raw = raw.trim_end_matches('\r');
        if raw.starts_with([' ', '\t']) {
            if let Some(last) = unfolded.last_mut() {
                last.push_str(&raw[1..]);
            }
        } else {
            unfolded.push(raw.to_string());
        }
    }
    unfolded
        .into_iter()
        .filter_map(|line| {
            // The value starts at the first colon outside a quoted parameter.
            let mut quoted = false;
            let at = line.char_indices().find(|(_, c)| {
                if *c == '"' {
                    quoted = !quoted;
                }
                *c == ':' && !quoted
            })?.0;
            let (head, value) = (&line[..at], &line[at + 1..]);
            let (name, params) = head.split_once(';').unwrap_or((head, ""));
            Some((name.to_ascii_uppercase(), params.to_string(), value.to_string()))
        })
        .collect()
}

fn unescape(text: &str) -> String {
    text.replace("\\n", "\n").replace("\\N", "\n").replace("\\,", ",").replace("\\;", ";").replace("\\\\", "\\")
}

/// A date-time value: UTC with `Z`, else local. `None` for a whole day.
fn moment(params: &str, value: &str) -> Option<DateTime<Utc>> {
    if params.to_ascii_uppercase().contains("VALUE=DATE") && !params.to_ascii_uppercase().contains("VALUE=DATE-TIME") {
        return None;
    }
    let value = value.trim();
    if let Some(utc) = value.strip_suffix('Z') {
        let naive = NaiveDateTime::parse_from_str(utc, "%Y%m%dT%H%M%S").ok()?;
        return Some(Utc.from_utc_datetime(&naive));
    }
    let naive = NaiveDateTime::parse_from_str(value, "%Y%m%dT%H%M%S").ok()?;
    Local.from_local_datetime(&naive).earliest().map(|at| at.with_timezone(&Utc))
}

#[derive(Default)]
struct Event {
    uid: String,
    start: Option<DateTime<Utc>>,
    start_local: Option<NaiveDateTime>,
    utc: bool,
    summary: String,
    location: String,
    description: String,
    rule: Option<String>,
    excluded: Vec<DateTime<Utc>>,
    recurrence_id: Option<DateTime<Utc>>,
    cancelled: bool,
}

fn parse_events(text: &str) -> Vec<Event> {
    let mut events = Vec::new();
    let mut current: Option<Event> = None;
    for (name, params, value) in lines(text) {
        match (name.as_str(), value.as_str()) {
            ("BEGIN", "VEVENT") => current = Some(Event::default()),
            ("END", "VEVENT") => {
                if let Some(event) = current.take() {
                    events.push(event);
                }
            }
            _ => {
                let Some(event) = current.as_mut() else { continue };
                match name.as_str() {
                    "UID" => event.uid = value.clone(),
                    "DTSTART" => {
                        event.start = moment(&params, &value);
                        event.utc = value.trim().ends_with('Z');
                        event.start_local = NaiveDateTime::parse_from_str(value.trim().trim_end_matches('Z'), "%Y%m%dT%H%M%S").ok();
                    }
                    "SUMMARY" => event.summary = unescape(&value),
                    "LOCATION" => event.location = unescape(&value),
                    "DESCRIPTION" => event.description = unescape(&value),
                    "RRULE" => event.rule = Some(value.clone()),
                    "EXDATE" => event.excluded.extend(value.split(',').filter_map(|v| moment(&params, v))),
                    "RECURRENCE-ID" => event.recurrence_id = moment(&params, &value),
                    "STATUS" => event.cancelled = value.trim().eq_ignore_ascii_case("CANCELLED"),
                    _ => {}
                }
            }
        }
    }
    events
}

fn weekday(code: &str) -> Option<Weekday> {
    Some(match code.trim().trim_start_matches(|c: char| c == '+' || c == '-' || c.is_ascii_digit()) {
        "MO" => Weekday::Mon,
        "TU" => Weekday::Tue,
        "WE" => Weekday::Wed,
        "TH" => Weekday::Thu,
        "FR" => Weekday::Fri,
        "SA" => Weekday::Sat,
        "SU" => Weekday::Sun,
        _ => return None,
    })
}

fn add_months(date: NaiveDate, months: i32) -> Option<NaiveDate> {
    let total = date.year() * 12 + date.month0() as i32 + months;
    NaiveDate::from_ymd_opt(total.div_euclid(12), total.rem_euclid(12) as u32 + 1, date.day())
}

/// The starts of an event from `from` to `until`, its rule expanded.
fn starts(event: &Event, from: DateTime<Utc>, until: DateTime<Utc>) -> Vec<DateTime<Utc>> {
    let Some(first) = event.start else { return Vec::new() };
    let Some(rule) = &event.rule else {
        return if first >= from && first <= until { vec![first] } else { Vec::new() };
    };
    let parts: Vec<(String, String)> = rule
        .split(';')
        .filter_map(|p| p.split_once('=').map(|(k, v)| (k.to_ascii_uppercase(), v.to_string())))
        .collect();
    let get = |key: &str| parts.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str());
    let interval = get("INTERVAL").and_then(|v| v.parse::<i64>().ok()).filter(|n| *n > 0).unwrap_or(1);
    let count = get("COUNT").and_then(|v| v.parse::<usize>().ok());
    let end = get("UNTIL").and_then(|v| moment(if v.len() == 8 { "VALUE=DATE" } else { "" }, v).or_else(|| {
        NaiveDate::parse_from_str(v, "%Y%m%d").ok().and_then(|d| Local.from_local_datetime(&d.and_hms_opt(23, 59, 59)?).earliest()).map(|d| d.with_timezone(&Utc))
    }));
    let supported_by = parts.iter().all(|(k, _)| matches!(k.as_str(), "FREQ" | "INTERVAL" | "COUNT" | "UNTIL" | "WKST" | "BYDAY"));
    let freq = get("FREQ").unwrap_or("");
    let by_day: Vec<Weekday> = get("BYDAY").map(|v| v.split(',').filter_map(weekday).collect()).unwrap_or_default();
    if !supported_by || (!by_day.is_empty() && freq != "WEEKLY") || !matches!(freq, "DAILY" | "WEEKLY" | "MONTHLY" | "YEARLY") {
        return if first >= from && first <= until { vec![first] } else { Vec::new() };
    }
    // Step in the event's own wall time -- UTC for a UTC start, else local --
    // so a 9:00 meeting stays at 9:00 across a daylight-saving change.
    let local_first = if event.utc {
        first.naive_utc()
    } else {
        event.start_local.unwrap_or_else(|| first.with_timezone(&Local).naive_local())
    };
    let to_utc = |naive: NaiveDateTime| -> Option<DateTime<Utc>> {
        if event.utc {
            Some(Utc.from_utc_datetime(&naive))
        } else {
            Local.from_local_datetime(&naive).earliest().map(|at| at.with_timezone(&Utc))
        }
    };
    let mut out = Vec::new();
    let mut produced = 0usize;
    let time = local_first.time();
    let mut step = 0i64;
    for _ in 0..MAX_EXPANSION {
        let candidates: Vec<NaiveDateTime> = match freq {
            "DAILY" => vec![local_first + Span::days(step * interval)],
            "WEEKLY" => {
                let week_start = local_first.date() - Span::days(local_first.weekday().num_days_from_monday() as i64) + Span::weeks(step * interval);
                if by_day.is_empty() {
                    vec![local_first + Span::weeks(step * interval)]
                } else {
                    let mut days: Vec<NaiveDateTime> = by_day
                        .iter()
                        .map(|d| (week_start + Span::days(d.num_days_from_monday() as i64)).and_time(time))
                        .filter(|at| *at >= local_first)
                        .collect();
                    days.sort();
                    days
                }
            }
            "MONTHLY" => add_months(local_first.date(), (step * interval) as i32).map(|d| d.and_time(time)).into_iter().collect(),
            _ => add_months(local_first.date(), (step * interval * 12) as i32).map(|d| d.and_time(time)).into_iter().collect(),
        };
        step += 1;
        let mut past_end = false;
        for naive in candidates {
            let Some(at) = to_utc(naive) else { continue };
            if end.is_some_and(|end| at > end) || count.is_some_and(|count| produced >= count) {
                past_end = true;
                break;
            }
            produced += 1;
            if at > until {
                past_end = true;
                break;
            }
            if at >= from && !event.excluded.contains(&at) {
                out.push(at);
            }
        }
        if past_end {
            break;
        }
    }
    out
}

/// The times events start from `from` to `until`, earliest first. A moved
/// or cancelled single time of a recurring event replaces its usual one.
pub fn occurrences(text: &str, from: DateTime<Utc>, until: DateTime<Utc>) -> Vec<Occurrence> {
    let events = parse_events(text);
    let overridden: Vec<(String, DateTime<Utc>)> =
        events.iter().filter_map(|e| e.recurrence_id.map(|at| (e.uid.clone(), at))).collect();
    let mut out = Vec::new();
    for event in &events {
        if event.cancelled && event.recurrence_id.is_none() {
            continue;
        }
        for start in starts(event, from, until) {
            if event.recurrence_id.is_none() && overridden.contains(&(event.uid.clone(), start)) {
                continue;
            }
            if event.cancelled {
                continue;
            }
            out.push(Occurrence {
                key: format!("{}@{}", event.uid, start.timestamp()),
                start,
                summary: event.summary.clone(),
                location: event.location.clone(),
                description: event.description.chars().take(800).collect(),
            });
        }
    }
    out.sort_by_key(|o| o.start);
    out.dedup_by(|a, b| a.key == b.key);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(text: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(text).unwrap().with_timezone(&Utc)
    }

    const ICS: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n\
BEGIN:VEVENT\r\nUID:standup\r\nDTSTART:20261116T010000Z\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE,FR\r\nSUMMARY:Stand-up\r\nEXDATE:20261120T010000Z\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:review\r\nDTSTART:20261118T060000Z\r\nSUMMARY:Design review\\, round 2\r\nLOCATION:Room 4\r\nDESCRIPTION:Bring the\\n numbers\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:holiday\r\nDTSTART;VALUE=DATE:20261118\r\nSUMMARY:Day off\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:gone\r\nDTSTART:20261118T070000Z\r\nSTATUS:CANCELLED\r\nSUMMARY:Cancelled\r\nEND:VEVENT\r\n\
BEGIN:VEVENT\r\nUID:standup\r\nRECURRENCE-ID:20261118T010000Z\r\nDTSTART:20261118T020000Z\r\nSUMMARY:Stand-up (moved)\r\nEND:VEVENT\r\n\
END:VCALENDAR\r\n";

    #[test]
    fn events_starting_in_a_window_with_their_rules_and_exceptions() {
        let found = occurrences(ICS, utc("2026-11-17T00:00:00Z"), utc("2026-11-21T00:00:00Z"));
        let listed: Vec<(String, String)> = found.iter().map(|o| (o.start.to_rfc3339(), o.summary.clone())).collect();
        assert_eq!(
            listed,
            vec![
                ("2026-11-18T02:00:00+00:00".to_string(), "Stand-up (moved)".to_string()),
                ("2026-11-18T06:00:00+00:00".to_string(), "Design review, round 2".to_string()),
            ],
            "Wednesday's stand-up moved, Friday's excluded, the all-day and cancelled ones left out"
        );
        assert_eq!(found[1].description, "Bring the\n numbers");
        assert_eq!(found[1].location, "Room 4");
    }

    #[test]
    fn a_rule_stops_at_its_count_and_a_long_line_unfolds() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:x\r\nDTSTART:20261101T090000Z\r\nRRULE:FREQ=DAILY;COUNT=3\r\nSUMMARY:Three\r\n  days\r\nEND:VEVENT\r\nEND:VCALENDAR";
        let found = occurrences(ics, utc("2026-10-01T00:00:00Z"), utc("2026-12-01T00:00:00Z"));
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].summary, "Three days");
        let monthly = "BEGIN:VEVENT\r\nUID:m\r\nDTSTART:20260115T090000Z\r\nRRULE:FREQ=MONTHLY\r\nSUMMARY:Rent\r\nEND:VEVENT";
        let found = occurrences(monthly, utc("2026-11-01T00:00:00Z"), utc("2026-12-31T00:00:00Z"));
        assert_eq!(found.len(), 2);
    }
}
