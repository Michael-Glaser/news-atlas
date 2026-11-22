//! Weekday names, English / German / French / Spanish, full and abbreviated,
//! with 0 = Monday .. 6 = Sunday — the same numbering as
//! [`crate::calendar::weekday_idx`], so a written weekday can be compared with
//! the weekday the date arithmetic computes.
//!
//! A stamp's weekday is *checked*, never trusted (see
//! [`crate::parse::Stamp::weekday_ok`]): feeds get it wrong, and a wrong weekday
//! must cost a report, not the article.

/// `(lowercase name, 0 = Monday .. 6 = Sunday)`.
pub(crate) const WEEKDAY_NAMES: &[(&str, usize)] = &[
    // -- Monday
    ("monday", 0),
    ("mon", 0),
    ("montag", 0),
    ("mo", 0),
    ("lundi", 0),
    ("lun", 0),
    ("lunes", 0),
    // -- Tuesday
    ("tuesday", 1),
    ("tue", 1),
    ("tues", 1),
    ("dienstag", 1),
    ("di", 1),
    ("mardi", 1),
    ("mar", 1),
    ("martes", 1),
    // -- Wednesday
    ("wednesday", 2),
    ("wed", 2),
    ("mittwoch", 2),
    ("mi", 2),
    ("mercredi", 2),
    ("mer", 2),
    ("miércoles", 2),
    ("miercoles", 2),
    ("mié", 2),
    ("mie", 2),
    // -- Thursday
    ("thursday", 3),
    ("thu", 3),
    ("thurs", 3),
    ("donnerstag", 3),
    ("do", 3),
    ("jeudi", 3),
    ("jeu", 3),
    ("jueves", 3),
    ("jue", 3),
    // -- Friday
    ("friday", 4),
    ("fri", 4),
    ("freitag", 4),
    ("fr", 4),
    ("vendredi", 4),
    ("ven", 4),
    ("viernes", 4),
    ("vie", 4),
    // -- Saturday
    ("saturday", 5),
    ("sat", 5),
    ("samstag", 5),
    ("sa", 5),
    ("sonnabend", 5),
    ("samedi", 5),
    ("sam", 5),
    ("sábado", 5),
    ("sabado", 5),
    ("sáb", 5),
    ("sab", 5),
    // -- Sunday
    ("sunday", 6),
    ("sun", 6),
    ("sonntag", 6),
    ("so", 6),
    ("dimanche", 6),
    ("dim", 6),
    ("domingo", 6),
    ("dom", 6),
];

#[cfg(test)]
mod tests {
    use super::WEEKDAY_NAMES;

    #[test]
    fn every_weekday_is_reachable_and_none_conflicts() {
        for day in 0..7usize {
            assert!(
                WEEKDAY_NAMES.iter().any(|(_, d)| *d == day),
                "weekday {} has no name at all",
                day
            );
        }
        assert!(WEEKDAY_NAMES.iter().all(|(_, d)| *d < 7));
        let mut seen: std::collections::HashMap<&str, usize> = std::collections::HashMap::new();
        for (name, day) in WEEKDAY_NAMES {
            assert_eq!(*name, name.to_lowercase(), "{:?} must be lowercase", name);
            if let Some(previous) = seen.insert(name, *day) {
                assert_eq!(previous, *day, "{:?} names two weekdays", name);
            }
        }
        // Monday..Sunday is the calendar's numbering, which is what a written
        // weekday is compared against.
        assert_eq!(WEEKDAY_NAMES.iter().find(|(n, _)| *n == "saturday").unwrap().1, 5);
        assert_eq!(WEEKDAY_NAMES.iter().find(|(n, _)| *n == "domingo").unwrap().1, 6);
    }
}
