use super::{Error, Table, Value};

const MAX_DEPTH: usize = 128;

pub(crate) fn parse(input: &str) -> Result<Table, Error> {
    let input = input.strip_prefix('\u{feff}').unwrap_or(input);
    Parser {
        input,
        pos: 0,
        line: 1,
        column: 1,
        root: Table::new(),
        current: Vec::new(),
        depth: 0,
    }
    .parse_document()
}

#[derive(Clone, Copy)]
struct Position {
    line: usize,
    column: usize,
}

struct Parser<'a> {
    input: &'a str,
    pos: usize,
    line: usize,
    column: usize,
    root: Table,
    current: Vec<String>,
    depth: usize,
}

impl<'a> Parser<'a> {
    fn parse_document(mut self) -> Result<Table, Error> {
        loop {
            self.skip_inline_whitespace();
            if self.peek() == Some(b'#') {
                self.skip_comment();
            }

            match self.peek() {
                None => break,
                Some(b'\n') => {
                    self.advance();
                    continue;
                }
                Some(b'\r') => {
                    self.advance();
                    if self.peek() == Some(b'\n') {
                        self.advance();
                    }
                    continue;
                }
                Some(b'[') => self.parse_table_header()?,
                Some(_) => self.parse_key_value()?,
            }

            self.skip_inline_whitespace();
            if self.peek() == Some(b'#') {
                self.skip_comment();
            }
            match self.peek() {
                None => break,
                Some(b'\n') => {
                    self.advance();
                }
                Some(b'\r') => {
                    self.advance();
                    if self.peek() == Some(b'\n') {
                        self.advance();
                    }
                }
                Some(_) => return Err(self.error("expected a newline")),
            }
        }

        Ok(self.root)
    }

    fn parse_table_header(&mut self) -> Result<(), Error> {
        let position = self.position();
        self.expect(b'[')?;
        let array = self.peek() == Some(b'[');
        if array {
            self.advance();
        }

        let path = self.parse_key_path()?;
        self.skip_inline_whitespace();
        self.expect(b']')?;
        if array {
            self.expect(b']')?;
        }

        if array {
            self.append_array_table(&path, position)?;
        } else {
            let table = resolve_table(&mut self.root, &path, position)?;
            if table.inline {
                return Err(error_at(position, "cannot extend an inline table"));
            }
            if table.defined {
                return Err(error_at(
                    position,
                    format!("table `{}` is defined twice", path.join(".")),
                ));
            }
            table.defined = true;
        }

        self.current = path;
        Ok(())
    }

    fn append_array_table(&mut self, path: &[String], position: Position) -> Result<(), Error> {
        let Some((last, parents)) = path.split_last() else {
            return Err(error_at(position, "expected a table name"));
        };
        let parent = resolve_table(&mut self.root, parents, position)?;

        match parent.entries.iter().position(|(key, _)| key == last) {
            Some(index) => match &mut parent.entries[index].1 {
                Value::Array(values) => {
                    if values.iter().any(|value| !matches!(value, Value::Table(_))) {
                        return Err(error_at(
                            position,
                            format!("`{last}` is not an array of tables"),
                        ));
                    }
                    values.push(Value::Table(defined_table()));
                }
                Value::Table(_) => {
                    return Err(error_at(position, format!("`{last}` is already a table")))
                }
                _ => {
                    return Err(error_at(
                        position,
                        format!("`{last}` is not an array of tables"),
                    ))
                }
            },
            None => {
                parent.entries.push((
                    last.clone(),
                    Value::Array(vec![Value::Table(defined_table())]),
                ));
            }
        }

        Ok(())
    }

    fn parse_key_value(&mut self) -> Result<(), Error> {
        let position = self.position();
        let path = self.parse_key_path()?;
        self.skip_inline_whitespace();
        self.expect(b'=')?;
        self.skip_inline_whitespace();
        let value = self.parse_value()?;

        let current = self.current.clone();
        let table = resolve_table(&mut self.root, &current, position)?;
        insert_dotted(table, &path, value, position)
    }

