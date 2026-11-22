//! `json` — a minimal JSON parser and writer, just enough for one request.
//!
//! Decides what a JSON value is (`Json`), how one is parsed, how one is written
//! back out, and the two number shapes an answer uses (`num_str` for integers,
//! `fmt_score` for a cosine score).
//!
//! It does not decide what a field means or whether a request is complete: each
//! subcommand reads its own fields and says what is missing.

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(items) => items.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Json::Str(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Json::Num(n) if n.is_finite() => Some(*n),
            _ => None,
        }
    }

    pub fn as_usize(&self) -> Option<usize> {
        match self {
            Json::Num(n) => {
                if n.is_finite() && *n >= 0.0 && *n <= (usize::MAX as f64) {
                    Some(*n as usize)
                } else {
                    None
                }
            }
            _ => None,
        }
    }

    pub fn as_arr(&self) -> Option<&Vec<Json>> {
        match self {
            Json::Arr(items) => Some(items),
            _ => None,
        }
    }
}

pub fn parse(src: &str) -> Result<Json, String> {
    let mut p = Parser {
        s: src,
        b: src.as_bytes(),
        i: 0,
        depth: 0,
    };
    p.ws();
    let value = p.value()?;
    p.ws();
    if p.i != p.b.len() {
        return Err(format!("trailing characters at byte {}", p.i));
    }
    Ok(value)
}

struct Parser<'a> {
    s: &'a str,
    b: &'a [u8],
    i: usize,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn ws(&mut self) {
        while self.i < self.b.len() {
            match self.b[self.i] {
                b' ' | b'\t' | b'\n' | b'\r' => self.i += 1,
                _ => break,
            }
        }
    }

    fn peek(&self) -> Option<u8> {
        if self.i < self.b.len() {
            Some(self.b[self.i])
        } else {
            None
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.depth += 1;
        if self.depth > 128 {
            return Err("json nesting too deep".to_string());
        }
        let out = self.value_inner();
        self.depth -= 1;
        out
    }

    fn value_inner(&mut self) -> Result<Json, String> {
        match self.peek() {
            None => Err("unexpected end of json input".to_string()),
            Some(b'{') => self.object(),
            Some(b'[') => self.array(),
            Some(b'"') => Ok(Json::Str(self.string()?)),
            Some(b't') => {
                self.literal("true")?;
                Ok(Json::Bool(true))
            }
            Some(b'f') => {
                self.literal("false")?;
                Ok(Json::Bool(false))
            }
            Some(b'n') => {
                self.literal("null")?;
                Ok(Json::Null)
            }
            Some(c) if c == b'-' || c.is_ascii_digit() => self.number(),
            Some(c) => Err(format!("unexpected byte {:?} at byte {}", c as char, self.i)),
        }
    }

    fn literal(&mut self, word: &str) -> Result<(), String> {
        match self.s.get(self.i..) {
            Some(rest) if rest.starts_with(word) => {
                self.i += word.len();
                Ok(())
            }
            _ => Err(format!("invalid literal at byte {}", self.i)),
        }
    }

    fn object(&mut self) -> Result<Json, String> {
        self.i += 1; // consume '{'
        let mut items: Vec<(String, Json)> = Vec::new();
        self.ws();
        if self.peek() == Some(b'}') {
            self.i += 1;
            return Ok(Json::Obj(items));
        }
        loop {
            self.ws();
            if self.peek() != Some(b'"') {
                return Err(format!("expected object key at byte {}", self.i));
            }
            let key = self.string()?;
            self.ws();
            if self.peek() != Some(b':') {
                return Err(format!("expected ':' at byte {}", self.i));
            }
            self.i += 1;
            self.ws();
            let value = self.value()?;
            items.push((key, value));
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b'}') => {
                    self.i += 1;
                    return Ok(Json::Obj(items));
                }
                _ => return Err(format!("expected ',' or '}}' at byte {}", self.i)),
            }
        }
    }

    fn array(&mut self) -> Result<Json, String> {
        self.i += 1; // consume '['
        let mut items: Vec<Json> = Vec::new();
        self.ws();
        if self.peek() == Some(b']') {
            self.i += 1;
            return Ok(Json::Arr(items));
        }
        loop {
            self.ws();
            let value = self.value()?;
            items.push(value);
            self.ws();
            match self.peek() {
                Some(b',') => {
                    self.i += 1;
                }
                Some(b']') => {
                    self.i += 1;
                    return Ok(Json::Arr(items));
                }
                _ => return Err(format!("expected ',' or ']' at byte {}", self.i)),
            }
        }
    }

    fn string(&mut self) -> Result<String, String> {
        self.i += 1; // consume opening quote
        let mut out = String::new();
        let mut seg = self.i;
        loop {
            if self.i >= self.b.len() {
                return Err("unterminated string".to_string());
            }
            let c = self.b[self.i];
            if c == b'"' {
                match self.s.get(seg..self.i) {
                    Some(part) => out.push_str(part),
                    None => return Err("invalid utf-8 in string".to_string()),
                }
                self.i += 1;
                return Ok(out);
            }
            if c != b'\\' {
                self.i += 1;
                continue;
            }
            match self.s.get(seg..self.i) {
                Some(part) => out.push_str(part),
                None => return Err("invalid utf-8 in string".to_string()),
            }
            self.i += 1;
            let esc = match self.peek() {
                Some(e) => e,
                None => return Err("unterminated escape".to_string()),
            };
            self.i += 1;
            match esc {
                b'"' => out.push('"'),
                b'\\' => out.push('\\'),
                b'/' => out.push('/'),
                b'b' => out.push('\u{8}'),
                b'f' => out.push('\u{c}'),
                b'n' => out.push('\n'),
                b'r' => out.push('\r'),
                b't' => out.push('\t'),
                b'u' => {
                    let hi = self.hex4()?;
                    let ch = if (0xD800..0xDC00).contains(&hi) {
                        if self.b.get(self.i) == Some(&b'\\') && self.b.get(self.i + 1) == Some(&b'u') {
                            self.i += 2;
                            let lo = self.hex4()?;
                            if (0xDC00..0xE000).contains(&lo) {
                                let combined = 0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00);
                                char::from_u32(combined).unwrap_or('\u{FFFD}')
                            } else {
                                '\u{FFFD}'
                            }
                        } else {
                            '\u{FFFD}'
                        }
                    } else if (0xDC00..0xE000).contains(&hi) {
                        '\u{FFFD}'
                    } else {
                        char::from_u32(hi).unwrap_or('\u{FFFD}')
                    };
                    out.push(ch);
                }
                _ => return Err(format!("invalid escape at byte {}", self.i)),
            }
            seg = self.i;
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        if self.i + 4 > self.b.len() {
            return Err("truncated \\u escape".to_string());
        }
        let mut value: u32 = 0;
        for k in 0..4 {
            let c = self.b[self.i + k];
            let digit = match c {
                b'0'..=b'9' => (c - b'0') as u32,
                b'a'..=b'f' => (c - b'a') as u32 + 10,
                b'A'..=b'F' => (c - b'A') as u32 + 10,
                _ => return Err("invalid hex digit in \\u escape".to_string()),
            };
            value = value * 16 + digit;
        }
        self.i += 4;
        Ok(value)
    }

    fn number(&mut self) -> Result<Json, String> {
        let start = self.i;
        if self.peek() == Some(b'-') {
            self.i += 1;
        }
        while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
            self.i += 1;
        }
        if self.peek() == Some(b'.') {
            self.i += 1;
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        if matches!(self.peek(), Some(b'e') | Some(b'E')) {
            self.i += 1;
            if matches!(self.peek(), Some(b'+') | Some(b'-')) {
                self.i += 1;
            }
            while matches!(self.peek(), Some(c) if c.is_ascii_digit()) {
                self.i += 1;
            }
        }
        let text = match self.s.get(start..self.i) {
            Some(t) => t,
            None => return Err(format!("invalid number at byte {}", start)),
        };
        match text.parse::<f64>() {
            Ok(v) if v.is_finite() => Ok(Json::Num(v)),
            _ => Err(format!("invalid number at byte {}", start)),
        }
    }
}

