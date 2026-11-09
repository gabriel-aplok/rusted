use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

#[derive(Clone, Debug, Default)]
pub(crate) struct Cfg {
    bare: BTreeSet<String>,
    pairs: BTreeSet<(String, String)>,
    target: String,
}

impl Cfg {
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn new(
        bare: impl IntoIterator<Item = String>,
        pairs: impl IntoIterator<Item = (String, String)>,
        target: impl Into<String>,
    ) -> Self {
        Self {
            bare: bare.into_iter().collect(),
            pairs: pairs.into_iter().collect(),
            target: target.into(),
        }
    }

    #[cfg(test)]
    pub(crate) fn host() -> Result<Self, String> {
        let identity = crate::build::fingerprint::rustc_identity()?;
        let target = identity.host().to_string();
        let cfg_text = spawn_cfg()?;
        Self::parse_output(&cfg_text, &target)
    }

    pub(crate) fn cached(cache_path: &Path) -> Result<Self, String> {
        let identity = crate::build::fingerprint::rustc_identity()?;
        let target = identity.host().to_string();
        let key = format!(
            "key={}|{}|{}",
            identity.release(),
            identity.host(),
            identity.commit()
        );

        if let Ok(text) = std::fs::read_to_string(cache_path) {
            let mut lines = text.lines();
            if lines.next() == Some(key.as_str()) {
                let body = lines.collect::<Vec<_>>().join(
                    "
",
                );
                return Self::parse_output(&body, &target);
            }
        }

        let cfg_text = spawn_cfg()?;
        let cfg = Self::parse_output(&cfg_text, &target)?;

        let mut text = key;
        text.push('\n');
        text.push_str(&cfg_text);
        text.push('\n');
        let temp = cache_path.with_extension(format!("tmp.{}", std::process::id()));
        if let Some(parent) = cache_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("{}: {error}", parent.display()))?;
        }
        std::fs::write(&temp, &text)
            .and_then(|_| std::fs::rename(&temp, cache_path))
            .map_err(|error| format!("{}: {error}", cache_path.display()))?;
        Ok(cfg)
    }

    fn parse_output(output: &str, target: &str) -> Result<Self, String> {
        let mut bare = BTreeSet::new();
        let mut pairs = BTreeSet::new();
        for line in output.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match line.split_once('=') {
                Some((key, value)) => {
                    pairs.insert((
                        key.trim().to_string(),
                        value.trim().trim_matches('"').to_string(),
                    ));
                }
                None => {
                    bare.insert(line.to_string());
                }
            }
        }
        Ok(Self {
            bare,
            pairs,
            target: target.to_string(),
        })
    }

    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn target(&self) -> &str {
        &self.target
    }

    pub(crate) fn environment(&self) -> Vec<(String, String)> {
        let mut values: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for bare in &self.bare {
            values
                .entry(bare.to_ascii_uppercase().replace('-', "_"))
                .or_default()
                .push("1".to_string());
        }
        for (key, value) in &self.pairs {
            values
                .entry(key.to_ascii_uppercase().replace('-', "_"))
                .or_default()
                .push(value.clone());
        }
        values
            .into_iter()
            .map(|(key, values)| (format!("CARGO_CFG_{key}"), values.join(",")))
            .collect()
    }

    pub(crate) fn matches(&self, expression: &str) -> Result<bool, String> {
        let expression = expression.trim();
        if expression.is_empty() {
            return Ok(true);
        }
        let inner = if let Some(rest) = expression.strip_prefix("cfg(") {
            let Some(rest) = rest.strip_suffix(')') else {
                return Err(format!("invalid cfg expression `{expression}`"));
            };
            rest
        } else {
            return Ok(expression == self.target);
        };

        let mut parser = Parser {
            tokens: tokenize(inner)?,
            position: 0,
            cfg: self,
        };
        let value = parser.parse_expression()?;
        if parser.peek() != &Token::End {
            return Err(format!("unexpected token in cfg expression `{expression}`"));
        }
        Ok(value)
    }
}

