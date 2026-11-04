//! A calendar file for a note's expiry, with reminders 14, 7 and 2 days before.

/// `YYYYMMDDTHHMMSSZ` of a Unix time.
pub fn stamp(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

/// The `.ics` text. `expiry_secs` comes from the vault's `deposit` event.
pub fn expiry_event(note_id: u32, expiry_secs: u64, now_secs: u64) -> String {
    let mut lines = vec![
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".into(),
        "PRODID:-//Nymira//nymirad//EN".into(),
        "BEGIN:VEVENT".into(),
        format!("UID:nymira-note-{note_id}-{expiry_secs}@local"),
        format!("DTSTAMP:{}", stamp(now_secs)),
        format!("DTSTART:{}", stamp(expiry_secs)),
        format!("SUMMARY:Nymira note {note_id} expires"),
        "DESCRIPTION:Close the note before it expires or the balance goes to the treasury.".into(),
    ];
    for days in [14, 7, 2] {
        lines.extend([
            "BEGIN:VALARM".to_string(),
            "ACTION:DISPLAY".into(),
            format!("DESCRIPTION:Nymira note {note_id} expires in {days} days"),
            format!("TRIGGER:-P{days}D"),
            "END:VALARM".into(),
        ]);
    }
    lines.extend(["END:VEVENT".to_string(), "END:VCALENDAR".into()]);
    lines.join("\r\n") + "\r\n"
}

/// A shareable reminder without a note identifier or funding details.
pub fn private_expiry_event(expiry_secs: u64, now_secs: u64) -> String {
    expiry_event(0, expiry_secs, now_secs)
        .lines()
        .map(|line| {
            if line.starts_with("UID:") {
                format!("UID:{}@local", crate::net::identity())
            } else {
                line.replace("Nymira note 0 expires", "Nymira note expires")
            }
        })
        .collect::<Vec<_>>()
        .join("\r\n")
        + "\r\n"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stamps_are_utc_calendar_dates() {
        assert_eq!(stamp(0), "19700101T000000Z");
        assert_eq!(stamp(951_782_400), "20000229T000000Z");
        assert_eq!(stamp(1_900_000_000), "20300317T174640Z");
    }
}