    fn parse_value(&mut self) -> Result<Value, Error> {
        match self.peek() {
            None => Err(self.error("expected a value")),
            Some(b'"') if self.starts_with(b"\"\"\"") => {
                self.parse_multiline_basic().map(Value::String)
            }
            Some(b'"') => self.parse_basic_string().map(Value::String),
            Some(b'\'') if self.starts_with(b"'''") => {
                self.parse_multiline_literal().map(Value::String)
            }
            Some(b'\'') => self.parse_literal_string().map(Value::String),
            Some(b'[') => self.parse_array(),
            Some(b'{') => self.parse_inline_table(),
            Some(b't') => self.parse_keyword(b"true", Value::Boolean(true)),
            Some(b'f') => self.parse_keyword(b"false", Value::Boolean(false)),
            Some(b'+' | b'-' | b'0'..=b'9') => self.parse_number(),
            Some(b'i' | b'n') => self.parse_number(),
            Some(_) => Err(self.error("expected a value")),
        }
    }

    fn parse_array(&mut self) -> Result<Value, Error> {
        self.enter()?;
        self.expect(b'[')?;
        let mut values = Vec::new();

        loop {
            self.skip_array_whitespace();
            if self.peek() == Some(b']') {
                self.advance();
                self.leave();
                return Ok(Value::Array(values));
            }

            values.push(self.parse_value()?);
            self.skip_array_whitespace();
            match self.next() {
                Some(b',') => continue,
                Some(b']') => {
                    self.leave();
                    return Ok(Value::Array(values));
                }
                Some(_) => return Err(self.error("expected a comma or closing bracket")),
                None => return Err(self.error("unterminated array")),
            }
        }
    }

    fn parse_inline_table(&mut self) -> Result<Value, Error> {
        self.enter()?;
        self.expect(b'{')?;

        let mut table = Table::new();
        table.inline = true;
        table.defined = true;

        self.skip_inline_whitespace();
        if self.peek() == Some(b'}') {
            self.advance();
            self.leave();
            return Ok(Value::Table(table));
        }

        loop {
            if matches!(self.peek(), Some(b'\n' | b'\r')) {
                return Err(self.error("newline in inline table"));
            }

            let position = self.position();
            let path = self.parse_key_path()?;
            self.skip_inline_whitespace();
            self.expect(b'=')?;
            self.skip_inline_whitespace();
            let value = self.parse_value()?;
            insert_dotted(&mut table, &path, value, position)?;

            self.skip_inline_whitespace();
            match self.next() {
                Some(b',') => {
                    self.skip_inline_whitespace();
                    if self.peek() == Some(b'}') {
                        return Err(self.error("trailing comma in inline table"));
                    }
                }
                Some(b'}') => {
                    self.leave();
                    return Ok(Value::Table(table));
                }
                Some(_) => return Err(self.error("expected a comma or closing brace")),
                None => return Err(self.error("unterminated inline table")),
            }
        }
    }

    fn parse_key_path(&mut self) -> Result<Vec<String>, Error> {
        let mut path = Vec::new();

        loop {
            path.push(self.parse_key()?);
            self.skip_inline_whitespace();
            if self.peek() == Some(b'.') {
                self.advance();
                self.skip_inline_whitespace();
            } else {
                break;
            }
        }

        Ok(path)
    }

    fn parse_key(&mut self) -> Result<String, Error> {
        match self.peek() {
            Some(b'"') if self.starts_with(b"\"\"\"") => {
                Err(self.error("multiline keys are not allowed"))
            }
            Some(b'"') => self.parse_basic_string(),
            Some(b'\'') if self.starts_with(b"'''") => {
                Err(self.error("multiline keys are not allowed"))
            }
            Some(b'\'') => self.parse_literal_string(),
            Some(byte) if is_bare_key_byte(byte) => {
                let start = self.pos;
                while matches!(self.peek(), Some(byte) if is_bare_key_byte(byte)) {
                    self.advance();
                }
                Ok(self.input[start..self.pos].to_string())
            }
            _ => Err(self.error("expected a key")),
        }
    }

    fn parse_basic_string(&mut self) -> Result<String, Error> {
        let start = self.position();
        self.expect(b'"')?;
        let mut out = String::new();

        loop {
            let Some(byte) = self.peek() else {
                return Err(error_at(start, "unterminated string"));
            };
            match byte {
                b'"' => {
                    self.advance();
                    return Ok(out);
                }
                b'\\' => {
                    self.advance();
                    self.parse_escape(&mut out, false)?;
                }
                b'\n' | b'\r' => return Err(self.error("newline in string")),
                0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | 0x7f => {
                    return Err(self.error("control character in string"))
                }
                _ => out.push(self.next_char()?),
            }
        }
    }

