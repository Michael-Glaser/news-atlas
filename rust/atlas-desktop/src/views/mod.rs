//! The five views and the pieces they share.

pub mod articles;
pub mod brief;
pub mod chat;
pub mod graph;
pub mod license;
pub mod map;
pub mod monitors;
pub mod overview;
pub mod reader;
pub mod settings;
pub mod topics;

use crate::theme;

/// A card: a hairline box with a title row and whatever the caller puts inside.
pub fn card<R>(
    ui: &mut egui::Ui,
    title: &str,
    note: &str,
    body: impl FnOnce(&mut egui::Ui) -> R,
) -> R {
    // The card's inner margin is `space::MD`, and egui counts the hairline as part of it: the
    // frame's own documentation (`egui-0.37.0/src/containers/frame.rs:13`) gives its size as
    // `content_size + inner_margin + 2 * stroke.width`. `Margin::same(MD)` therefore paints the
    // text 13 px from the fill's edge — measured with `brief.rs`'s paint walker, and read by
    // `tests/design_audit` as 13 px, or 14 px where the page's first pixel falls on a half.
    // That extra pixel is the stroke, not a spacing decision, and it put the card off the 4 px
    // scale. Subtracting the hairline keeps the padding a reader sees at exactly `space::MD`.
    let inset = theme::space::MD as i8 - theme::HAIRLINE_WIDTH as i8;
    let frame = egui::Frame::NONE
        .fill(theme::PANEL)
        .stroke(theme::line())
        .inner_margin(egui::Margin::same(inset))
        .corner_radius(egui::CornerRadius::same(2));
    frame
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(theme::card_title(title));
                if !note.is_empty() {
                    ui.add_space(6.0);
                    ui.label(theme::dim(note));
                }
            });
            ui.add_space(8.0);
            body(ui)
        })
        .inner
}

/// A page heading: the title, and one sentence that says what the page is.
pub fn heading(ui: &mut egui::Ui, title: &str, note: &str) {
    ui.label(theme::page_title(title));
    if !note.is_empty() {
        ui.add_space(2.0);
        ui.label(theme::dim(note));
    }
    ui.add_space(12.0);
}

/// `1204` -> `1 204`. Counts are read, not decoded.
pub fn nf(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::new();
    for (index, ch) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index) % 3 == 0 {
            out.push(' ');
        }
        out.push(ch);
    }
    if value < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// `2026-11-21T10:25:12Z` -> `10:25` or `20 Nov`, by how far back it is.
///
/// A time label is drawn on four pages (Overview, Articles, Topics, Monitors) from a
/// column this window does not write, so this function has two duties that matter more
/// than its formatting:
///
/// * **It never panics.** The old version sliced `&day[8..10]` after only checking that
///   byte 10 and byte 16 happened to be boundaries, so a stamp holding any multi-byte
///   character in those positions — a locale-formatted date, a euro sign — panicked while
///   slicing, on the default page, from one row. A panic unwinds through the window's
///   event callback, and the window closes. Measured before the fix: `2026-11€T10:25:12Z`
///   → `start byte index 8 is not a char boundary`.
/// * **It never returns more than a label.** A stamp it cannot read used to be echoed
///   whole — measured, a 20 000-character `published` became a 20 000-character time
///   label and a 298-row-tall story. The fallback is now bounded, and it says what it is.
pub fn when(stamp: &str) -> String {
    // The shape first, by bytes, before any slicing: a stamp is `YYYY-MM-DDTHH:MM:SSZ`.
    // Checking the digits rather than trusting the length is what makes the slices below
    // safe — a byte that is an ASCII digit cannot be part of a multi-byte character.
    let bytes = stamp.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        bytes
            .get(range)
            .is_some_and(|slice| slice.iter().all(u8::is_ascii_digit))
    };
    let shaped = bytes.len() >= 16
        && digits(0..4)
        && bytes.get(4) == Some(&b'-')
        && digits(5..7)
        && bytes.get(7) == Some(&b'-')
        && digits(8..10)
        && !digits(10..16);

    if !shaped {
        // Not a stamp: say so, in one bounded line. Two things happen here and both are
        // load-bearing. Control characters (a newline from a multi-line column, a NUL from
        // stored bytes) become a space, because this label sits inside a *row*: a newline
        // would forge a second line and the reader would read a second story. Then `clip`
        // cuts on a character boundary, so a 20 000-character value cannot become a
        // 20 000-character time label.
        let flat: String = stamp
            .trim()
            .chars()
            .map(|ch| if ch.is_control() { ' ' } else { ch })
            .collect();
        return clip(&flat, 24);
    }

    let today = utc_day();
    if bytes.get(0..10) == Some(today.as_bytes()) {
        String::from_utf8_lossy(&bytes[11..16]).into_owned()
    } else {
        let day = String::from_utf8_lossy(&bytes[8..10]);
        let month = month(&String::from_utf8_lossy(&bytes[5..7]));
        format!("{day} {month}")
    }
}

