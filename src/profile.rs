use crate::toml::{Table, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OptLevel {
    O0,
    O1,
    O2,
    O3,
    SizeS,
    SizeZ,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DebugInfo {
    None,
    LineTablesOnly,
    Full,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Strip {
    None,
    Debuginfo,
    Symbols,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lto {
    Off,
    Thin,
    Fat,
    ThinLocal,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Panic {
    Unwind,
    Abort,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Linker {
    System,
    Lld,
    Mold,
    Path(String),
}

impl Linker {
    pub(crate) fn parse(text: &str) -> Self {
        match text {
            "system" => Self::System,
            "lld" => Self::Lld,
            "mold" => Self::Mold,
            other => Self::Path(other.to_string()),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Profile {
    pub(crate) opt_level: Option<OptLevel>,
    pub(crate) debug: Option<DebugInfo>,
    pub(crate) strip: Option<Strip>,
    pub(crate) lto: Option<Lto>,
    pub(crate) codegen_units: Option<u32>,
    pub(crate) panic: Option<Panic>,
    pub(crate) incremental: Option<bool>,
    pub(crate) overflow_checks: Option<bool>,
    pub(crate) debug_assertions: Option<bool>,
    pub(crate) linker: Option<Linker>,
}

impl Profile {
    pub(crate) fn from_table(table: &Table) -> Result<(Self, Vec<String>), String> {
        let mut profile = Self::default();
        let mut unknown = Vec::new();

        for (key, value) in table.iter() {
            match key {
                "opt-level" => profile.opt_level = Some(parse_opt_level(value)?),
                "debug" => profile.debug = Some(parse_debug(value)?),
                "strip" => profile.strip = Some(parse_strip(value)?),
                "lto" => profile.lto = Some(parse_lto(value)?),
                "codegen-units" => profile.codegen_units = Some(parse_codegen_units(value)?),
                "panic" => profile.panic = Some(parse_panic(value)?),
                "incremental" => profile.incremental = Some(parse_bool(key, value)?),
                "overflow-checks" => profile.overflow_checks = Some(parse_bool(key, value)?),
                "debug-assertions" => profile.debug_assertions = Some(parse_bool(key, value)?),
                "linker" => profile.linker = Some(parse_linker(value)?),
                _ => unknown.push(key.to_string()),
            }
        }

        Ok((profile, unknown))
    }
}

fn parse_opt_level(value: &Value) -> Result<OptLevel, String> {
    match value {
        Value::Integer(level @ 0..=3) => Ok(match level {
            0 => OptLevel::O0,
            1 => OptLevel::O1,
            2 => OptLevel::O2,
            _ => OptLevel::O3,
        }),
        Value::String(level) => match level.as_str() {
            "s" => Ok(OptLevel::SizeS),
            "z" => Ok(OptLevel::SizeZ),
            _ => Err("`opt-level` must be an integer from 0 to 3 or `s`/`z`".into()),
        },
        _ => Err("`opt-level` must be an integer from 0 to 3 or `s`/`z`".into()),
    }
}

fn parse_debug(value: &Value) -> Result<DebugInfo, String> {
    match value {
        Value::Integer(0) => Ok(DebugInfo::None),
        Value::Integer(1) => Ok(DebugInfo::LineTablesOnly),
        Value::Integer(2) => Ok(DebugInfo::Full),
        Value::Boolean(false) => Ok(DebugInfo::None),
        Value::Boolean(true) => Ok(DebugInfo::Full),
        Value::String(text) => match text.as_str() {
            "none" => Ok(DebugInfo::None),
            "line-tables-only" => Ok(DebugInfo::LineTablesOnly),
            "full" => Ok(DebugInfo::Full),
            _ => Err(
                "`debug` must be 0, 1, 2, `none`, `line-tables-only`, `full`, or a boolean".into(),
            ),
        },
        _ => {
            Err("`debug` must be 0, 1, 2, `none`, `line-tables-only`, `full`, or a boolean".into())
        }
    }
}

fn parse_strip(value: &Value) -> Result<Strip, String> {
    match value {
        Value::Boolean(false) => return Ok(Strip::None),
        Value::Boolean(true) => return Ok(Strip::Symbols),
        Value::String(text) => match text.as_str() {
            "none" => return Ok(Strip::None),
            "debuginfo" => return Ok(Strip::Debuginfo),
            "symbols" => return Ok(Strip::Symbols),
            _ => {}
        },
        _ => {}
    }
    Err("`strip` must be `none`, `debuginfo`, `symbols`, or a boolean".into())
}

fn parse_lto(value: &Value) -> Result<Lto, String> {
    match value {
        Value::Boolean(false) => return Ok(Lto::Off),
        Value::Boolean(true) => return Ok(Lto::Fat),
        Value::String(text) => match text.as_str() {
            "off" => return Ok(Lto::Off),
            "thin" => return Ok(Lto::Thin),
            "fat" => return Ok(Lto::Fat),
            "thin-local" => return Ok(Lto::ThinLocal),
            _ => {}
        },
        _ => {}
    }
    Err("`lto` must be a boolean or `fat`, `thin`, `thin-local`, `off`".into())
}

fn parse_codegen_units(value: &Value) -> Result<u32, String> {
    match value {
        Value::Integer(units @ 1..=65_535) => Ok(*units as u32),
        _ => Err("`codegen-units` must be an integer from 1 to 65535".into()),
    }
}

fn parse_panic(value: &Value) -> Result<Panic, String> {
    let Value::String(text) = value else {
        return Err("`panic` must be `unwind` or `abort`".into());
    };
    match text.as_str() {
        "unwind" => Ok(Panic::Unwind),
        "abort" => Ok(Panic::Abort),
        _ => Err("`panic` must be `unwind` or `abort`".into()),
    }
}

fn parse_bool(key: &str, value: &Value) -> Result<bool, String> {
    match value {
        Value::Boolean(value) => Ok(*value),
        _ => Err(format!("`{key}` must be a boolean")),
    }
}

fn parse_linker(value: &Value) -> Result<Linker, String> {
    match value {
        Value::String(text) => Ok(Linker::parse(text)),
        _ => Err("`linker` must be `system`, `lld`, `mold`, or a path".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::Linker;

    #[test]
    fn parses_linker_choices() {
        assert_eq!(Linker::parse("system"), Linker::System);
        assert_eq!(Linker::parse("lld"), Linker::Lld);
        assert_eq!(Linker::parse("mold"), Linker::Mold);
        assert_eq!(
            Linker::parse("/usr/bin/mold"),
            Linker::Path("/usr/bin/mold".to_string())
        );
    }
}
