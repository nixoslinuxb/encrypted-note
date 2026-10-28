//! Minimal YAML frontmatter parsing and note metadata (title, aliases, tags).
//!
//! Only the handful of frontmatter keys Obsidian users actually rely on are
//! supported: `title`, `aliases`, and `tags`. Inline `#tags` in the body are
//! collected too. This is deliberately a small, dependency-free subset of YAML.

/// Parsed frontmatter fields.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frontmatter {
    pub title: Option<String>,
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
}

/// Everything we extract from a note's text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Meta {
    pub frontmatter: Frontmatter,
    /// The note body with any frontmatter block removed.
    pub body: String,
    /// Title resolved from frontmatter (falls back to the caller's file stem).
    pub title: Option<String>,
    /// Aliases, from frontmatter.
    pub aliases: Vec<String>,
    /// All tags: frontmatter tags plus inline `#tags`, deduplicated, no `#`.
    pub tags: Vec<String>,
}

impl Meta {
    #[cfg(test)]
    pub fn has_frontmatter(&self) -> bool {
        self.frontmatter != Frontmatter::default()
    }
}

/// Parse a note's full text into [`Meta`].
pub fn parse(text: &str) -> Meta {
    let mut fm = Frontmatter::default();
    let body = split_frontmatter(text, &mut fm);
    let mut tags: Vec<String> = fm.tags.clone();
    for tag in inline_tags(body) {
        if !tags.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
            tags.push(tag);
        }
    }
    Meta {
        title: fm.title.clone(),
        aliases: fm.aliases.clone(),
        tags,
        body: body.to_string(),
        frontmatter: fm,
    }
}

/// Remove a leading `---` frontmatter block, parsing it into `fm`, and return
/// the remaining body.
fn split_frontmatter<'a>(text: &'a str, fm: &mut Frontmatter) -> &'a str {
    let rest = match text.strip_prefix("---\n") {
        Some(r) => r,
        None => match text.strip_prefix("---\r\n") {
            Some(r) => r,
            None => return text,
        },
    };
    // Find a closing fence line ("---" possibly with trailing spaces).
    let mut offset = 0usize;
    for line in rest.split_inclusive('\n') {
        if line.trim_end() == "---" {
            let yaml = &rest[..offset];
            parse_yaml(yaml, fm);
            return &rest[offset + line.len()..];
        }
        offset += line.len();
    }
    // No closing fence: treat the whole thing as body.
    text
}

fn parse_yaml(yaml: &str, fm: &mut Frontmatter) {
    let lines: Vec<&str> = yaml.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            i += 1;
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            i += 1;
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        let mut items: Vec<String> = Vec::new();
        if value.is_empty() {
            // Possibly a block list on the following indented `- ` lines.
            let mut j = i + 1;
            while j < lines.len() {
                let l = lines[j];
                if l.trim().is_empty() {
                    j += 1;
                    continue;
                }
                let lt = l.trim_start();
                if let Some(item) = lt.strip_prefix("- ") {
                    if l.len() == lt.len() {
                        // Not indented: belongs to the next key.
                        break;
                    }
                    items.push(unquote(item.trim()));
                    j += 1;
                } else {
                    break;
                }
            }
            i = j;
        } else {
            items = parse_list(value);
            i += 1;
        }
        match key.as_str() {
            "title" => {
                fm.title = if value.is_empty() {
                    items.into_iter().next()
                } else {
                    Some(unquote(value))
                };
            }
            "aliases" | "alias" => fm.aliases = items,
            "tags" | "tag" => fm.tags = items,
            _ => {}
        }
    }
}

/// Parse an inline YAML list: `[a, b]` or `a, b` or `a b`.
fn parse_list(value: &str) -> Vec<String> {
    let v = value.trim();
    if let Some(inner) = v.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
        // Comma-separated; keep spaces inside quoted entries.
        inner
            .split(',')
            .map(|s| unquote(s.trim()))
            .filter(|s| !s.is_empty())
            .collect()
    } else {
        v.split([',', ' '])
            .map(|s| unquote(s.trim()))
            .filter(|s| !s.is_empty())
            .collect()
    }
}

fn unquote(s: &str) -> String {
    let s = s.trim();
    let s = s
        .strip_prefix('"')
        .and_then(|x| x.strip_suffix('"'))
        .unwrap_or(s);
    let s = s
        .strip_prefix('\'')
        .and_then(|x| x.strip_suffix('\''))
        .unwrap_or(s);
    s.to_string()
}

/// Collect inline `#tags` from a note body, ignoring headings and code fences.
pub fn inline_tags(body: &str) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    let mut in_code = false;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        // A heading is `#` at line start followed by space.
        if trimmed.starts_with("# ") || trimmed == "#" {
            continue;
        }
        for tag in tags_in_line(line) {
            if !tags.iter().any(|t| t.eq_ignore_ascii_case(&tag)) {
                tags.push(tag);
            }
        }
    }
    tags
}

fn tags_in_line(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            let prev_ok = i == 0
                || matches!(
                    bytes[i - 1],
                    b' ' | b'\t' | b'(' | b'[' | b'{' | b'>' | b','
                );
            if prev_ok {
                let start = i + 1;
                let mut j = start;
                while j < bytes.len() {
                    let c = bytes[j];
                    if c.is_ascii_alphanumeric() || c == b'-' || c == b'_' || c == b'/' {
                        j += 1;
                    } else {
                        break;
                    }
                }
                if j > start {
                    out.push(line[start..j].to_string());
                    i = j;
                    continue;
                }
            }
        }
        i += 1;
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_inline_and_block_lists() {
        let text = "---\ntitle: My Note\naliases: [mn, \"my note\"]\ntags:\n  - project\n  - rust\n---\n# Body\n";
        let meta = parse(text);
        assert_eq!(meta.title.as_deref(), Some("My Note"));
        assert_eq!(meta.aliases, vec!["mn", "my note"]);
        assert_eq!(meta.tags, vec!["project", "rust"]);
        assert!(meta.body.starts_with("# Body"));
    }

    #[test]
    fn collects_inline_tags_and_skips_headings_and_code() {
        let text = "# Heading not a tag\n\nsome #work and #rust/async here\n\n```\n#notatag\n```\n";
        let meta = parse(text);
        assert_eq!(meta.tags, vec!["work", "rust/async"]);
    }

    #[test]
    fn merges_frontmatter_and_inline_tags() {
        let text = "---\ntags: [a]\n---\nbody #b\n";
        let meta = parse(text);
        assert_eq!(meta.tags, vec!["a", "b"]);
    }

    #[test]
    fn no_frontmatter_is_all_body() {
        let meta = parse("just text #tag\n");
        assert!(!meta.has_frontmatter());
        assert_eq!(meta.body, "just text #tag\n");
        assert_eq!(meta.tags, vec!["tag"]);
    }

    #[test]
    fn unterminated_frontmatter_is_body() {
        let meta = parse("---\ntitle: x\n");
        assert!(!meta.has_frontmatter());
    }

    #[test]
    fn url_fragments_are_not_tags() {
        let meta = parse("see https://example.com/page#section for details\n");
        assert!(meta.tags.is_empty());
    }
}
