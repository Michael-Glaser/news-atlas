//! `title` — the headline without the outlet's tail.
//!
//! Decides which separator ends a headline's suffix (` | `, ` – `, ` — `, ` - `,
//! ` :: `, ` · `), when the tail really is the outlet's name and when it is part
//! of the headline (`A - B: the fight` keeps its dash), and that the result is
//! trimmed.
//!
//! It does not fix a headline's case or wording, does not translate it, and does
//! not decide whether a title is clickbait: it removes a suffix and stops.

/// Drop the trailing " | Publisher", " - Site", " — Outlet" a headline carries.
pub fn clean_title(input: &str, publisher: &str) -> String {
    let mut title = input.trim().to_string();
    let separators = [" | ", " – ", " — ", " - ", " :: ", " · "];
    let pub_low = publisher.trim().to_lowercase();
    let mut changed = true;
    while changed {
        changed = false;
        for sep in separators {
            if let Some(pos) = title.rfind(sep) {
                let (head, tail) = title.split_at(pos);
                if !head.trim().is_empty() && tail.trim().chars().count() <= 40 {
                    let tail_clean = tail.trim_start_matches(sep).trim().to_lowercase();
                    let words: Vec<&str> = tail_clean.split_whitespace().collect();
                    let looks_like_outlet = !words.is_empty()
                        && words.len() <= 3
                        && !tail_clean.contains(':')
                        && !tail_clean.contains('?')
                        && words.iter().all(|w| w.chars().count() <= 12);
                    let drop = tail_clean.is_empty()
                        || (!pub_low.is_empty() && tail_clean == pub_low)
                        || looks_like_outlet;
                    if drop {
                        title = head.trim().to_string();
                        changed = true;
                        break;
                    }
                }
            }
        }
    }
    title
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn title_loses_the_outlet_suffix() {
        assert_eq!(clean_title("Chips halt | Reuters", "reuters"), "Chips halt");
        assert_eq!(clean_title("Chips halt - The Verge", ""), "Chips halt");
        assert_eq!(clean_title("A - B: the fight", ""), "A - B: the fight");
    }
}