    fn parse_literal_string(&mut self) -> Result<String, Error> {
        let start = self.position();
        self.expect(b'\'')?;
        let mut out = String::new();

        loop {
            let Some(byte) = self.peek() else {
                return Err(error_at(start, "unterminated string"));
            };
            match byte {
                b'\'' => {
                    self.advance();
                    return Ok(out);
                }
                b'\n' | b'\r' => return Err(self.error("newline in string")),
                0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | 0x7f => {
                    return Err(self.error("control character in string"))
                }
                _ => out.push(self.next_char()?),
            }
        }
    }

    fn parse_multiline_basic(&mut self) -> Result<String, Error> {
        let start = self.position();
        self.expect_str("\"\"\"")?;
        self.trim_first_newline();
        let mut out = String::new();

        loop {
            if self.peek() == Some(b'"') {
                let count = self.count_byte(b'"');
                if count >= 3 {
                    if count > 5 {
                        return Err(self.error("too many quotes in multiline string"));
                    }
                    for _ in 0..count - 3 {
                        out.push('"');
                    }
                    for _ in 0..count {
                        self.advance();
                    }
                    return Ok(out);
                }
                for _ in 0..count {
                    self.advance();
                    out.push('"');
                }
                continue;
            }

            let Some(byte) = self.peek() else {
                return Err(error_at(start, "unterminated string"));
            };
            match byte {
                b'\\' => {
                    self.advance();
                    self.parse_escape(&mut out, true)?;
                }
                b'\n' => {
                    self.advance();
                    out.push('\n');
                }
                b'\r' => {
                    self.advance();
                    if self.peek() == Some(b'\n') {
                        self.advance();
                    }
                    out.push('\n');
                }
                0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | 0x7f => {
                    return Err(self.error("control character in string"))
                }
                _ => out.push(self.next_char()?),
            }
        }
    }

    fn parse_multiline_literal(&mut self) -> Result<String, Error> {
        let start = self.position();
        self.expect_str("'''")?;
        self.trim_first_newline();
        let mut out = String::new();

        loop {
            if self.peek() == Some(b'\'') {
                let count = self.count_byte(b'\'');
                if count >= 3 {
                    if count > 5 {
                        return Err(self.error("too many quotes in multiline string"));
                    }
                    for _ in 0..count - 3 {
                        out.push('\'');
                    }
                    for _ in 0..count {
                        self.advance();
                    }
                    return Ok(out);
                }
                for _ in 0..count {
                    self.advance();
                    out.push('\'');
                }
                continue;
            }

            let Some(byte) = self.peek() else {
                return Err(error_at(start, "unterminated string"));
            };
            match byte {
                b'\n' => {
                    self.advance();
                    out.push('\n');
                }
                b'\r' => {
                    self.advance();
                    if self.peek() == Some(b'\n') {
                        self.advance();
                    }
                    out.push('\n');
                }
                0x00..=0x08 | 0x0b | 0x0c | 0x0e..=0x1f | 0x7f => {
                    return Err(self.error("control character in string"))
                }
                _ => out.push(self.next_char()?),
            }
        }
    }

    fn parse_escape(&mut self, out: &mut String, multiline: bool) -> Result<(), Error> {
        let Some(byte) = self.next() else {
            return Err(self.error("unterminated escape"));
        };

        match byte {
            b'b' => out.push('\u{0008}'),
            b't' => out.push('\t'),
            b'n' => out.push('\n'),
            b'f' => out.push('\u{000c}'),
            b'r' => out.push('\r'),
            b'"' => out.push('"'),
            b'\\' => out.push('\\'),
            b'u' => {
                let code = self.parse_hex_escape(4)?;
                out.push(char::from_u32(code).ok_or_else(|| self.error("invalid unicode escape"))?);
            }
            b'U' => {
                let code = self.parse_hex_escape(8)?;
                out.push(char::from_u32(code).ok_or_else(|| self.error("invalid unicode escape"))?);
            }
            b'\n' if multiline => self.skip_line_continuation(),
            b'\r' if multiline => {
                if self.peek() == Some(b'\n') {
                    self.advance();
                }
                self.skip_line_continuation();
            }
            _ => return Err(self.error("invalid escape")),
        }

        Ok(())
    }

