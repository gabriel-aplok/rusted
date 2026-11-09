use std::path::Path;

pub(crate) fn add_dependency(
    input: &str,
    section: &str,
    name: &str,
    value: &str,
    path: &Path,
) -> Result<String, String> {
    let newline = newline_for(input);
    let mut lines: Vec<String> = input.split_inclusive('\n').map(str::to_string).collect();
    let header = format!("[{section}]");

    let start = lines
        .iter()
        .position(|line| section_header(line).as_deref() == Some(section));

    if let Some((index, line)) = table_style(&lines, section, name) {
        return Err(ambiguous(path, name, index, line, value));
    }

    let Some(start) = start else {
        let mut output = input.to_string();
        if !output.is_empty() && !output.ends_with('\n') {
            output.push_str(newline);
        }
        if !output.is_empty() && !output.ends_with(&format!("{newline}{newline}")) {
            output.push_str(newline);
        }
        output.push_str(&format!("{header}{newline}{name} = {value}{newline}"));
        return Ok(output);
    };

    let end = section_end(&lines, start);
    for index in start + 1..end {
        if let Some(key) = line_key(&lines[index]) {
            if key == name || key == format!("{name}.") || key.starts_with(&format!("{name}.")) {
                if key != name {
                    return Err(ambiguous(path, name, index, &lines[index], value));
                }
                let indent = leading_whitespace(&lines[index]);
                let ending = line_ending(&lines[index], newline);
                lines[index] = format!("{indent}{name} = {value}{ending}");
                return Ok(lines.concat());
            }
        }
    }

    let mut insert = end;
    while insert > start + 1 && lines[insert - 1].trim().is_empty() {
        insert -= 1;
    }
    lines.insert(insert, format!("{name} = {value}{newline}"));
    Ok(lines.concat())
}

pub(crate) fn remove_dependency(input: &str, name: &str, path: &Path) -> Result<String, String> {
    let mut lines: Vec<String> = input.split_inclusive('\n').map(str::to_string).collect();
    let sections = ["dependencies", "dev-dependencies", "build-dependencies"];
    let mut removed = false;

    for section in sections {
        let Some(start) = lines
            .iter()
            .position(|line| section_header(line).as_deref() == Some(section))
        else {
            continue;
        };
        let end = section_end(&lines, start);
        for index in start + 1..end {
            if let Some(key) = line_key(&lines[index]) {
                if key == name {
                    lines.remove(index);
                    removed = true;
                    break;
                }
                if key.starts_with(&format!("{name}.")) {
                    return Err(ambiguous(path, name, index, &lines[index], ""));
                }
            }
        }
        if let Some((index, line)) = table_style(&lines, section, name) {
            return Err(ambiguous(path, name, index, line, ""));
        }
    }

    if !removed {
        return Err(format!(
            "`{name}` is not a dependency in {}",
            path.display()
        ));
    }
    Ok(lines.concat())
}

fn section_header(line: &str) -> Option<String> {
    let trimmed = line.trim();
    if !trimmed.starts_with('[') || trimmed.starts_with("[[") {
        return None;
    }
    let end = trimmed.find(']')?;
    Some(trimmed[1..end].trim().to_string())
}

fn section_end(lines: &[String], start: usize) -> usize {
    lines
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, line)| {
            let trimmed = line.trim_start();
            trimmed.starts_with('[') || trimmed.starts_with("[[")
        })
        .map(|(index, _)| index)
        .unwrap_or(lines.len())
}

fn line_key(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (key, _) = trimmed.split_once('=')?;
    let key = key.trim();
    let key = key
        .strip_prefix('"')
        .and_then(|key| key.strip_suffix('"'))
        .or_else(|| {
            key.strip_prefix('\'')
                .and_then(|key| key.strip_suffix('\''))
        })
        .unwrap_or(key);
    Some(key.to_string())
}

