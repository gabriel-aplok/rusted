use std::fmt;

const MAX_DEPTH: usize = 128;

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Value>),
    Object(Vec<(String, Value)>),
}

#[cfg_attr(not(test), allow(dead_code))]
impl Value {
    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Object(fields) => fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value),
            _ => None,
        }
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub(crate) fn as_u64(&self) -> Option<u64> {
        match self {
            Value::Number(value)
                if value.is_finite()
                    && *value >= 0.0
                    && value.fract() == 0.0
                    && *value <= u64::MAX as f64 =>
            {
                Some(*value as u64)
            }
            _ => None,
        }
    }

    pub(crate) fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(values) => Some(values),
            _ => None,
        }
    }

    pub(crate) fn as_object(&self) -> Option<&[(String, Value)]> {
        match self {
            Value::Object(fields) => Some(fields),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Error {
    pub(crate) offset: usize,
    pub(crate) reason: &'static str,
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at byte {}", self.reason, self.offset)
    }
}

impl std::error::Error for Error {}

pub(crate) fn escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if (character as u32) < 0x20 => {
                escaped.push_str(&format!("\\u{:04x}", character as u32));
            }
            character => escaped.push(character),
        }
    }
    escaped
}

pub(crate) fn parse(input: &str) -> Result<Value, Error> {
    let mut parser = Parser {
        input: input.as_bytes(),
        pos: 0,
        depth: 0,
    };
    parser.skip_whitespace();
    let value = parser.parse_value()?;
    parser.skip_whitespace();
    if parser.pos != parser.input.len() {
        return Err(parser.error("trailing characters"));
    }
    Ok(value)
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn parse_lines(input: &str) -> Result<Vec<Value>, Error> {
    let mut values = Vec::new();
    let mut base = 0;

    for line in input.split_inclusive('\n') {
        if !line.trim().is_empty() {
            match parse(line) {
                Ok(value) => values.push(value),
                Err(mut error) => {
                    error.offset += base;
                    return Err(error);
                }
            }
        }
        base += line.len();
    }

    Ok(values)
}

struct Parser<'a> {
    input: &'a [u8],
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn parse_value(&mut self) -> Result<Value, Error> {
        self.skip_whitespace();
        match self.peek() {
            None => Err(self.error("unexpected end of input")),
            Some(b'n') => self.literal(b"null", Value::Null),
            Some(b't') => self.literal(b"true", Value::Bool(true)),
            Some(b'f') => self.literal(b"false", Value::Bool(false)),
            Some(b'"') => {
                self.pos += 1;
                Ok(Value::String(self.parse_string()?))
            }
            Some(b'[') => self.parse_array(),
            Some(b'{') => self.parse_object(),
            Some(b'-' | b'0'..=b'9') => self.parse_number(),
            Some(_) => Err(self.error("expected value")),
        }
    }

    fn literal(&mut self, text: &[u8], value: Value) -> Result<Value, Error> {
        if self.input[self.pos..].starts_with(text) {
            self.pos += text.len();
            Ok(value)
        } else {
            Err(self.error("expected value"))
        }
    }

    fn parse_array(&mut self) -> Result<Value, Error> {
        self.enter()?;
        self.pos += 1;
        self.skip_whitespace();

        let mut values = Vec::new();
        if self.peek() == Some(b']') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Value::Array(values));
        }

        loop {
            values.push(self.parse_value()?);
            self.skip_whitespace();
            match self.next() {
                Some(b',') => self.skip_whitespace(),
                Some(b']') => break,
                Some(_) => return Err(self.error("expected comma or closing bracket")),
                None => return Err(self.error("unterminated array")),
            }
        }

        self.depth -= 1;
        Ok(Value::Array(values))
    }

    fn parse_object(&mut self) -> Result<Value, Error> {
        self.enter()?;
        self.pos += 1;
        self.skip_whitespace();

        let mut fields = Vec::new();
        if self.peek() == Some(b'}') {
            self.pos += 1;
            self.depth -= 1;
            return Ok(Value::Object(fields));
        }

        loop {
            self.skip_whitespace();
            if self.next() != Some(b'"') {
                return Err(self.error("expected string key"));
            }
            let key = self.parse_string()?;

            self.skip_whitespace();
            if self.next() != Some(b':') {
                return Err(self.error("expected colon"));
            }

            let value = self.parse_value()?;
            fields.push((key, value));

            self.skip_whitespace();
            match self.next() {
                Some(b',') => self.skip_whitespace(),
                Some(b'}') => break,
                Some(_) => return Err(self.error("expected comma or closing brace")),
                None => return Err(self.error("unterminated object")),
            }
        }

        self.depth -= 1;
        Ok(Value::Object(fields))
    }

    fn parse_string(&mut self) -> Result<String, Error> {
        let mut out = Vec::new();

        loop {
            let Some(byte) = self.next() else {
                return Err(self.error("unterminated string"));
            };

            match byte {
                b'"' => {
                    return String::from_utf8(out).map_err(|_| self.error("invalid utf-8"));
                }
                b'\\' => {
                    let Some(escape) = self.next() else {
                        return Err(self.error("unterminated string"));
                    };
                    match escape {
                        b'"' => out.push(b'"'),
                        b'\\' => out.push(b'\\'),
                        b'/' => out.push(b'/'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'u' => {
                            let code = self.parse_unicode_escape()?;
                            let character = char::from_u32(code)
                                .ok_or_else(|| self.error("invalid unicode escape"))?;
                            let mut buffer = [0u8; 4];
                            out.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
                        }
                        _ => return Err(self.error("invalid escape")),
                    }
                }
                0x00..=0x1f => return Err(self.error("control character in string")),
                _ => out.push(byte),
            }
        }
    }

    fn parse_unicode_escape(&mut self) -> Result<u32, Error> {
        let first = self.parse_hex4()?;

        if (0xd800..=0xdbff).contains(&first) {
            if self.next() != Some(b'\\') || self.next() != Some(b'u') {
                return Err(self.error("missing low surrogate"));
            }
            let second = self.parse_hex4()?;
            if !(0xdc00..=0xdfff).contains(&second) {
                return Err(self.error("invalid low surrogate"));
            }
            Ok(0x10000 + ((first - 0xd800) << 10) + (second - 0xdc00))
        } else if (0xdc00..=0xdfff).contains(&first) {
            Err(self.error("unexpected low surrogate"))
        } else {
            Ok(first)
        }
    }

    fn parse_hex4(&mut self) -> Result<u32, Error> {
        let mut value = 0;

        for _ in 0..4 {
            let Some(byte) = self.next() else {
                return Err(self.error("unterminated unicode escape"));
            };
            let digit = match byte {
                b'0'..=b'9' => (byte - b'0') as u32,
                b'a'..=b'f' => (byte - b'a' + 10) as u32,
                b'A'..=b'F' => (byte - b'A' + 10) as u32,
                _ => return Err(self.error("invalid unicode escape")),
            };
            value = (value << 4) | digit;
        }

        Ok(value)
    }

    fn parse_number(&mut self) -> Result<Value, Error> {
        let start = self.pos;

        if self.peek() == Some(b'-') {
            self.pos += 1;
        }

        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                self.pos += 1;
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(self.error("invalid number")),
        }

        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("invalid number"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }

        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.error("invalid number"));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }

        let text = std::str::from_utf8(&self.input[start..self.pos])
            .map_err(|_| self.error("invalid number"))?;
        let number: f64 = text.parse().map_err(|_| self.error("invalid number"))?;
        if !number.is_finite() {
            return Err(self.error("number out of range"));
        }
        Ok(Value::Number(number))
    }

    fn enter(&mut self) -> Result<(), Error> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error("nesting too deep"));
        }
        Ok(())
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\n' | b'\r')) {
            self.pos += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.input.get(self.pos).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.pos += 1;
        Some(byte)
    }

    fn error(&self, reason: &'static str) -> Error {
        Error {
            offset: self.pos,
            reason,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{parse, parse_lines, Error, Value};

    fn error(input: &str) -> Error {
        parse(input).unwrap_err()
    }

    #[test]
    fn parses_nested_values() {
        let value = parse(r#"{"a":1,"b":[true,null,"x"],"c":{"d":-2.5e1}}"#).unwrap();

        assert_eq!(value.get("a").and_then(Value::as_u64), Some(1));
        assert_eq!(
            value
                .get("b")
                .and_then(Value::as_array)
                .map(|items| items.len()),
            Some(3)
        );
        assert_eq!(
            value
                .get("b")
                .and_then(Value::as_array)
                .and_then(|items| items[0].as_bool()),
            Some(true)
        );
        assert_eq!(
            value
                .get("b")
                .and_then(Value::as_array)
                .and_then(|items| items.get(1)),
            Some(&Value::Null)
        );
        assert_eq!(
            value.get("c").and_then(|value| value.get("d")),
            Some(&Value::Number(-25.0))
        );

        let fields = value.as_object().unwrap();
        assert_eq!(fields[0].0, "a");
        assert_eq!(fields[1].0, "b");
    }

    #[test]
    fn parses_string_escapes_and_unicode() {
        let value = parse(r#""a\nb\t\"c\\d\/e\u00e9\uD83D\uDE00""#).unwrap();
        assert_eq!(value.as_str(), Some("a\nb\t\"c\\d/eé😀"));
    }

    #[test]
    fn parses_numbers() {
        assert_eq!(parse("0").unwrap().as_u64(), Some(0));
        assert_eq!(parse("-0").unwrap().as_u64(), Some(0));
        assert_eq!(parse("1.5").unwrap(), Value::Number(1.5));
        assert_eq!(parse("1e3").unwrap(), Value::Number(1000.0));
        assert_eq!(parse("-2E-2").unwrap(), Value::Number(-0.02));
    }

    #[test]
    fn rejects_malformed_input() {
        assert_eq!(error("").reason, "unexpected end of input");
        assert_eq!(error(r#"{"a":1"#).reason, "unterminated object");
        assert_eq!(error(r#"{"a"}"#).reason, "expected colon");
        assert_eq!(error("[1,]").reason, "expected value");
        assert_eq!(error(r#""\q""#).reason, "invalid escape");
        assert_eq!(error(r#""\uD800""#).reason, "missing low surrogate");
        assert_eq!(error("01").reason, "trailing characters");
        assert_eq!(error(&"[".repeat(200)).reason, "nesting too deep");
    }

    #[test]
    fn parses_newline_delimited_objects() {
        let values = parse_lines("{\"a\":1}\n\n{\"b\":2}\n").unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values[1].get("b").and_then(Value::as_u64), Some(2));
    }

    #[test]
    fn reports_absolute_offset_for_line_errors() {
        let error = parse_lines("{\"a\":1}\n{\"b\"}\n").unwrap_err();
        assert!(error.offset > 8);
    }

    #[test]
    fn escapes_strings() {
        assert_eq!(super::escape("a\"b\\c\n"), "a\\\"b\\\\c\\n");
    }

    #[test]
    fn missing_fields_return_none() {
        assert_eq!(Value::Null.get("a"), None);
        assert_eq!(parse("true").unwrap().as_str(), None);
    }

    #[test]
    fn never_panics_on_malformed_input() {
        for input in [
            "",
            "  ",
            "{\"a\":}",
            "[]]",
            "{\"a\":1,}",
            "\"\\u00\"",
            "\"ab",
            "1.5.5",
            "01",
            "+1",
            "Infinity",
            "-Infinity",
            "-",
            "-.",
            "1e",
            "1e+",
            "00",
            "{\"a\" 1}",
            "[1,,2]",
            "\"\\uD800\"",
            "\"\\uDC00\"",
            "{\"\":\"\"}",
            "01.5",
            "1e999",
            "-1e999",
        ] {
            let _ = parse(input);
        }
    }
}
