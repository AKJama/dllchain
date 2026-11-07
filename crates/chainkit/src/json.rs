//! Just enough JSON for what a mod's zip carries: the text fields at the top of an object (a `mod.json`'s name and
//! version, a `SHA256.json`'s file list). Values that are not text (numbers, lists, objects) are read past.

/// The fields of the object `text` holds whose values are text, in order; `None` when it is not one JSON object.
pub fn text_fields(text: &str) -> Option<Vec<(String, String)>> {
    let mut p = Parser { s: text.trim_start_matches('\u{feff}').as_bytes(), i: 0 };
    p.ws();
    p.eat(b'{')?;
    let mut out = Vec::new();
    p.ws();
    if p.peek() == Some(b'}') {
        p.i += 1;
    } else {
        loop {
            p.ws();
            let key = p.string()?;
            p.ws();
            p.eat(b':')?;
            p.ws();
            if p.peek() == Some(b'"') {
                out.push((key, p.string()?));
            } else {
                p.skip_value(0)?;
            }
            p.ws();
            match p.next()? {
                b',' => continue,
                b'}' => break,
                _ => return None,
            }
        }
    }
    p.ws();
    (p.i == p.s.len()).then_some(out)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        Some(c)
    }

    fn eat(&mut self, c: u8) -> Option<()> {
        (self.next()? == c).then_some(())
    }

    fn ws(&mut self) {
        while self.peek().is_some_and(|c| c.is_ascii_whitespace()) {
            self.i += 1;
        }
    }

    fn string(&mut self) -> Option<String> {
        self.eat(b'"')?;
        let mut units: Vec<u16> = Vec::new();
        let mut out = String::new();
        let flush = |units: &mut Vec<u16>, out: &mut String| {
            out.push_str(&String::from_utf16_lossy(units));
            units.clear();
        };
        loop {
            let start = self.i;
            while self.peek().is_some_and(|c| c != b'"' && c != b'\\' && c >= 0x20) {
                self.i += 1;
            }
            if self.i > start {
                flush(&mut units, &mut out);
                out.push_str(std::str::from_utf8(&self.s[start..self.i]).ok()?);
            }
            match self.next()? {
                b'"' => break,
                b'\\' => {
                    let c = match self.next()? {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let hex = std::str::from_utf8(self.s.get(self.i..self.i + 4)?).ok()?;
                            self.i += 4;
                            units.push(u16::from_str_radix(hex, 16).ok()?);
                            continue;
                        }
                        _ => return None,
                    };
                    flush(&mut units, &mut out);
                    out.push(c);
                }
                _ => return None, // a control character
            }
        }
        flush(&mut units, &mut out);
        Some(out)
    }

    /// Reads past one value of any kind; `depth` keeps a hostile file from nesting without end.
    fn skip_value(&mut self, depth: usize) -> Option<()> {
        if depth > 32 {
            return None;
        }
        match self.peek()? {
            b'"' => self.string().map(|_| ()),
            open @ (b'{' | b'[') => {
                let close = if open == b'{' { b'}' } else { b']' };
                self.i += 1;
                self.ws();
                if self.peek() == Some(close) {
                    self.i += 1;
                    return Some(());
                }
                loop {
                    self.ws();
                    if open == b'{' {
                        self.string()?;
                        self.ws();
                        self.eat(b':')?;
                        self.ws();
                    }
                    self.skip_value(depth + 1)?;
                    self.ws();
                    match self.next()? {
                        b',' => continue,
                        c if c == close => return Some(()),
                        _ => return None,
                    }
                }
            }
            _ => {
                // a number, true, false or null
                let start = self.i;
                while self.peek().is_some_and(|c| c.is_ascii_alphanumeric() || b"+-.".contains(&c)) {
                    self.i += 1;
                }
                (self.i > start).then_some(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_text_fields() {
        let t = "\u{feff}{\n \"id\": \"town-portal\", \"api\": 2, \"enabled\": true, \"tags\": [\"a\", {\"b\": null}],\n \
                 \"name\": \"Town \\\"Portals\\\" \\u00e9\\ud83d\\ude00\" }\n";
        assert_eq!(
            text_fields(t),
            Some(vec![("id".into(), "town-portal".into()), ("name".into(), "Town \"Portals\" \u{e9}\u{1f600}".into())])
        );
        assert_eq!(text_fields("{}"), Some(Vec::new()));
        assert_eq!(text_fields(r#"{"client/a.dll": "ab"}"#), Some(vec![("client/a.dll".into(), "ab".into())]));
    }

    #[test]
    fn refuses_what_is_not_one_object() {
        for bad in
            ["", "[]", "{", r#"{"a" "b"}"#, r#"{"a": "b",}"#, r#"{"a": "b"} x"#, "{\"a\": \"\u{1}\"}", r#"{"a":}"#]
        {
            assert_eq!(text_fields(bad), None, "{bad:?}");
        }
        let deep = format!("{{\"a\": {}{}}}", "[".repeat(100), "]".repeat(100));
        assert_eq!(text_fields(&deep), None);
    }
}
