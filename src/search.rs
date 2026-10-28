//! Search query language: plain terms, `"phrases"`, `tag:`, `path:`, `file:`,
//! `/regex/`, and `-` negation. All positive terms are ANDed.

use regex::Regex;

/// A document as seen by the search engine.
pub struct Doc<'a> {
    pub title: &'a str,
    pub path: &'a str,
    pub tags: &'a [String],
    pub content: &'a str,
}

enum Term {
    Text(String),
    Tag(String),
    Path(String),
    File(String),
    Regex(Regex),
}

pub struct Query {
    /// `(negated, term)` pairs, ANDed together.
    terms: Vec<(bool, Term)>,
}

impl Query {
    pub fn parse(input: &str) -> Query {
        let mut terms = Vec::new();
        for raw in tokenize(input) {
            let mut tok = raw.as_str();
            let mut negate = false;
            if let Some(rest) = tok.strip_prefix('-') {
                if !rest.is_empty() {
                    negate = true;
                    tok = rest;
                }
            }
            if tok.is_empty() {
                continue;
            }
            let term = if let Some(v) = tok.strip_prefix("tag:") {
                Term::Tag(v.to_string())
            } else if let Some(v) = tok.strip_prefix("path:") {
                Term::Path(v.to_string())
            } else if let Some(v) = tok.strip_prefix("file:") {
                Term::File(v.to_string())
            } else if tok.len() >= 2 && tok.starts_with('/') && tok.ends_with('/') {
                let pat = &tok[1..tok.len() - 1];
                match Regex::new(pat) {
                    Ok(re) => Term::Regex(re),
                    Err(_) => Term::Text(pat.to_string()),
                }
            } else {
                Term::Text(tok.to_string())
            };
            terms.push((negate, term));
        }
        Query { terms }
    }

    pub fn matches(&self, doc: &Doc) -> bool {
        self.terms.iter().all(|(neg, term)| {
            let m = term_matches(term, doc);
            if *neg {
                !m
            } else {
                m
            }
        })
    }

    /// First content line matching a positive plain/regex term, for a snippet.
    pub fn snippet(&self, doc: &Doc) -> Option<(usize, String)> {
        for (neg, term) in &self.terms {
            if *neg {
                continue;
            }
            match term {
                Term::Text(s) => {
                    if let Some(r) = find_line(doc.content, s, None) {
                        return Some(r);
                    }
                }
                Term::Regex(re) => {
                    if let Some(r) = find_line(doc.content, "", Some(re)) {
                        return Some(r);
                    }
                }
                _ => {}
            }
        }
        None
    }
}

fn term_matches(term: &Term, doc: &Doc) -> bool {
    match term {
        Term::Text(s) => {
            let s = s.to_lowercase();
            doc.content.to_lowercase().contains(&s) || doc.title.to_lowercase().contains(&s)
        }
        Term::Tag(s) => {
            let s = s.to_lowercase();
            doc.tags.iter().any(|tag| {
                let tl = tag.to_lowercase();
                tl == s || tl.starts_with(&format!("{s}/"))
            })
        }
        Term::Path(s) => doc.path.to_lowercase().contains(&s.to_lowercase()),
        Term::File(s) => doc.title.to_lowercase().contains(&s.to_lowercase()),
        Term::Regex(re) => re.is_match(doc.content) || re.is_match(doc.title),
    }
}

fn find_line(content: &str, needle: &str, re: Option<&Regex>) -> Option<(usize, String)> {
    let needle = needle.to_lowercase();
    for (i, line) in content.lines().enumerate() {
        let hit = match re {
            Some(r) => r.is_match(line),
            None => line.to_lowercase().contains(&needle),
        };
        if hit {
            let snippet: String = line.trim().chars().take(100).collect();
            return Some((i + 1, snippet));
        }
    }
    None
}

/// Split a query into tokens, keeping `"quoted phrases"` and `/regex/` intact.
fn tokenize(input: &str) -> Vec<String> {
    let chars: Vec<char> = input.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '"' {
            i += 1;
            let mut s = String::new();
            while i < chars.len() && chars[i] != '"' {
                s.push(chars[i]);
                i += 1;
            }
            i += 1; // closing quote (or end)
            if !s.is_empty() {
                out.push(s);
            }
            continue;
        }
        if c == '/' {
            let mut j = i + 1;
            let mut s = String::from("/");
            while j < chars.len() && chars[j] != '/' {
                s.push(chars[j]);
                j += 1;
            }
            if j < chars.len() {
                s.push('/');
                j += 1;
                out.push(s);
                i = j;
                continue;
            }
            // No closing slash: fall through and treat as a normal token.
        }
        let mut s = String::new();
        while i < chars.len() && !chars[i].is_whitespace() {
            s.push(chars[i]);
            i += 1;
        }
        if !s.is_empty() {
            out.push(s);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc<'a>(title: &'a str, path: &'a str, tags: &'a [String], content: &'a str) -> Doc<'a> {
        Doc {
            title,
            path,
            tags,
            content,
        }
    }

    #[test]
    fn tokenizes_phrases_and_regex() {
        assert_eq!(
            tokenize(r#"foo "bar baz" /a.c/ qux"#),
            vec!["foo", "bar baz", "/a.c/", "qux"]
        );
    }

    #[test]
    fn plain_terms_are_anded() {
        let re = vec!["work".to_string()];
        let q = Query::parse("alpha beta");
        assert!(q.matches(&doc("Alpha", "/v/a.md", &re, "alpha and beta")));
        assert!(!q.matches(&doc("Alpha", "/v/a.md", &re, "only alpha")));
    }

    #[test]
    fn tag_path_file_operators() {
        let tags = vec!["work/rust".to_string()];
        let d = doc("Rust Notes", "/v/notes/rust.md", &tags, "x");
        assert!(Query::parse("tag:work").matches(&d)); // nested tag prefix
        assert!(Query::parse("tag:work/rust").matches(&d));
        assert!(Query::parse("path:notes").matches(&d));
        assert!(Query::parse("file:rust").matches(&d));
        assert!(!Query::parse("tag:home").matches(&d));
    }

    #[test]
    fn negation() {
        let tags: Vec<String> = vec![];
        let d = doc("A", "/v/a.md", &tags, "hello world");
        assert!(Query::parse("hello -goodbye").matches(&d));
        assert!(!Query::parse("hello -world").matches(&d));
    }

    #[test]
    fn regex_terms() {
        let tags: Vec<String> = vec![];
        let d = doc("A", "/v/a.md", &tags, "color and colour");
        assert!(Query::parse("/colou?r/").matches(&d));
        assert!(!Query::parse("/xyz/").matches(&d));
    }

    #[test]
    fn invalid_regex_falls_back_to_text() {
        let tags: Vec<String> = vec![];
        let d = doc("A", "/v/a.md", &tags, "a(b text");
        // `/a(b/` is not a valid regex, so it becomes a literal search.
        assert!(Query::parse("/a(b/").matches(&d));
    }

    #[test]
    fn snippet_finds_the_matching_line() {
        let tags: Vec<String> = vec![];
        let d = doc("A", "/v/a.md", &tags, "line one\nfind me here\nline three");
        let (n, line) = Query::parse("find").snippet(&d).unwrap();
        assert_eq!(n, 2);
        assert!(line.contains("find me here"));
    }
}