fn spawn_cfg() -> Result<String, String> {
    let rustc = crate::build::rustc_program();
    let output = Command::new(&rustc)
        .arg("--print")
        .arg("cfg")
        .output()
        .map_err(|error| format!("failed to run rustc --print cfg: {error}"))?;
    if !output.status.success() {
        return Err(format!("rustc --print cfg failed with {}", output.status));
    }
    String::from_utf8(output.stdout)
        .map(|text| text.trim_end_matches('\n').to_string())
        .map_err(|error| format!("rustc --print cfg output is not UTF-8: {error}"))
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Token {
    Identifier(String),
    String(String),
    LeftParen,
    RightParen,
    Comma,
    Equals,
    End,
}

fn tokenize(input: &str) -> Result<Vec<Token>, String> {
    let bytes = input.as_bytes();
    let mut tokens = Vec::new();
    let mut index = 0;

    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        match byte {
            b'(' => {
                tokens.push(Token::LeftParen);
                index += 1;
            }
            b')' => {
                tokens.push(Token::RightParen);
                index += 1;
            }
            b',' => {
                tokens.push(Token::Comma);
                index += 1;
            }
            b'=' => {
                tokens.push(Token::Equals);
                index += 1;
            }
            b'"' => {
                index += 1;
                let mut value = String::new();
                let mut closed = false;
                while index < bytes.len() {
                    match bytes[index] {
                        b'"' => {
                            index += 1;
                            closed = true;
                            break;
                        }
                        b'\\' => {
                            index += 1;
                            let Some(escaped) = bytes.get(index).copied() else {
                                break;
                            };
                            value.push(escaped as char);
                            index += 1;
                        }
                        byte => {
                            value.push(byte as char);
                            index += 1;
                        }
                    }
                }
                if !closed {
                    return Err("unterminated string in cfg expression".to_string());
                }
                tokens.push(Token::String(value));
            }
            byte if byte.is_ascii_alphanumeric() || byte == b'_' => {
                let start = index;
                while index < bytes.len()
                    && (bytes[index].is_ascii_alphanumeric() || bytes[index] == b'_')
                {
                    index += 1;
                }
                tokens.push(Token::Identifier(
                    std::str::from_utf8(&bytes[start..index])
                        .map_err(|_| "invalid identifier in cfg expression".to_string())?
                        .to_string(),
                ));
            }
            _ => {
                return Err(format!(
                    "unexpected character `{}` in cfg expression",
                    byte as char
                ))
            }
        }
    }

    tokens.push(Token::End);
    Ok(tokens)
}

struct Parser<'a> {
    tokens: Vec<Token>,
    position: usize,
    cfg: &'a Cfg,
}

impl Parser<'_> {
    fn peek(&self) -> &Token {
        self.tokens.get(self.position).unwrap_or(&Token::End)
    }

    fn next(&mut self) -> Token {
        let token = self.peek().clone();
        self.position += 1;
        token
    }

    fn parse_expression(&mut self) -> Result<bool, String> {
        match self.next() {
            Token::Identifier(name) => match name.as_str() {
                "all" => self.parse_list(true),
                "any" => self.parse_list(false),
                "not" => self.parse_not(),
                _ => {
                    if self.peek() == &Token::Equals {
                        self.position += 1;
                        match self.next() {
                            Token::String(value) => Ok(self.cfg.pairs.contains(&(name, value))),
                            _ => Err("expected a string after `=` in cfg expression".to_string()),
                        }
                    } else {
                        Ok(self.cfg.bare.contains(&name))
                    }
                }
            },
            _ => Err("expected an identifier in cfg expression".to_string()),
        }
    }

    fn parse_list(&mut self, all: bool) -> Result<bool, String> {
        if self.next() != Token::LeftParen {
            return Err("expected `(` in cfg expression".to_string());
        }
        let mut value = all;
        if self.peek() == &Token::RightParen {
            self.position += 1;
            return Ok(value);
        }
        loop {
            let item = self.parse_expression()?;
            value = if all { value && item } else { value || item };
            match self.next() {
                Token::Comma => continue,
                Token::RightParen => break,
                _ => return Err("expected `,` or `)` in cfg expression".to_string()),
            }
        }
        Ok(value)
    }

    fn parse_not(&mut self) -> Result<bool, String> {
        if self.next() != Token::LeftParen {
            return Err("expected `(` after `not`".to_string());
        }
        let value = self.parse_expression()?;
        if self.next() != Token::RightParen {
            return Err("expected `)` after `not`".to_string());
        }
        Ok(!value)
    }
}