    fn skip_line_continuation(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n') => self.advance(),
                Some(b'\r') => {
                    self.advance();
                    if self.peek() == Some(b'\n') {
                        self.advance();
                    }
                }
                _ => break,
            }
        }
    }

    fn parse_hex_escape(&mut self, length: usize) -> Result<u32, Error> {
        let mut value = 0;

        for _ in 0..length {
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

    fn parse_keyword(&mut self, keyword: &[u8], value: Value) -> Result<Value, Error> {
        if self.input.as_bytes()[self.pos..].starts_with(keyword) {
            for _ in 0..keyword.len() {
                self.advance();
            }
            Ok(value)
        } else {
            Err(self.error("expected a value"))
        }
    }

    fn parse_number(&mut self) -> Result<Value, Error> {
        let position = self.position();
        let start = self.pos;

        while let Some(byte) = self.peek() {
            if is_value_delimiter(byte) {
                break;
            }
            self.advance();
        }

        parse_number_token(&self.input[start..self.pos])
            .map_err(|message| error_at(position, message))
    }

    fn enter(&mut self) -> Result<(), Error> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(self.error("nesting too deep"));
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn trim_first_newline(&mut self) {
        if self.peek() == Some(b'\n') {
            self.advance();
        } else if self.peek() == Some(b'\r') {
            self.advance();
            if self.peek() == Some(b'\n') {
                self.advance();
            }
        }
    }

    fn skip_inline_whitespace(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.advance();
        }
    }

    fn skip_array_whitespace(&mut self) {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\n' | b'\r') => self.advance(),
                Some(b'#') => self.skip_comment(),
                _ => break,
            }
        }
    }

    fn skip_comment(&mut self) {
        while let Some(byte) = self.peek() {
            if byte == b'\n' || byte == b'\r' {
                break;
            }
            self.advance();
        }
    }

    fn count_byte(&self, byte: u8) -> usize {
        self.input.as_bytes()[self.pos..]
            .iter()
            .take_while(|&&value| value == byte)
            .count()
    }

    fn next_char(&mut self) -> Result<char, Error> {
        let character = self.input[self.pos..]
            .chars()
            .next()
            .ok_or_else(|| self.error("unexpected end of input"))?;
        for _ in 0..character.len_utf8() {
            self.advance();
        }
        Ok(character)
    }

    fn expect(&mut self, byte: u8) -> Result<(), Error> {
        if self.peek() == Some(byte) {
            self.advance();
            Ok(())
        } else {
            Err(self.error(format!("expected `{}`", byte as char)))
        }
    }

    fn expect_str(&mut self, text: &str) -> Result<(), Error> {
        if self.input.as_bytes()[self.pos..].starts_with(text.as_bytes()) {
            for _ in 0..text.len() {
                self.advance();
            }
            Ok(())
        } else {
            Err(self.error("expected a string"))
        }
    }

    fn starts_with(&self, text: &[u8]) -> bool {
        self.input.as_bytes()[self.pos..].starts_with(text)
    }

    fn peek(&self) -> Option<u8> {
        self.input.as_bytes().get(self.pos).copied()
    }

    fn next(&mut self) -> Option<u8> {
        let byte = self.peek()?;
        self.advance();
        Some(byte)
    }

    fn advance(&mut self) {
        if let Some(byte) = self.input.as_bytes().get(self.pos).copied() {
            self.pos += 1;
            if byte == b'\n' {
                self.line += 1;
                self.column = 1;
            } else {
                self.column += 1;
            }
        }
    }

    fn position(&self) -> Position {
        Position {
            line: self.line,
            column: self.column,
        }
    }

    fn error(&self, message: impl Into<String>) -> Error {
        Error::new(self.line, self.column, message)
    }
}

fn resolve_table<'a>(
    root: &'a mut Table,
    path: &[String],
    position: Position,
) -> Result<&'a mut Table, Error> {
    let mut table = root;
    for key in path {
        let value = table_entry(table, key);
        table = descend_table(value, key, position)?;
    }
    Ok(table)
}

fn table_entry<'a>(table: &'a mut Table, key: &str) -> &'a mut Value {
    if let Some(index) = table.entries.iter().position(|(name, _)| name == key) {
        &mut table.entries[index].1
    } else {
        table
            .entries
            .push((key.to_string(), Value::Table(Table::new())));
        &mut table
            .entries
            .last_mut()
            .expect("table entry was just inserted")
            .1
    }
}