/// Cut a text to `limit` characters at a word boundary.
///
/// One implementation, in the module every view shares, because it is the answer to a
/// question all of them ask: how much of a string this crate did not write may reach a
/// label. It cuts on a character boundary and appends an ellipsis, so a caller can use it
/// on a stored address, a headline or a model's answer without thinking about UTF-8.
pub fn clip(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit {
        return text.to_string();
    }
    let mut out: String = text.chars().take(limit).collect();
    if let Some(cut) = out.rfind(' ') {
        out.truncate(cut);
    }
    out.push('…');
    out
}

fn month(number: &str) -> &'static str {
    match number {
        "01" => "Jan",
        "02" => "Feb",
        "03" => "Mar",
        "04" => "Apr",
        "05" => "May",
        "06" => "Jun",
        "07" => "Jul",
        "08" => "Aug",
        "09" => "Sep",
        "10" => "Oct",
        "11" => "Nov",
        "12" => "Dec",
        _ => "",
    }
}

fn utc_day() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|spent| spent.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    format!("{:04}-{:02}-{:02}", year, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_count_reads_with_thin_spaces() {
        assert_eq!(nf(0), "0");
        assert_eq!(nf(999), "999");
        assert_eq!(nf(1204), "1 204");
        assert_eq!(nf(7483210), "7 483 210");
    }

    #[test]
    fn a_stamp_becomes_a_time_or_a_day() {
        assert_eq!(when(""), "");
        let today = utc_day();
        assert_eq!(when(&format!("{today}T09:05:00Z")), "09:05");
        assert_eq!(when("2026-03-04T09:05:00Z"), "04 Mar");
    }

    #[test]
    fn a_stamp_that_is_not_a_stamp_is_never_echoed_whole() {
        // The time label of a row, from a column this window does not write. A stamp it
        // cannot read used to be returned as-is: measured, a 20 000-character `published`
        // became a 20 000-character label and a 298-row-tall story.
        let long = "x".repeat(20_000);
        assert_eq!(when(&long).chars().count(), 25, "the fallback is one clipped line");
        // …and it is clipped on a character boundary, not a byte.
        let umlauts = "ä".repeat(200);
        assert!(when(&umlauts).chars().count() <= 25);
    }

    #[test]
    fn a_stamp_with_a_wide_character_where_a_date_belongs_does_not_panic() {
        // The severe case: `&day[8..10]` used to slice through a multi-byte character and
        // panic, which closes the window (the frame runs inside the platform's event
        // callback). Every one of these reached it — a locale-formatted date, a currency
        // symbol, an override character.
        for stamp in [
            "2026-11€T10:25:12Z",
            "2026-1€-21T10:25:12Z",
            "2026-11-2€T10:25:12Z",
            "2026-11-21\u{202e}0:25:12Z",
            "2026-11-21T10:25:12Z\n",
            "not a date at all, but long enough",
            "\u{202e}2026-11-21T10:25:12Z",
        ] {
            let label = when(stamp);
            assert!(!label.is_empty(), "{stamp:?} produced an empty label");
            assert!(label.chars().count() <= 25, "{stamp:?} produced {label:?}");
        }
    }

    #[test]
    fn a_stamp_whose_digits_are_missing_is_not_read_as_a_date() {
        // Shape first: `2026-XX-21T…` is not a stamp, so it must not be formatted as one.
        for stamp in [
            "2026-ab-21T10:25:12Z",
            "2026-11-2XT10:25:12Z",
            "20XX-11-21T10:25:12Z",
            "2026-11-21",
        ] {
            assert_eq!(when(stamp), stamp, "{stamp:?} should fall back to itself");
        }
    }
}