#[cfg(test)]
mod tests {
    use super::Cfg;

    fn test_cfg() -> Cfg {
        Cfg::new(
            ["unix".to_string(), "debug_assertions".to_string()],
            [
                ("target_os".to_string(), "linux".to_string()),
                ("target_arch".to_string(), "x86_64".to_string()),
                ("target_env".to_string(), "gnu".to_string()),
                ("target_family".to_string(), "unix".to_string()),
                ("target_pointer_width".to_string(), "64".to_string()),
            ],
            "x86_64-unknown-linux-gnu",
        )
    }

    #[test]
    fn evaluates_bare_and_paired_keys() {
        let cfg = test_cfg();
        assert!(cfg.matches("cfg(unix)").unwrap());
        assert!(!cfg.matches("cfg(windows)").unwrap());
        assert!(cfg.matches("cfg(target_os = \"linux\")").unwrap());
        assert!(!cfg.matches("cfg(target_os = \"windows\")").unwrap());
        assert!(cfg.matches("cfg(target_arch = \"x86_64\")").unwrap());
        assert!(cfg.matches("cfg(target_env = \"gnu\")").unwrap());
        assert!(cfg.matches("cfg(target_family = \"unix\")").unwrap());
        assert!(cfg.matches("cfg(target_pointer_width = \"64\")").unwrap());
        assert!(!cfg.matches("cfg(target_pointer_width = \"32\")").unwrap());
        assert!(cfg
            .matches(
                "cfg(all(unix, any(target_os = \"linux\", target_os = \"macos\"), not(windows)))"
            )
            .unwrap());
        assert!(cfg.matches("x86_64-unknown-linux-gnu").unwrap());
        assert!(!cfg.matches("aarch64-unknown-linux-gnu").unwrap());
        assert!(cfg.matches("").unwrap());
    }

    #[test]
    fn evaluates_boolean_functions() {
        let cfg = test_cfg();
        assert!(cfg
            .matches("cfg(all(unix, target_arch = \"x86_64\"))")
            .unwrap());
        assert!(!cfg
            .matches("cfg(all(unix, target_arch = \"aarch64\"))")
            .unwrap());
        assert!(cfg
            .matches("cfg(any(windows, target_os = \"linux\"))")
            .unwrap());
        assert!(!cfg
            .matches("cfg(any(windows, target_os = \"macos\"))")
            .unwrap());
        assert!(cfg.matches("cfg(not(windows))").unwrap());
        assert!(cfg.matches("cfg(all())").unwrap());
        assert!(!cfg.matches("cfg(any())").unwrap());
    }

    #[test]
    fn rejects_malformed_expressions() {
        let cfg = test_cfg();
        for expression in [
            "cfg(",
            "cfg()",
            "cfg(all(unix)",
            "cfg(not(unix, unix))",
            "cfg(all(, unix))",
            "cfg(target_os = linux)",
            "cfg(unix))",
        ] {
            assert!(cfg.matches(expression).is_err(), "{expression}");
        }
    }

    #[test]
    fn host_reports_a_target() {
        let cfg = Cfg::host().unwrap();
        assert!(cfg.target().contains('-'), "{}", cfg.target());
    }
}
