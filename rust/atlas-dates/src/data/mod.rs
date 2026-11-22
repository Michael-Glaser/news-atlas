//! Static tables: the words a timestamp can be written with. Data only, no
//! logic — the reader that decides *how* a word is matched is [`crate::phrases`].
//!
//! Every table entry is lowercase and carries the number it means, so a lookup
//! is a `eq_ignore_ascii_case`/`==` compare and never a second rule.
//!
//! These are *language* tables, not news-word lists: they say what "März" or
//! "sábado" means, and nothing about what counts as news, who publishes it or
//! which entity a name is. Reference data of that shape is what the project
//! allows (the same way the Natural Earth geodata is data); a table of
//! publishers, hosts or people would not be.
pub(crate) mod months;
pub(crate) mod weekdays;
