//! The compiled-in gazetteer, split by what each table is for.
//!
//! Nothing here is logic: every file below is a `static` slice, copied from the
//! Python tables and `data/countries.json` so the port decides on exactly the
//! same names, coordinates and weights — plus `NAMESAKES`, which the port adds:
//! the second city of a name two countries share. The index is built from these
//! tables once per process (`crate::index`); no other module reads them directly.
//!
//! The tables are re-exported flat, so the index code spells them
//! `data::CITIES` and never has to know which file a table lives in.

mod aliases;
mod cities;
mod countries;
mod namesakes;
mod outlets;

pub use aliases::{
    ADJECTIVES, AMBIGUOUS, COUNTRY_ALIASES, GOV_METONYMS, GUARDS, NON_LATIN, PRE_GUARDS,
    REGION_ALIASES,
};
pub use cities::CITIES;
pub use countries::{CAPITAL_CITY, COUNTRIES, ISO_NAMES, SHORT_NAMES};
pub use namesakes::NAMESAKES;
pub use outlets::{OUTLET_COUNTRY, OUTLET_NAME_COUNTRY};