fn descend_table<'a>(
    value: &'a mut Value,
    key: &str,
    position: Position,
) -> Result<&'a mut Table, Error> {
    match value {
        Value::Table(table) => {
            if table.inline {
                return Err(error_at(position, "cannot extend an inline table"));
            }
            Ok(table)
        }
        Value::Array(values) => {
            let Some(Value::Table(table)) = values.last_mut() else {
                return Err(error_at(position, format!("`{key}` is not a table")));
            };
            if table.inline {
                return Err(error_at(position, "cannot extend an inline table"));
            }
            Ok(table)
        }
        _ => Err(error_at(position, format!("`{key}` is not a table"))),
    }
}

fn insert_dotted(
    table: &mut Table,
    path: &[String],
    value: Value,
    position: Position,
) -> Result<(), Error> {
    let Some((last, parents)) = path.split_last() else {
        return Err(error_at(position, "expected a key"));
    };

    let mut table = table;
    for key in parents {
        let entry = table_entry(table, key);
        table = descend_table(entry, key, position)?;
    }

    table
        .insert(last.clone(), value)
        .map_err(|_| error_at(position, format!("duplicate key `{last}`")))
}

fn defined_table() -> Table {
    let mut table = Table::new();
    table.defined = true;
    table
}

fn is_bare_key_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-'
}

fn is_value_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        b' ' | b'\t' | b'\n' | b'\r' | b'#' | b',' | b']' | b'}'
    )
}

fn error_at(position: Position, message: impl Into<String>) -> Error {
    Error::new(position.line, position.column, message)
}

fn parse_number_token(token: &str) -> Result<Value, &'static str> {
    if token.is_empty() {
        return Err("expected a value");
    }

    if looks_like_datetime(token) {
        return Err("date and time values are not supported");
    }

    if let Some(value) = parse_special_float(token) {
        return Ok(Value::Float(value));
    }

    let (sign, body) = match token.as_bytes().first() {
        Some(b'+') => (1, &token[1..]),
        Some(b'-') => (-1, &token[1..]),
        _ => (1, token),
    };

    if body.is_empty() {
        return Err("invalid number");
    }

    if body.starts_with("0x") || body.starts_with("0o") || body.starts_with("0b") {
        if sign != 1 || token.starts_with('+') {
            return Err("base-prefixed integers cannot have a sign");
        }
        return parse_radix_integer(body);
    }

    if body.contains('.') || body.contains('e') || body.contains('E') {
        return parse_float(sign, body);
    }

    parse_decimal_integer(sign, body)
}

fn looks_like_datetime(token: &str) -> bool {
    let bytes = token.as_bytes();
    let date = bytes.len() >= 5
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2].is_ascii_digit()
        && bytes[3].is_ascii_digit()
        && bytes[4] == b'-';
    let time = bytes.len() >= 3
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2] == b':';
    date || time
}

fn parse_special_float(token: &str) -> Option<f64> {
    match token {
        "inf" | "+inf" => Some(f64::INFINITY),
        "-inf" => Some(f64::NEG_INFINITY),
        "nan" | "+nan" | "-nan" => Some(f64::NAN),
        _ => None,
    }
}

fn parse_decimal_integer(sign: i64, body: &str) -> Result<Value, &'static str> {
    let cleaned = strip_underscores(body, false)?;
    let bytes = cleaned.as_bytes();

    if bytes.is_empty() {
        return Err("invalid integer");
    }
    if bytes[0] == b'0' && bytes.len() > 1 {
        return Err("leading zero in integer");
    }

    let magnitude: i128 = cleaned.parse().map_err(|_| "invalid integer")?;
    let value = if sign < 0 { -magnitude } else { magnitude };
    if value < i64::MIN as i128 || value > i64::MAX as i128 {
        return Err("integer out of range");
    }

    Ok(Value::Integer(value as i64))
}

