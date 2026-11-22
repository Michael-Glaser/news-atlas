//! Month names in the languages real publishers write dates in: English,
//! German (including the Austrian `Jänner`), French and Spanish, full and
//! abbreviated, with the month number each one means.
//!
//! Two entries may point at the same month from different languages (`mar` is
//! both English "March" and Spanish "marzo"); one entry may never point at two
//! different months, and [`super::super::phrases::month_from_name`] refuses a
//! guess when two months would both answer (`ju` could be June or July).
//!
//! Spelling only: ASCII-folded spellings that a feed actually prints
//! (`Maerz`, `fevrier`, `aout`, `miercoles`) sit next to the accented ones, so a
//! stamp is read whichever way the publisher's CMS encoded it.

/// `(lowercase name, month 1..=12)`.
pub(crate) const MONTH_NAMES: &[(&str, u32)] = &[
    // -- January
    ("january", 1),
    ("jan", 1),
    ("januar", 1),
    ("jänner", 1),
    ("jaenner", 1),
    ("janv", 1),
    ("janvier", 1),
    ("enero", 1),
    ("ene", 1),
    // -- February
    ("february", 2),
    ("feb", 2),
    ("februar", 2),
    ("févr", 2),
    ("fevr", 2),
    ("février", 2),
    ("fevrier", 2),
    ("febrero", 2),
    // -- March
    ("march", 3),
    ("mar", 3),
    ("märz", 3),
    ("maerz", 3),
    ("mär", 3),
    ("mrz", 3),
    ("mars", 3),
    ("marzo", 3),
    // -- April
    ("april", 4),
    ("apr", 4),
    ("avril", 4),
    ("avr", 4),
    ("abril", 4),
    ("abr", 4),
    // -- May
    ("may", 5),
    ("mai", 5),
    ("mayo", 5),
    // -- June
    ("june", 6),
    ("jun", 6),
    ("juni", 6),
    ("juin", 6),
    ("junio", 6),
    // -- July
    ("july", 7),
    ("jul", 7),
    ("juli", 7),
    ("juillet", 7),
    ("juil", 7),
    ("julio", 7),
    // -- August
    ("august", 8),
    ("aug", 8),
    ("août", 8),
    ("aout", 8),
    ("agosto", 8),
    ("ago", 8),
    // -- September
    ("september", 9),
    ("sept", 9),
    ("sep", 9),
    ("septembre", 9),
    ("septiembre", 9),
    ("setiembre", 9),
    // -- October
    ("october", 10),
    ("oct", 10),
    ("oktober", 10),
    ("okt", 10),
    ("octobre", 10),
    ("octubre", 10),
    // -- November
    ("november", 11),
    ("nov", 11),
    ("novembre", 11),
    ("noviembre", 11),
    // -- December
    ("december", 12),
    ("dec", 12),
    ("dezember", 12),
    ("dez", 12),
    ("décembre", 12),
    ("decembre", 12),
    ("diciembre", 12),
    ("dic", 12),
];

#[cfg(test)]
mod tests {
    use super::MONTH_NAMES;
    use std::collections::HashMap;

    #[test]
    fn every_month_is_reachable_under_one_name_at_least() {
        for month in 1..=12u32 {
            assert!(
                MONTH_NAMES.iter().any(|(_, m)| *m == month),
                "month {} has no name at all",
                month
            );
        }
        assert!(MONTH_NAMES.iter().all(|(_, m)| (1..=12).contains(m)));
    }

    #[test]
    fn no_name_means_two_months() {
        // A duplicated spelling with a different month would make the answer depend
        // on the table's order, which is exactly what the reader refuses to do.
        let mut seen: HashMap<String, u32> = HashMap::new();
        for (name, month) in MONTH_NAMES {
            assert_eq!(*name, name.to_lowercase(), "{:?} must be lowercase", name);
            assert!(
                !name.ends_with('.'),
                "{:?} must not carry a dot: the reader trims it",
                name
            );
            if let Some(previous) = seen.insert(name.to_string(), *month) {
                assert_eq!(previous, *month, "{:?} names two months", name);
            }
        }
    }
}