/// Append `s` to `out` as a quoted, escaped JSON string.
pub fn escape_str(out: &mut String, s: &str) {
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                let code = c as u32;
                out.push_str("\\u");
                for shift in [12u32, 8, 4, 0] {
                    let digit = (code >> shift) & 0xf;
                    let letter = if digit < 10 {
                        (b'0' + digit as u8) as char
                    } else {
                        (b'a' + (digit as u8 - 10)) as char
                    };
                    out.push(letter);
                }
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn write(out: &mut String, value: &Json) {
    match value {
        Json::Null => out.push_str("null"),
        Json::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
        Json::Num(n) => out.push_str(&num_str(*n)),
        Json::Str(s) => escape_str(out, s),
        Json::Arr(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write(out, item);
            }
            out.push(']');
        }
        Json::Obj(items) => {
            out.push('{');
            for (i, (k, v)) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                escape_str(out, k);
                out.push(':');
                write(out, v);
            }
            out.push('}');
        }
    }
}

pub fn to_string(value: &Json) -> String {
    let mut out = String::new();
    write(&mut out, value);
    out
}

/// Integral numbers are printed without a fractional part.
pub fn num_str(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    if v == v.trunc() && v.abs() < 1e15 {
        format!("{}", v as i64)
    } else {
        format!("{}", v)
    }
}

/// Fixed-point with trailing zeros trimmed, e.g. `0.61`, `1`, `0`.
pub fn fmt_score(v: f64) -> String {
    if !v.is_finite() {
        return "0".to_string();
    }
    let raw = format!("{:.4}", v);
    let trimmed = raw.trim_end_matches('0');
    let trimmed = trimmed.trim_end_matches('.');
    if trimmed.is_empty() || trimmed == "-" {
        "0".to_string()
    } else {
        trimmed.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn json_roundtrip_and_bad_input() {
        let root =
            parse("{\"a\":[1,2.5,true,null,\"x\\u00e9\"],\"b\":{\"c\":-3}}").expect("json");
        let text = to_string(&root);
        let again = parse(&text).expect("reparse");
        assert_eq!(root, again);
        assert!(parse("{").is_err());
        assert!(parse("[]").is_ok());
        assert!(parse("").is_err());
    }
}