fn parse_radix_integer(body: &str) -> Result<Value, &'static str> {
    let (radix, digits) = if let Some(rest) = body.strip_prefix("0x") {
        (16, rest)
    } else if let Some(rest) = body.strip_prefix("0o") {
        (8, rest)
    } else if let Some(rest) = body.strip_prefix("0b") {
        (2, rest)
    } else {
        return Err("invalid integer");
    };

    if digits.is_empty() || digits.starts_with('+') || digits.starts_with('-') {
        return Err("base-prefixed integers cannot have a sign");
    }

    let cleaned = strip_underscores(digits, true)?;
    let value = i64::from_str_radix(&cleaned, radix).map_err(|_| "integer out of range")?;
    Ok(Value::Integer(value))
}

fn parse_float(sign: i64, body: &str) -> Result<Value, &'static str> {
    let cleaned = strip_underscores(body, false)?;
    validate_float(&cleaned)?;

    let mut text = cleaned;
    if sign < 0 {
        text.insert(0, '-');
    }

    let value: f64 = text.parse().map_err(|_| "invalid float")?;
    if !value.is_finite() {
        return Err("float out of range");
    }
    Ok(Value::Float(value))
}

fn validate_float(text: &str) -> Result<(), &'static str> {
    let exponent_index = text.find(|character: char| character == 'e' || character == 'E');
    let (mantissa, exponent) = match exponent_index {
        Some(index) => (&text[..index], Some(&text[index + 1..])),
        None => (text, None),
    };

    if let Some(exponent) = exponent {
        let exponent = exponent
            .strip_prefix('+')
            .or_else(|| exponent.strip_prefix('-'))
            .unwrap_or(exponent);
        if exponent.is_empty() || !exponent.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("invalid float exponent");
        }
    }

    let (integer, fraction) = match mantissa.split_once('.') {
        Some((integer, fraction)) => (integer, Some(fraction)),
        None => (mantissa, None),
    };

    if integer.is_empty() || !integer.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("invalid float");
    }
    if integer.len() > 1 && integer.starts_with('0') {
        return Err("leading zero in float");
    }
    if let Some(fraction) = fraction {
        if fraction.is_empty() || !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("invalid float");
        }
    }
    if exponent.is_none() && fraction.is_none() {
        return Err("invalid float");
    }

    Ok(())
}

