mod parser;

pub(crate) use parser::parse;

#[derive(Clone, Debug, Default)]
pub(crate) struct Table {
    entries: Vec<(String, Value)>,
    defined: bool,
    inline: bool,
}

impl Table {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn get(&self, key: &str) -> Option<&Value> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&str, &Value)> {
        self.entries
            .iter()
            .map(|(key, value)| (key.as_str(), value))
    }

    pub(crate) fn insert(&mut self, key: String, value: Value) -> Result<(), ()> {
        if self.get(&key).is_some() {
            Err(())
        } else {
            self.entries.push((key, value));
            Ok(())
        }
    }
}

impl PartialEq for Table {
    fn eq(&self, other: &Self) -> bool {
        self.entries == other.entries
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Value {
    String(String),
    Integer(i64),
    Float(f64),
    Boolean(bool),
    Array(Vec<Value>),
    Table(Table),
}

#[cfg_attr(not(test), allow(dead_code))]
impl Value {
    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(value) => Some(value),
            _ => None,
        }
    }

    pub(crate) fn as_integer(&self) -> Option<i64> {
        match self {
            Value::Integer(value) => Some(*value),
            _ => None,
        }
    }

    pub(crate) fn as_float(&self) -> Option<f64> {
        match self {
            Value::Float(value) => Some(*value),
            _ => None,
        }
    }

    pub(crate) fn as_bool(&self) -> Option<bool> {
        match self {
            Value::Boolean(value) => Some(*value),
            _ => None,
        }
    }

    pub(crate) fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(values) => Some(values),
            _ => None,
        }
    }

    pub(crate) fn as_table(&self) -> Option<&Table> {
        match self {
            Value::Table(table) => Some(table),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Error {
    pub(crate) line: usize,
    pub(crate) column: usize,
    pub(crate) message: String,
}

impl Error {
    pub(crate) fn new(line: usize, column: usize, message: impl Into<String>) -> Self {
        Self {
            line,
            column,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{} at line {}, column {}",
            self.message, self.line, self.column
        )
    }
}

impl std::error::Error for Error {}
