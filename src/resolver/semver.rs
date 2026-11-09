use std::cmp::Ordering;
use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
enum Identifier {
    Numeric(u64),
    Alpha(String),
}

#[derive(Clone, Debug)]
pub(crate) struct Version {
    pub(crate) major: u64,
    pub(crate) minor: u64,
    pub(crate) patch: u64,
    pre: Vec<Identifier>,
    build: Vec<String>,
}

impl Version {
    pub(crate) fn parse(input: &str) -> Result<Self, String> {
        let (core, build) = split_build(input)?;
        let (core, pre) = split_pre(core)?;
        let mut parts = core.split('.');
        let major = parse_number(parts.next(), "major")?;
        let minor = parse_number(parts.next(), "minor")?;
        let patch = parse_number(parts.next(), "patch")?;
        if parts.next().is_some() {
            return Err(format!("`{input}` has more than three version components"));
        }
        Ok(Self {
            major,
            minor,
            patch,
            pre,
            build,
        })
    }

    pub(crate) fn is_prerelease(&self) -> bool {
        !self.pre.is_empty()
    }

    fn core_cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch).cmp(&(other.major, other.minor, other.patch))
    }
}

impl PartialEq for Version {
    fn eq(&self, other: &Self) -> bool {
        self.core_cmp(other) == Ordering::Equal && self.pre == other.pre
    }
}

impl Eq for Version {}

impl std::hash::Hash for Version {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.major.hash(state);
        self.minor.hash(state);
        self.patch.hash(state);
        self.pre.hash(state);
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core_cmp(other)
            .then_with(|| compare_pre(&self.pre, &other.pre))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            formatter.write_str("-")?;
            write_identifiers(formatter, &self.pre)?;
        }
        if !self.build.is_empty() {
            formatter.write_str("+")?;
            for (index, identifier) in self.build.iter().enumerate() {
                if index > 0 {
                    formatter.write_str(".")?;
                }
                formatter.write_str(identifier)?;
            }
        }
        Ok(())
    }
}

fn write_identifiers(
    formatter: &mut fmt::Formatter<'_>,
    identifiers: &[Identifier],
) -> fmt::Result {
    for (index, identifier) in identifiers.iter().enumerate() {
        if index > 0 {
            formatter.write_str(".")?;
        }
        match identifier {
            Identifier::Numeric(value) => write!(formatter, "{value}")?,
            Identifier::Alpha(value) => formatter.write_str(value)?,
        }
    }
    Ok(())
}

