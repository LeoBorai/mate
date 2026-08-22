//! Hand-rolled parsing of a `SKILL.md`'s YAML frontmatter — deliberately narrow rather than a
//! general YAML parser. Only two fields are ever consumed (`name`, `description`), and in every
//! real `SKILL.md` seen from Claude Code's and OpenCode's own examples both are flat scalars,
//! occasionally quoted. Adding a real YAML dependency (the original `serde_yaml` is
//! unmaintained; `yaml-serde`, the YAML-org fork, is the closest drop-in today) is worth
//! revisiting only if a real skill author needs a block/folded scalar — not worth paying for
//! up front.

/// A `SKILL.md`'s frontmatter fields plus its body, with the frontmatter block itself
/// stripped — the shape both [`crate::discovery`] (name/description only) and
/// [`crate::skill`] (the body too) need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedSkill {
    pub name: String,
    pub description: String,
    pub body: String,
}

/// `name`'s length ceiling — matches Claude Code's own `SKILL.md` field rules, not anything
/// `mate` itself has to enforce; kept anyway so a name is always short enough to line up with
/// the "Available skills" preamble list without wrapping.
const MAX_NAME_LEN: usize = 64;

/// `description`'s length ceiling, mirrored from the same source as `MAX_NAME_LEN`.
const MAX_DESCRIPTION_LEN: usize = 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FrontmatterError {
    #[error("no YAML frontmatter block (must start with a `---` line and close with another)")]
    NoFrontmatter,
    #[error("frontmatter is missing required field `{0}`")]
    MissingField(&'static str),
    #[error("invalid `name` {0:?}: must be 1-64 chars, lowercase letters/digits/hyphens only")]
    InvalidName(String),
    #[error("invalid `description`: must be non-empty and at most 1024 chars")]
    InvalidDescription,
}

/// Parses `content` (a whole `SKILL.md` file) into its frontmatter fields and body.
pub fn parse(content: &str) -> Result<ParsedSkill, FrontmatterError> {
    let mut lines = content.lines();
    let first = lines.next().ok_or(FrontmatterError::NoFrontmatter)?;
    if first.trim() != "---" {
        return Err(FrontmatterError::NoFrontmatter);
    }

    let mut frontmatter_lines: Vec<&str> = Vec::new();
    let mut closed = false;
    for line in lines.by_ref() {
        if line.trim() == "---" {
            closed = true;
            break;
        }
        frontmatter_lines.push(line);
    }
    if !closed {
        return Err(FrontmatterError::NoFrontmatter);
    }

    let body: String = lines.collect::<Vec<_>>().join("\n").trim().to_string();
    let (name, description) = parse_fields(&frontmatter_lines)?;

    Ok(ParsedSkill {
        name,
        description,
        body,
    })
}

/// Extracts `name`/`description` from the lines between the frontmatter's `---` delimiters.
/// Every line that isn't blank and contains a `:` is treated as `key: value`; anything else
/// (a comment, a nested/multiline value) is silently skipped — this parser only ever looks for
/// the two flat scalars it needs.
fn parse_fields(lines: &[&str]) -> Result<(String, String), FrontmatterError> {
    let mut name: Option<String> = None;
    let mut description: Option<String> = None;

    for line in lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        match key.trim() {
            "name" => name = Some(unquote(value.trim())),
            "description" => description = Some(unquote(value.trim())),
            _ => {}
        }
    }

    let name = name.ok_or(FrontmatterError::MissingField("name"))?;
    let description = description.ok_or(FrontmatterError::MissingField("description"))?;
    validate_name(&name)?;
    validate_description(&description)?;
    Ok((name, description))
}

/// Strips one layer of matching `"..."`/`'...'` quoting, the only quoting form this parser
/// understands — no escape sequences.
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let (first, last) = (bytes[0], bytes[bytes.len() - 1]);
        if (first == b'"' && last == b'"') || (first == b'\'' && last == b'\'') {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

fn validate_name(name: &str) -> Result<(), FrontmatterError> {
    let valid = !name.is_empty()
        && name.len() <= MAX_NAME_LEN
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if valid {
        Ok(())
    } else {
        Err(FrontmatterError::InvalidName(name.to_string()))
    }
}

fn validate_description(description: &str) -> Result<(), FrontmatterError> {
    if description.is_empty() || description.chars().count() > MAX_DESCRIPTION_LEN {
        Err(FrontmatterError::InvalidDescription)
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_name_description_and_body() {
        let content = "---\nname: pdf-processing\ndescription: Extract text from PDFs.\n---\n\n# PDF Processing\n\nBody text.\n";
        let parsed = parse(content).unwrap();
        assert_eq!(parsed.name, "pdf-processing");
        assert_eq!(parsed.description, "Extract text from PDFs.");
        assert_eq!(parsed.body, "# PDF Processing\n\nBody text.");
    }

    #[test]
    fn strips_matching_quotes_around_scalar_values() {
        let content = "---\nname: \"pdf-processing\"\ndescription: 'Extract text.'\n---\nbody\n";
        let parsed = parse(content).unwrap();
        assert_eq!(parsed.name, "pdf-processing");
        assert_eq!(parsed.description, "Extract text.");
    }

    #[test]
    fn ignores_optional_fields_it_does_not_consume() {
        let content =
            "---\nname: a\ndescription: b\nlicense: MIT\nmetadata:\n  foo: bar\n---\nbody\n";
        let parsed = parse(content).unwrap();
        assert_eq!(parsed.name, "a");
        assert_eq!(parsed.description, "b");
    }

    #[test]
    fn rejects_content_missing_the_frontmatter_block_entirely() {
        let err = parse("# Just a heading\n\nno frontmatter here\n").unwrap_err();
        assert_eq!(err, FrontmatterError::NoFrontmatter);
    }

    #[test]
    fn rejects_an_unclosed_frontmatter_block() {
        let err = parse("---\nname: a\ndescription: b\nno closing delimiter\n").unwrap_err();
        assert_eq!(err, FrontmatterError::NoFrontmatter);
    }

    #[test]
    fn rejects_a_missing_required_field() {
        let err = parse("---\nname: a\n---\nbody\n").unwrap_err();
        assert_eq!(err, FrontmatterError::MissingField("description"));
    }

    #[test]
    fn rejects_a_name_with_uppercase_or_underscore_characters() {
        assert!(matches!(
            parse("---\nname: Bad_Name\ndescription: x\n---\n").unwrap_err(),
            FrontmatterError::InvalidName(_)
        ));
    }

    #[test]
    fn rejects_a_name_over_the_length_cap() {
        let long_name = "a".repeat(65);
        let content = format!("---\nname: {long_name}\ndescription: x\n---\n");
        assert!(matches!(
            parse(&content).unwrap_err(),
            FrontmatterError::InvalidName(_)
        ));
    }

    #[test]
    fn rejects_an_empty_description() {
        let err = parse("---\nname: a\ndescription: \n---\nbody\n").unwrap_err();
        assert_eq!(err, FrontmatterError::InvalidDescription);
    }

    #[test]
    fn a_body_with_no_trailing_content_is_an_empty_string_not_an_error() {
        let parsed = parse("---\nname: a\ndescription: b\n---\n").unwrap();
        assert_eq!(parsed.body, "");
    }
}
