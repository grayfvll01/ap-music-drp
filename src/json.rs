//! Just enough JSON: a string escaper for building payloads and a small
//! parser for reading Discord / iTunes responses. Avoids pulling in serde.

use crate::prelude::*;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Num(i64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Obj(kv) => kv.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }
    pub fn str(&self, key: &str) -> Option<&str> {
        match self.get(key) {
            Some(Json::Str(s)) => Some(s),
            _ => None,
        }
    }
    pub fn num(&self, key: &str) -> Option<i64> {
        match self.get(key) {
            Some(Json::Num(n)) => Some(*n),
            _ => None,
        }
    }
    pub fn arr(&self, key: &str) -> &[Json] {
        match self.get(key) {
            Some(Json::Arr(a)) => a,
            _ => &[],
        }
    }
}

/// Appends `s` to `out` as a quoted JSON string.
pub fn push_str(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                const HEX: &[u8; 16] = b"0123456789abcdef";
                out.push_str("\\u00");
                out.push(HEX[(c as usize) >> 4] as char);
                out.push(HEX[(c as usize) & 15] as char);
            }
            c => out.push(c),
        }
    }
    out.push('"');
}

pub fn parse(s: &str) -> Option<Json> {
    let mut p = Parser { b: s.as_bytes(), i: 0, depth: 0 };
    let v = p.value()?;
    p.ws();
    (p.i == p.b.len()).then_some(v)
}

struct Parser<'a> {
    b: &'a [u8],
    i: usize,
    depth: u32,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.i < self.b.len() && matches!(self.b[self.i], b' ' | b'\t' | b'\n' | b'\r') {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        self.ws();
        (self.b.get(self.i) == Some(&c)).then(|| self.i += 1)
    }

    fn lit(&mut self, word: &[u8], v: Json) -> Option<Json> {
        self.b[self.i..].starts_with(word).then(|| {
            self.i += word.len();
            v
        })
    }

    fn value(&mut self) -> Option<Json> {
        self.ws();
        match *self.b.get(self.i)? {
            b'{' => self.nested(Self::object),
            b'[' => self.nested(Self::array),
            b'"' => self.string().map(Json::Str),
            b't' => self.lit(b"true", Json::Bool(true)),
            b'f' => self.lit(b"false", Json::Bool(false)),
            b'n' => self.lit(b"null", Json::Null),
            _ => self.number(),
        }
    }

    fn nested(&mut self, f: fn(&mut Self) -> Option<Json>) -> Option<Json> {
        self.depth += 1;
        if self.depth > 64 {
            return None;
        }
        let v = f(self);
        self.depth -= 1;
        v
    }

    fn object(&mut self) -> Option<Json> {
        self.i += 1;
        let mut kv = Vec::new();
        if self.eat(b'}').is_some() {
            return Some(Json::Obj(kv));
        }
        loop {
            self.ws();
            let k = self.string()?;
            self.eat(b':')?;
            kv.push((k, self.value()?));
            if self.eat(b',').is_none() {
                self.eat(b'}')?;
                return Some(Json::Obj(kv));
            }
        }
    }

    fn array(&mut self) -> Option<Json> {
        self.i += 1;
        let mut a = Vec::new();
        if self.eat(b']').is_some() {
            return Some(Json::Arr(a));
        }
        loop {
            a.push(self.value()?);
            if self.eat(b',').is_none() {
                self.eat(b']')?;
                return Some(Json::Arr(a));
            }
        }
    }

    fn hex4(&mut self) -> Option<u32> {
        let h = core::str::from_utf8(self.b.get(self.i..self.i + 4)?).ok()?;
        self.i += 4;
        u32::from_str_radix(h, 16).ok()
    }

    fn string(&mut self) -> Option<String> {
        if self.b.get(self.i) != Some(&b'"') {
            return None;
        }
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let c = *self.b.get(self.i)?;
            self.i += 1;
            match c {
                b'"' => return String::from_utf8(out).ok(),
                b'\\' => {
                    let e = *self.b.get(self.i)?;
                    self.i += 1;
                    let ch = match e {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let mut cp = self.hex4()?;
                            if (0xD800..0xDC00).contains(&cp) && self.b[self.i..].starts_with(b"\\u") {
                                self.i += 2;
                                let lo = self.hex4()?;
                                cp = 0x10000 + ((cp - 0xD800) << 10) + (lo.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            char::from_u32(cp).unwrap_or('\u{FFFD}')
                        }
                        _ => return None,
                    };
                    let mut buf = [0u8; 4];
                    out.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
                }
                c => out.push(c),
            }
        }
    }

    /// Integers only (ids, codes, timestamps); a fraction or exponent is
    /// consumed but dropped. Skipping float parsing keeps the exe smaller.
    fn number(&mut self) -> Option<Json> {
        let neg = self.b.get(self.i) == Some(&b'-');
        if neg {
            self.i += 1;
        }
        let digits = self.i;
        let mut n: i64 = 0;
        while let Some(d @ b'0'..=b'9') = self.b.get(self.i).copied() {
            n = n.saturating_mul(10).saturating_add((d - b'0') as i64);
            self.i += 1;
        }
        if self.i == digits {
            return None;
        }
        while self.i < self.b.len() && matches!(self.b[self.i], b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9') {
            self.i += 1;
        }
        Some(Json::Num(if neg { -n } else { n }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_lookup() {
        let v = parse(r#"{"a":[1,2.5,-3e2,12345678901234],"b":{"c":"x\"yé🎵"},"d":null,"e":true}"#).unwrap();
        assert_eq!(v.arr("a"), &[Json::Num(1), Json::Num(2), Json::Num(-3), Json::Num(12345678901234)]);
        assert_eq!(v.get("b").unwrap().str("c"), Some("x\"yé🎵"));
        assert_eq!(v.get("d"), Some(&Json::Null));
        let mut s = String::new();
        push_str(&mut s, "a\"b\\c\n\u{1}");
        assert_eq!(s, r#""a\"b\\c\n\u0001""#);
        assert!(parse("{\"a\":}").is_none());
        assert!(parse(&"[".repeat(100)).is_none());
    }
}