fn compare_pre(left: &[Identifier], right: &[Identifier]) -> Ordering {
    match (left.is_empty(), right.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater,
        (false, true) => return Ordering::Less,
        (false, false) => {}
    }
    for (left, right) in left.iter().zip(right.iter()) {
        let ordering = left.cmp(right);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    left.len().cmp(&right.len())
}

fn split_build(input: &str) -> Result<(&str, Vec<String>), String> {
    match input.split_once('+') {
        None => Ok((input, Vec::new())),
        Some((core, build)) => {
            if build.is_empty() {
                return Err(format!("`{input}` has empty build metadata"));
            }
            let identifiers = build
                .split('.')
                .map(|identifier| {
                    validate_identifier(identifier, true)
                        .map(|_| identifier.to_string())
                        .map_err(|error| format!("`{input}`: {error}"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            Ok((core, identifiers))
        }
    }
}

fn split_pre(input: &str) -> Result<(&str, Vec<Identifier>), String> {
    let Some(index) = input.find('-') else {
        return Ok((input, Vec::new()));
    };
    let (core, pre) = (&input[..index], &input[index + 1..]);
    if pre.is_empty() {
        return Err(format!("`{input}` has empty pre-release"));
    }
    if core.split('.').count() != 3 {
        return Err(format!(
            "`{input}` must have three numeric components before a pre-release"
        ));
    }
    let mut identifiers = Vec::new();
    for identifier in pre.split('.') {
        validate_identifier(identifier, false).map_err(|error| format!("`{input}`: {error}"))?;
        identifiers.push(match identifier.parse::<u64>() {
            Ok(value) => Identifier::Numeric(value),
            Err(_) => Identifier::Alpha(identifier.to_string()),
        });
    }
    Ok((core, identifiers))
}

fn validate_identifier(identifier: &str, allow_leading_zeros: bool) -> Result<(), String> {
    if identifier.is_empty() {
        return Err("empty identifier".to_string());
    }
    if !identifier
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err(format!("invalid identifier `{identifier}`"));
    }
    if !allow_leading_zeros
        && identifier.len() > 1
        && identifier.starts_with('0')
        && identifier.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(format!(
            "numeric identifier `{identifier}` has a leading zero"
        ));
    }
    Ok(())
}

fn parse_number(part: Option<&str>, name: &str) -> Result<u64, String> {
    let part = part.ok_or_else(|| format!("missing {name} version component"))?;
    parse_number_text(part).map_err(|error| format!("invalid {name} component `{part}`: {error}"))
}

fn parse_number_text(text: &str) -> Result<u64, String> {
    if text.is_empty() {
        return Err("empty number".to_string());
    }
    if text.len() > 1 && text.starts_with('0') {
        return Err("leading zero".to_string());
    }
    if !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("not a number".to_string());
    }
    text.parse().map_err(|_| "number out of range".to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Op {
    Caret,
    Tilde,
    Exact,
    Greater,
    GreaterEq,
    Less,
    LessEq,
    Wildcard,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct Comparator {
    op: Op,
    major: u64,
    minor: Option<u64>,
    patch: Option<u64>,
    pre: Vec<Identifier>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct VersionReq {
    comparators: Vec<Comparator>,
}

impl VersionReq {
    pub(crate) fn parse(input: &str) -> Result<Self, String> {
        let input = input.trim();
        if input.is_empty() {
            return Err("empty version requirement".to_string());
        }

        let mut comparators = Vec::new();
        for text in input.split(',') {
            let text = text.trim();
            if text.is_empty() {
                return Err(format!("`{input}` has an empty comparator"));
            }
            if text == "*" {
                continue;
            }
            comparators.push(parse_comparator(text)?);
        }

        Ok(Self { comparators })
    }

    pub(crate) fn matches(&self, version: &Version) -> bool {
        if self.comparators.is_empty() {
            return version.pre.is_empty();
        }
        if !self
            .comparators
            .iter()
            .all(|comparator| comparator.matches(version))
        {
            return false;
        }
        if version.pre.is_empty() {
            return true;
        }
        self.comparators
            .iter()
            .any(|comparator| comparator.pre_base_matches(version))
    }
}

fn parse_comparator(text: &str) -> Result<Comparator, String> {
    let (op, rest, explicit) = if let Some(rest) = text.strip_prefix(">=") {
        (Op::GreaterEq, rest, true)
    } else if let Some(rest) = text.strip_prefix("<=") {
        (Op::LessEq, rest, true)
    } else if let Some(rest) = text.strip_prefix('>') {
        (Op::Greater, rest, true)
    } else if let Some(rest) = text.strip_prefix('<') {
        (Op::Less, rest, true)
    } else if let Some(rest) = text.strip_prefix('=') {
        (Op::Exact, rest, true)
    } else if let Some(rest) = text.strip_prefix('~') {
        (Op::Tilde, rest, true)
    } else if let Some(rest) = text.strip_prefix('^') {
        (Op::Caret, rest, true)
    } else {
        (Op::Caret, text, false)
    };
    let rest = rest.trim();
    if rest.is_empty() {
        return Err(format!("comparator `{text}` has no version"));
    }

    let (rest, _) = split_build(rest).map_err(|error| format!("comparator `{text}`: {error}"))?;
    let (core, pre) = split_pre(rest).map_err(|error| format!("comparator `{text}`: {error}"))?;
    let mut parts = core.split('.');
    let first = parts.next().unwrap_or("");
    if first == "*" {
        if explicit {
            return Err(format!(
                "comparator `{text}` cannot combine an operator with a wildcard"
            ));
        }
        if parts.next().is_some() {
            return Err(format!("comparator `{text}` has an invalid wildcard"));
        }
        return Ok(Comparator {
            op: Op::Wildcard,
            major: 0,
            minor: None,
            patch: None,
            pre: Vec::new(),
        });
    }
    let major =
        parse_number_text(first).map_err(|error| format!("comparator `{text}`: {error}"))?;
    let mut minor = None;
    let mut patch = None;
    let mut wildcard = Op::Caret;

    if let Some(part) = parts.next() {
        if part == "*" {
            wildcard = Op::Wildcard;
        } else {
            minor = Some(
                parse_number_text(part).map_err(|error| format!("comparator `{text}`: {error}"))?,
            );
        }
    }
    if let Some(part) = parts.next() {
        if part == "*" {
            wildcard = Op::Wildcard;
        } else if minor.is_none() {
            return Err(format!("comparator `{text}` has a gap before patch"));
        } else {
            patch = Some(
                parse_number_text(part).map_err(|error| format!("comparator `{text}`: {error}"))?,
            );
        }
    }
    if parts.next().is_some() {
        return Err(format!("comparator `{text}` has too many components"));
    }

    let op = if wildcard == Op::Wildcard {
        if explicit {
            return Err(format!(
                "comparator `{text}` cannot combine an operator with a wildcard"
            ));
        }
        if !pre.is_empty() {
            return Err(format!(
                "comparator `{text}` cannot combine a wildcard with a pre-release"
            ));
        }
        Op::Wildcard
    } else {
        op
    };

    Ok(Comparator {
        op,
        major,
        minor,
        patch,
        pre,
    })
}

impl Comparator {
    fn lower(&self) -> Version {
        Version {
            major: self.major,
            minor: self.minor.unwrap_or(0),
            patch: self.patch.unwrap_or(0),
            pre: self.pre.clone(),
            build: Vec::new(),
        }
    }

    fn upper_for(&self, major: u64, minor: u64, patch: u64) -> Version {
        Version {
            major,
            minor,
            patch,
            pre: Vec::new(),
            build: Vec::new(),
        }
    }

    fn pre_base_matches(&self, version: &Version) -> bool {
        !self.pre.is_empty()
            && self.major == version.major
            && self.minor == Some(version.minor)
            && self.patch == Some(version.patch)
    }

    fn matches(&self, version: &Version) -> bool {
        match self.op {
            Op::Caret => {
                if version < &self.lower() {
                    return false;
                }
                let upper = if self.major > 0 {
                    self.upper_for(self.major + 1, 0, 0)
                } else {
                    match (self.minor, self.patch) {
                        (Some(minor), _) if minor > 0 => self.upper_for(0, minor + 1, 0),
                        (Some(_), Some(patch)) => self.upper_for(0, 0, patch + 1),
                        (Some(_), None) => self.upper_for(0, 1, 0),
                        (None, _) => self.upper_for(1, 0, 0),
                    }
                };
                version < &upper
            }
            Op::Tilde => {
                if version < &self.lower() {
                    return false;
                }
                let upper = match self.minor {
                    Some(minor) => self.upper_for(self.major, minor + 1, 0),
                    None => self.upper_for(self.major + 1, 0, 0),
                };
                version < &upper
            }
            Op::Exact => match (self.minor, self.patch) {
                (Some(_), Some(_)) => {
                    version.core_cmp(&self.lower()) == Ordering::Equal && version.pre == self.pre
                }
                (Some(minor), None) => {
                    version.major == self.major
                        && version.minor == minor
                        && *version >= self.lower()
                }
                (None, _) => version.major == self.major && *version >= self.lower(),
            },
            Op::Wildcard => match self.minor {
                Some(minor) => version.major == self.major && version.minor == minor,
                None => version.major == self.major && self.patch.is_none(),
            },
            Op::Greater => match self.patch {
                Some(_) => version > &self.lower(),
                None => match self.minor {
                    Some(minor) => *version >= self.upper_for(self.major, minor + 1, 0),
                    None => *version >= self.upper_for(self.major + 1, 0, 0),
                },
            },
            Op::GreaterEq => version >= &self.lower(),
            Op::Less => version < &self.lower(),
            Op::LessEq => match self.patch {
                Some(_) => version <= &self.lower(),
                None => match self.minor {
                    Some(minor) => *version < self.upper_for(self.major, minor + 1, 0),
                    None => *version < self.upper_for(self.major + 1, 0, 0),
                },
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Version, VersionReq};

    fn version(text: &str) -> Version {
        Version::parse(text).unwrap()
    }

    fn requirement(text: &str) -> VersionReq {
        VersionReq::parse(text).unwrap()
    }

    #[test]
    fn versions_compare_per_semver() {
        let ordered = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for pair in ordered.windows(2) {
            assert!(
                version(pair[0]) < version(pair[1]),
                "{} < {}",
                pair[0],
                pair[1]
            );
        }
        assert_eq!(version("1.0.0+meta"), version("1.0.0"));
        assert!(version("1.0.0+meta") < version("1.0.1"));
    }

    #[test]
    fn rejects_invalid_versions() {
        for text in [
            "",
            "1",
            "1.2",
            "01.2.3",
            "1.02.3",
            "1.2.03",
            "1.2.3-",
            "1.2.3+",
            "1.2.3-alpha..1",
            "1.2.3-alpha_1",
            "1.2.3-01",
            "1.2.3.4",
        ] {
            assert!(Version::parse(text).is_err(), "{text}");
        }
        assert!(Version::parse("1.2.3-alpha.1+build.5").is_ok());
    }

    #[test]
    fn caret_requirements_match_cargo() {
        assert!(requirement("^1.2.3").matches(&version("1.2.3")));
        assert!(requirement("^1.2.3").matches(&version("1.9.9")));
        assert!(!requirement("^1.2.3").matches(&version("1.2.2")));
        assert!(!requirement("^1.2.3").matches(&version("2.0.0")));
        assert!(requirement("^0.2.3").matches(&version("0.2.9")));
        assert!(!requirement("^0.2.3").matches(&version("0.3.0")));
        assert!(requirement("^0.0.3").matches(&version("0.0.3")));
        assert!(!requirement("^0.0.3").matches(&version("0.0.4")));
        assert!(requirement("^0").matches(&version("0.9.9")));
        assert!(!requirement("^0").matches(&version("1.0.0")));
        assert!(requirement("^0.0").matches(&version("0.0.9")));
        assert!(!requirement("^0.0").matches(&version("0.1.0")));
    }

    #[test]
    fn tilde_and_exact_requirements_match_cargo() {
        assert!(requirement("~1.2.3").matches(&version("1.2.9")));
        assert!(!requirement("~1.2.3").matches(&version("1.3.0")));
        assert!(requirement("~1.2").matches(&version("1.2.9")));
        assert!(!requirement("~1.2").matches(&version("1.3.0")));
        assert!(requirement("~1").matches(&version("1.9.0")));
        assert!(!requirement("~1").matches(&version("2.0.0")));
        assert_eq!(requirement("1.2.3"), requirement("^1.2.3"));
        assert!(requirement("=1.2.3").matches(&version("1.2.3")));
        assert!(!requirement("=1.2.3").matches(&version("1.2.4")));
        assert!(requirement("=1.2").matches(&version("1.2.9")));
        assert!(!requirement("=1.2").matches(&version("1.3.0")));
        assert!(requirement("=1").matches(&version("1.9.0")));
        assert!(!requirement("=1").matches(&version("2.0.0")));
    }

    #[test]
    fn range_requirements_match_cargo() {
        assert!(requirement(">1.2.3").matches(&version("1.2.4")));
        assert!(!requirement(">1.2.3").matches(&version("1.2.3")));
        assert!(requirement(">1.2").matches(&version("1.3.0")));
        assert!(!requirement(">1.2").matches(&version("1.2.9")));
        assert!(requirement(">1").matches(&version("2.0.0")));
        assert!(!requirement(">1").matches(&version("1.9.9")));
        assert!(requirement(">=1.2").matches(&version("1.2.0")));
        assert!(requirement("<2.0.0").matches(&version("1.9.9")));
        assert!(!requirement("<2.0.0").matches(&version("2.0.0")));
        assert!(requirement("<=1.2").matches(&version("1.2.9")));
        assert!(!requirement("<=1.2").matches(&version("1.3.0")));
        assert!(requirement("<=1").matches(&version("1.9.9")));
        assert!(!requirement("<=1").matches(&version("2.0.0")));
        assert!(requirement(">=1.2, <2.0").matches(&version("1.5.0")));
        assert!(!requirement(">=1.2, <2.0").matches(&version("2.0.0")));
    }

    #[test]
    fn wildcards_match_cargo() {
        assert!(requirement("*").matches(&version("0.0.1")));
        assert!(requirement("*").matches(&version("9.9.9")));
        assert!(!requirement("*").matches(&version("1.0.0-alpha")));
        assert!(requirement("1.*").matches(&version("1.9.9")));
        assert!(!requirement("1.*").matches(&version("2.0.0")));
        assert!(requirement("1.2.*").matches(&version("1.2.9")));
        assert!(!requirement("1.2.*").matches(&version("1.3.0")));
    }

    #[test]
    fn prereleases_require_an_explicit_base() {
        assert!(requirement("^1.0.0-alpha").matches(&version("1.0.0-beta")));
        assert!(requirement(">=1.0.0-alpha").matches(&version("1.0.0-beta")));
        assert!(!requirement(">=1.0.0-alpha").matches(&version("1.0.1-beta")));
        assert!(!requirement("^1.0.0").matches(&version("1.0.1-alpha")));
        assert!(!requirement("=1.0.0").matches(&version("1.0.0-alpha")));
        assert!(!requirement("*").matches(&version("1.0.0-alpha")));
        assert!(requirement("=1.0.0-alpha").matches(&version("1.0.0-alpha")));
        assert!(!requirement("=1.0.0-alpha").matches(&version("1.0.0-beta")));
        assert!(requirement("=1.0.0-alpha+meta").matches(&version("1.0.0-alpha+other")));
    }

    #[test]
    fn rejects_invalid_requirements() {
        for text in [
            "",
            ",",
            ">=,",
            ">=",
            "1.2.3.4",
            "01",
            "1.*.3",
            ">=1.*",
            "1.2.*-alpha",
        ] {
            assert!(VersionReq::parse(text).is_err(), "{text}");
        }
    }
}
#[test]
fn matches_comparators_like_the_semver_crate() {
    const EXPECTED: &[&str] = &[
        "^1	1.0.0	1",
        "^1	1.2.0	1",
        "^1	1.4.9	1",
        "^1	1.5.0	1",
        "^1	2.0.0	0",
        "^1	0.2.1	0",
        "^1	0.3.0	0",
        "^1	0.4.0	0",
        "^1	1.2.3	1",
        "^1	1.2.3-beta.1	0",
        "^1	1.2.4-beta.1	0",
        "^1	0.2.3-rc1	0",
        "^1.2	1.0.0	0",
        "^1.2	1.2.0	1",
        "^1.2	1.4.9	1",
        "^1.2	1.5.0	1",
        "^1.2	2.0.0	0",
        "^1.2	0.2.1	0",
        "^1.2	0.3.0	0",
        "^1.2	0.4.0	0",
        "^1.2	1.2.3	1",
        "^1.2	1.2.3-beta.1	0",
        "^1.2	1.2.4-beta.1	0",
        "^1.2	0.2.3-rc1	0",
        "~1.2	1.0.0	0",
        "~1.2	1.2.0	1",
        "~1.2	1.4.9	0",
        "~1.2	1.5.0	0",
        "~1.2	2.0.0	0",
        "~1.2	0.2.1	0",
        "~1.2	0.3.0	0",
        "~1.2	0.4.0	0",
        "~1.2	1.2.3	1",
        "~1.2	1.2.3-beta.1	0",
        "~1.2	1.2.4-beta.1	0",
        "~1.2	0.2.3-rc1	0",
        ">1.2	1.0.0	0",
        ">1.2	1.2.0	0",
        ">1.2	1.4.9	1",
        ">1.2	1.5.0	1",
        ">1.2	2.0.0	1",
        ">1.2	0.2.1	0",
        ">1.2	0.3.0	0",
        ">1.2	0.4.0	0",
        ">1.2	1.2.3	0",
        ">1.2	1.2.3-beta.1	0",
        ">1.2	1.2.4-beta.1	0",
        ">1.2	0.2.3-rc1	0",
        "<2	1.0.0	1",
        "<2	1.2.0	1",
        "<2	1.4.9	1",
        "<2	1.5.0	1",
        "<2	2.0.0	0",
        "<2	0.2.1	1",
        "<2	0.3.0	1",
        "<2	0.4.0	1",
        "<2	1.2.3	1",
        "<2	1.2.3-beta.1	0",
        "<2	1.2.4-beta.1	0",
        "<2	0.2.3-rc1	0",
        ">=1.2, <1.5	1.0.0	0",
        ">=1.2, <1.5	1.2.0	1",
        ">=1.2, <1.5	1.4.9	1",
        ">=1.2, <1.5	1.5.0	0",
        ">=1.2, <1.5	2.0.0	0",
        ">=1.2, <1.5	0.2.1	0",
        ">=1.2, <1.5	0.3.0	0",
        ">=1.2, <1.5	0.4.0	0",
        ">=1.2, <1.5	1.2.3	1",
        ">=1.2, <1.5	1.2.3-beta.1	0",
        ">=1.2, <1.5	1.2.4-beta.1	0",
        ">=1.2, <1.5	0.2.3-rc1	0",
        "1.*	1.0.0	1",
        "1.*	1.2.0	1",
        "1.*	1.4.9	1",
        "1.*	1.5.0	1",
        "1.*	2.0.0	0",
        "1.*	0.2.1	0",
        "1.*	0.3.0	0",
        "1.*	0.4.0	0",
        "1.*	1.2.3	1",
        "1.*	1.2.3-beta.1	0",
        "1.*	1.2.4-beta.1	0",
        "1.*	0.2.3-rc1	0",
        "0.3.*	1.0.0	0",
        "0.3.*	1.2.0	0",
        "0.3.*	1.4.9	0",
        "0.3.*	1.5.0	0",
        "0.3.*	2.0.0	0",
        "0.3.*	0.2.1	0",
        "0.3.*	0.3.0	1",
        "0.3.*	0.4.0	0",
        "0.3.*	1.2.3	0",
        "0.3.*	1.2.3-beta.1	0",
        "0.3.*	1.2.4-beta.1	0",
        "0.3.*	0.2.3-rc1	0",
        "=1.0.0	1.0.0	1",
        "=1.0.0	1.2.0	0",
        "=1.0.0	1.4.9	0",
        "=1.0.0	1.5.0	0",
        "=1.0.0	2.0.0	0",
        "=1.0.0	0.2.1	0",
        "=1.0.0	0.3.0	0",
        "=1.0.0	0.4.0	0",
        "=1.0.0	1.2.3	0",
        "=1.0.0	1.2.3-beta.1	0",
        "=1.0.0	1.2.4-beta.1	0",
        "=1.0.0	0.2.3-rc1	0",
        "^0.2	1.0.0	0",
        "^0.2	1.2.0	0",
        "^0.2	1.4.9	0",
        "^0.2	1.5.0	0",
        "^0.2	2.0.0	0",
        "^0.2	0.2.1	1",
        "^0.2	0.3.0	0",
        "^0.2	0.4.0	0",
        "^0.2	1.2.3	0",
        "^0.2	1.2.3-beta.1	0",
        "^0.2	1.2.4-beta.1	0",
        "^0.2	0.2.3-rc1	0",
        "*	1.0.0	1",
        "*	1.2.0	1",
        "*	1.4.9	1",
        "*	1.5.0	1",
        "*	2.0.0	1",
        "*	0.2.1	1",
        "*	0.3.0	1",
        "*	0.4.0	1",
        "*	1.2.3	1",
        "*	1.2.3-beta.1	0",
        "*	1.2.4-beta.1	0",
        "*	0.2.3-rc1	0",
        "^1.2.3-beta	1.0.0	0",
        "^1.2.3-beta	1.2.0	0",
        "^1.2.3-beta	1.4.9	1",
        "^1.2.3-beta	1.5.0	1",
        "^1.2.3-beta	2.0.0	0",
        "^1.2.3-beta	0.2.1	0",
        "^1.2.3-beta	0.3.0	0",
        "^1.2.3-beta	0.4.0	0",
        "^1.2.3-beta	1.2.3	1",
        "^1.2.3-beta	1.2.3-beta.1	1",
        "^1.2.3-beta	1.2.4-beta.1	0",
        "^1.2.3-beta	0.2.3-rc1	0",
        ">=1.0.0-alpha	1.0.0	1",
        ">=1.0.0-alpha	1.2.0	1",
        ">=1.0.0-alpha	1.4.9	1",
        ">=1.0.0-alpha	1.5.0	1",
        ">=1.0.0-alpha	2.0.0	1",
        ">=1.0.0-alpha	0.2.1	0",
        ">=1.0.0-alpha	0.3.0	0",
        ">=1.0.0-alpha	0.4.0	0",
        ">=1.0.0-alpha	1.2.3	1",
        ">=1.0.0-alpha	1.2.3-beta.1	0",
        ">=1.0.0-alpha	1.2.4-beta.1	0",
        ">=1.0.0-alpha	0.2.3-rc1	0",
        "^0.2.3-rc1	1.0.0	0",
        "^0.2.3-rc1	1.2.0	0",
        "^0.2.3-rc1	1.4.9	0",
        "^0.2.3-rc1	1.5.0	0",
        "^0.2.3-rc1	2.0.0	0",
        "^0.2.3-rc1	0.2.1	0",
        "^0.2.3-rc1	0.3.0	0",
        "^0.2.3-rc1	0.4.0	0",
        "^0.2.3-rc1	1.2.3	0",
        "^0.2.3-rc1	1.2.3-beta.1	0",
        "^0.2.3-rc1	1.2.4-beta.1	0",
        "^0.2.3-rc1	0.2.3-rc1	1",
    ];
    for line in EXPECTED {
        let mut parts = line.split('\t');
        let req = parts.next().unwrap();
        let version = parts.next().unwrap();
        let expected = parts.next().unwrap() == "1";
        let req = VersionReq::parse(req).unwrap();
        let version = Version::parse(version).unwrap();
        assert_eq!(req.matches(&version), expected, "{line}");
    }
}