fn table_style<'a>(lines: &'a [String], section: &str, name: &str) -> Option<(usize, &'a str)> {
    let dotted = format!("{section}.{name}");
    let quoted = format!("{section}.\"{name}\"");
    let single = format!("{section}.'{name}'");
    lines.iter().enumerate().find_map(|(index, line)| {
        let header = section_header(line)?;
        if header == dotted || header == quoted || header == single {
            Some((index, line.as_str()))
        } else {
            None
        }
    })
}

fn ambiguous(path: &Path, name: &str, index: usize, line: &str, value: &str) -> String {
    let mut message = format!(
        "`{name}` is declared with a table or dotted key in {}, edit it manually\n\n",
        path.display()
    );
    message.push_str(&format!("line {}: {}\n", index + 1, line.trim_end()));
    if !value.is_empty() {
        message.push_str("\nsuggested replacement:\n");
        message.push_str(&format!("- {}\n", line.trim_end()));
        message.push_str(&format!("+ {name} = {value}\n"));
    }
    message
}

fn leading_whitespace(line: &str) -> &str {
    let trimmed = line.trim_start();
    &line[..line.len() - trimmed.len()]
}

fn line_ending<'a>(line: &'a str, default: &'a str) -> &'a str {
    if line.ends_with("\r\n") {
        "\r\n"
    } else if line.ends_with('\n') {
        "\n"
    } else {
        default
    }
}

fn newline_for(input: &str) -> &'static str {
    if input.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    }
}

#[cfg(test)]
mod tests {
    use super::{add_dependency, remove_dependency};
    use std::path::Path;

    #[test]
    fn adds_to_an_existing_section() {
        let input = "[package]\nname = \"app\"\n\n[dependencies]\n# keep me\nfoo = \"1\"\n";
        let output = add_dependency(
            input,
            "dependencies",
            "bar",
            "{ version = \"2\", features = [\"x\"] }",
            Path::new("Cargo.toml"),
        )
        .unwrap();
        assert!(output.contains("# keep me"));
        assert!(output.contains("foo = \"1\""));
        assert!(output.contains("bar = { version = \"2\", features = [\"x\"] }"));
        assert!(output.ends_with('\n'));
    }

    #[test]
    fn creates_a_missing_section() {
        let input = "[package]\nname = \"app\"\n";
        let output = add_dependency(
            input,
            "dev-dependencies",
            "bar",
            "\"1\"",
            Path::new("Cargo.toml"),
        )
        .unwrap();
        assert!(output.contains("[dev-dependencies]\nbar = \"1\"\n"));
    }

    #[test]
    fn replaces_an_existing_simple_entry() {
        let input = "[dependencies]\nbar = \"1\" # comment\n";
        let output = add_dependency(
            input,
            "dependencies",
            "bar",
            "\"2\"",
            Path::new("Cargo.toml"),
        )
        .unwrap();
        assert!(output.contains("bar = \"2\"\n"));
    }

    #[test]
    fn refuses_table_style_entries() {
        let input = "[dependencies.bar]\nversion = \"1\"\n";
        let error = add_dependency(
            input,
            "dependencies",
            "bar",
            "\"2\"",
            Path::new("Cargo.toml"),
        )
        .unwrap_err();
        assert!(error.contains("[dependencies.bar]"), "{error}");
        assert!(error.contains("edit it manually"), "{error}");
    }

    #[test]
    fn removes_entries_from_all_sections() {
        let input = "[dependencies]\nfoo = \"1\"\nbar = \"1\"\n\n[dev-dependencies]\nbar = \"2\"\n";
        let output = remove_dependency(input, "bar", Path::new("Cargo.toml")).unwrap();
        assert!(!output.contains("bar = \"1\""));
        assert!(!output.contains("bar = \"2\""));
        assert!(output.contains("foo = \"1\""));
    }

    #[test]
    fn preserves_crlf() {
        let input = "[dependencies]\r\nfoo = \"1\"\r\n";
        let output = add_dependency(
            input,
            "dependencies",
            "bar",
            "\"1\"",
            Path::new("Cargo.toml"),
        )
        .unwrap();
        assert!(output.contains("\r\nbar = \"1\"\r\n"));
    }
}