fn strip_underscores(token: &str, hex: bool) -> Result<String, &'static str> {
    let bytes = token.as_bytes();
    let mut out = String::with_capacity(token.len());

    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'_' {
            let valid = |byte: u8| {
                if hex {
                    byte.is_ascii_hexdigit()
                } else {
                    byte.is_ascii_digit()
                }
            };
            let previous = index
                .checked_sub(1)
                .and_then(|index| bytes.get(index))
                .copied();
            let next = bytes.get(index + 1).copied();
            if !previous.map(valid).unwrap_or(false) || !next.map(valid).unwrap_or(false) {
                return Err("misplaced underscore in number");
            }
        } else {
            if !byte.is_ascii() {
                return Err("invalid number");
            }
            out.push(byte as char);
        }
    }

    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::{parse, Table, Value};

    const MANIFESTS: &[(&str, &str)] = &[
        (
            "cfg-if",
            include_str!("../../tests/fixtures/manifests/cfg-if-1.0.5.toml"),
        ),
        (
            "anyhow",
            include_str!("../../tests/fixtures/manifests/anyhow-1.0.104.toml"),
        ),
        (
            "serde",
            include_str!("../../tests/fixtures/manifests/serde-1.0.229.toml"),
        ),
        (
            "syn",
            include_str!("../../tests/fixtures/manifests/syn-3.0.9.toml"),
        ),
        (
            "clap",
            include_str!("../../tests/fixtures/manifests/clap-4.6.7.toml"),
        ),
    ];

    fn table_at<'a>(table: &'a Table, path: &[&str]) -> &'a Table {
        let mut current = table;
        for key in path {
            current = current
                .get(key)
                .and_then(Value::as_table)
                .unwrap_or_else(|| panic!("missing table `{key}`"));
        }
        current
    }

    #[test]
    fn parses_cargo_style_manifest() {
        let input = r#"
[package]
name = "demo"
version = "0.1.0"
edition = "2021"

[dependencies]
serde = { version = "1", features = ["derive"] }
anyhow = "1"

[target.'cfg(unix)'.dependencies]
libc = "0.2"

[[bin]]
name = "demo"
path = "src/main.rs"
"#;
        let root = parse(input).unwrap();

        assert_eq!(
            table_at(&root, &["package"])
                .get("name")
                .and_then(Value::as_str),
            Some("demo")
        );

        let dependencies = table_at(&root, &["dependencies"]);
        let serde = dependencies.get("serde").and_then(Value::as_table).unwrap();
        assert_eq!(serde.get("version").and_then(Value::as_str), Some("1"));
        assert_eq!(
            serde.get("features").and_then(Value::as_array).unwrap()[0].as_str(),
            Some("derive")
        );

        let target = table_at(&root, &["target", "cfg(unix)", "dependencies"]);
        assert_eq!(target.get("libc").and_then(Value::as_str), Some("0.2"));

        let bins = root.get("bin").and_then(Value::as_array).unwrap();
        assert_eq!(bins.len(), 1);
        assert_eq!(
            bins[0]
                .as_table()
                .unwrap()
                .get("name")
                .and_then(Value::as_str),
            Some("demo")
        );
    }

    #[test]
    fn parses_dotted_keys() {
        let root = parse("a.b = 1\na.c = 2\n").unwrap();
        let a = table_at(&root, &["a"]);
        assert_eq!(a.get("b").and_then(Value::as_integer), Some(1));
        assert_eq!(a.get("c").and_then(Value::as_integer), Some(2));
    }

    #[test]
    fn parses_string_forms() {
        let input = "basic = \"a\\nb\\t\\u00e9\\U0001F600\"\nliteral = 'C:\\path\\.\\.'\nmultiline = \"\"\"\nline1\nline2\"\"\"\nmultiline_literal = '''\nraw \\n\n'''\n";
        let root = parse(input).unwrap();

        assert_eq!(root.get("basic").and_then(Value::as_str), Some("a\nb\té😀"));
        assert_eq!(
            root.get("literal").and_then(Value::as_str),
            Some("C:\\path\\.\\.")
        );
        assert_eq!(
            root.get("multiline").and_then(Value::as_str),
            Some("line1\nline2")
        );
        assert_eq!(
            root.get("multiline_literal").and_then(Value::as_str),
            Some("raw \\n\n")
        );
    }

    #[test]
    fn parses_numbers_and_booleans() {
        let input = "a = 0\nb = -1\nc = 1_000\nd = 0xDEAD_BEEF\ne = 0o755\nk = 0b1101\ng = 1.5\nh = -2e3\ni = inf\nj = nan\nt = true\nf = false\n";
        let root = parse(input).unwrap();

        assert_eq!(root.get("a").and_then(Value::as_integer), Some(0));
        assert_eq!(root.get("b").and_then(Value::as_integer), Some(-1));
        assert_eq!(root.get("c").and_then(Value::as_integer), Some(1000));
        assert_eq!(root.get("d").and_then(Value::as_integer), Some(0xDEAD_BEEF));
        assert_eq!(root.get("e").and_then(Value::as_integer), Some(0o755));
        assert_eq!(root.get("k").and_then(Value::as_integer), Some(0b1101));
        assert_eq!(root.get("g").and_then(Value::as_float), Some(1.5));
        assert_eq!(root.get("h").and_then(Value::as_float), Some(-2000.0));
        assert!(root
            .get("i")
            .and_then(Value::as_float)
            .unwrap()
            .is_infinite());
        assert!(root.get("j").and_then(Value::as_float).unwrap().is_nan());
        assert_eq!(root.get("t").and_then(Value::as_bool), Some(true));
        assert_eq!(root.get("f").and_then(Value::as_bool), Some(false));
    }

    #[test]
    fn parses_multiline_line_continuation() {
        let root = parse("a = \"\"\"one \\\n    two\"\"\"\n").unwrap();
        assert_eq!(root.get("a").and_then(Value::as_str), Some("one two"));
    }

    #[test]
    fn parses_quote_runs_in_multiline_strings() {
        let root = parse("a = \"\"\"one \"\" two\"\"\"\n").unwrap();
        assert_eq!(root.get("a").and_then(Value::as_str), Some("one \"\" two"));

        let root = parse("a = \"\"\"one \"\"\"\"\n").unwrap();
        assert_eq!(root.get("a").and_then(Value::as_str), Some("one \""));
    }

    #[test]
    fn parses_arrays_with_comments_and_trailing_comma() {
        let root = parse("a = [\n  1, # one\n  2,\n]\n").unwrap();
        let values = root.get("a").and_then(Value::as_array).unwrap();
        assert_eq!(values.len(), 2);
        assert_eq!(values[1].as_integer(), Some(2));
    }

    #[test]
    fn rejects_inline_table_newlines_and_trailing_comma() {
        assert!(parse("a = { b = 1,\n c = 2 }\n")
            .unwrap_err()
            .message
            .contains("newline"));
        assert!(parse("a = { b = 1, }\n")
            .unwrap_err()
            .message
            .contains("trailing comma"));
        assert!(parse("a = { b = 1 }\n[a.c]\n")
            .unwrap_err()
            .message
            .contains("inline"));
    }

    #[test]
    fn rejects_duplicate_keys_and_tables() {
        let error = parse("[a]\nb = 1\nb = 2\n").unwrap_err();
        assert_eq!((error.line, error.column), (3, 1));
        assert!(error.message.contains("duplicate key"));

        let error = parse("[a]\n[a]\n").unwrap_err();
        assert!(error.message.contains("defined twice"));
    }

    #[test]
    fn allows_parent_table_after_child() {
        let root = parse("[a.b]\nx = 1\n[a]\ny = 2\n").unwrap();
        assert_eq!(
            table_at(&root, &["a"]).get("y").and_then(Value::as_integer),
            Some(2)
        );
        assert_eq!(
            table_at(&root, &["a", "b"])
                .get("x")
                .and_then(Value::as_integer),
            Some(1)
        );
    }

    #[test]
    fn parses_arrays_of_tables() {
        let root = parse("[[bin]]\nname = \"a\"\n[[bin]]\nname = \"b\"\n").unwrap();
        let bins = root.get("bin").and_then(Value::as_array).unwrap();
        assert_eq!(bins.len(), 2);
        assert_eq!(
            bins[1]
                .as_table()
                .unwrap()
                .get("name")
                .and_then(Value::as_str),
            Some("b")
        );
    }

    #[test]
    fn reports_malformed_input_position() {
        let error = parse("[package\nname = \"x\"\n").unwrap_err();
        assert_eq!(error.line, 1);
        assert!(error.column > 1);
        assert!(error.message.contains("expected"));
    }

    #[test]
    fn rejects_nesting_beyond_the_limit() {
        let input = format!("a = {}1{}", "[".repeat(200), "]".repeat(200));
        let error = parse(&input).unwrap_err();
        assert_eq!(error.message, "nesting too deep");
    }

    #[test]
    fn parses_crlf_documents() {
        let root = parse("[a]\r\nb = 1\r\nc = 'x'\r\n").unwrap();
        let a = table_at(&root, &["a"]);
        assert_eq!(a.get("b").and_then(Value::as_integer), Some(1));
        assert_eq!(a.get("c").and_then(Value::as_str), Some("x"));
    }

    #[test]
    fn skips_a_leading_byte_order_mark() {
        let root = parse("\u{feff}[a]\nb = 1\n").unwrap();
        assert_eq!(
            table_at(&root, &["a"]).get("b").and_then(Value::as_integer),
            Some(1)
        );
    }

    #[test]
    fn never_panics_on_malformed_input() {
        for input in [
            "",
            "}ffff",
            "/",
            "a",
            "a=1",
            "a = ",
            "a = [",
            "a = {",
            "a = \"",
            "a = \"\\",
            "a = '''",
            "a = \"\"\"",
            "a = \"\\uD800",
            "a = \"\\u",
            "[[a]",
            "[a.b]",
            "a inf",
            "a = inf",
            "a = 1x",
            "a = 1_",
            "a = 0x_",
            "a = 1e",
            "a = 1.",
            "a = .5",
            "a = 00",
            "a = 0x",
            "a = 1é",
            "a = 1979-05-27",
            "a = 12:30",
            "\"\"x = 1",
        ] {
            let _ = parse(input);
        }
    }

    #[test]
    fn parses_real_manifests() {
        for (name, input) in MANIFESTS {
            let root = parse(input).unwrap_or_else(|error| panic!("{name}: {error}"));
            let package = root
                .get("package")
                .and_then(Value::as_table)
                .unwrap_or_else(|| panic!("{name}: missing package table"));
            assert!(
                package.get("name").and_then(Value::as_str).is_some(),
                "{name}: missing package name"
            );
        }
    }
}
