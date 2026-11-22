//! How a country is spelled in an answer.
//!
//! Decides: the short, readable label for a long official name
//! ("United States of America" -> "United States"), the fallback label for an
//! ISO code with no geodata record, and the one binary search the sorted
//! key->value tables are read with.
//!
//! Does not decide: which country won, or where it is. That is `crate::score`.

use crate::data;

/// Look `key` up in one of the sorted `(key, value)` tables.
pub(crate) fn lookup_pair(
    table: &'static [(&'static str, &'static str)],
    key: &str,
) -> Option<&'static str> {
    table
        .binary_search_by(|(k, _)| (*k).cmp(key))
        .ok()
        .map(|i| table[i].1)
}

/// Short, readable label for a long official country name.
///
/// Only `SHORT_NAMES` is consulted: [`iso_name`]'s table is keyed by ISO code, and
/// a country's *name* is never an ISO code, so looking there could only ever
/// return the name it was given.
pub(crate) fn short_country(name: &'static str) -> &'static str {
    lookup_pair(data::SHORT_NAMES, name).unwrap_or(name)
}

/// The label for an ISO code, when no geodata record exists for it.
pub(crate) fn iso_name(iso: &'static str) -> &'static str {
    match lookup_pair(data::ISO_NAMES, iso) {
        Some(short) => short,
        None => iso,
    }
}
