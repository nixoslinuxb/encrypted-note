//! A small built-in RSS/Atom reader.
//!
//! * `parse` turns RSS 2.0 / RDF / Atom XML into [`Item`]s (using `roxmltree`,
//!   already in our dependency tree).
//! * `fetch` downloads a feed with the system `curl` (no HTTP crate needed) and
//!   parses it.
//! * [`Store`] persists feeds, categories, and cached items to
//!   `$XDG_CONFIG_HOME/encrypted-note/feeds.json`.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Item {
    pub title: String,
    pub link: String,
    #[serde(default)]
    pub date: String,
    /// Plain-text body (HTML stripped, paragraphs preserved).
    #[serde(default)]
    pub text: String,
    /// Fetched full article text, if the user loaded it.
    #[serde(default)]
    pub full: String,
    /// Lead image URL, if any.
    #[serde(default)]
    pub image: String,
    /// Media enclosure URL (audio/video).
    #[serde(default)]
    pub enclosure: String,
    /// `"video"`, `"audio"`, or empty.
    #[serde(default)]
    pub kind: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Feed {
    #[serde(default)]
    pub id: u32,
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub items: Vec<Item>,
    #[serde(default)]
    pub error: String,
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct Store {
    #[serde(default)]
    pub feeds: Vec<Feed>,
    #[serde(default)]
    pub next_id: u32,
    /// Articles the user saved (bookmarked).
    #[serde(default)]
    pub saved: Vec<Item>,
}

impl Store {
    pub fn path() -> PathBuf {
        crate::config::dir().join("feeds.json")
    }

    pub fn load() -> Store {
        std::fs::read_to_string(Self::path())
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        if let Ok(json) = serde_json::to_string_pretty(self) {
            if let Some(parent) = Self::path().parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::write(Self::path(), json);
        }
    }

    pub fn add(&mut self, url: &str, title: &str, category: &str) -> u32 {
        self.next_id += 1;
        let id = self.next_id;
        self.feeds.push(Feed {
            id,
            url: url.to_string(),
            title: title.to_string(),
            category: category.to_string(),
            items: Vec::new(),
            error: String::new(),
        });
        id
    }

    pub fn find(&self, id: u32) -> Option<&Feed> {
        self.feeds.iter().find(|f| f.id == id)
    }

    pub fn find_mut(&mut self, id: u32) -> Option<&mut Feed> {
        self.feeds.iter_mut().find(|f| f.id == id)
    }

    pub fn remove(&mut self, id: u32) {
        self.feeds.retain(|f| f.id != id);
    }

    /// All distinct categories, sorted.
    pub fn categories(&self) -> Vec<String> {
        let mut cats: Vec<String> = self
            .feeds
            .iter()
            .map(|f| f.category.clone())
            .filter(|c| !c.trim().is_empty())
            .collect();
        cats.sort();
        cats.dedup();
        cats
    }

    /// Whether an article is saved (matched by link, else title).
    pub fn is_saved(&self, item: &Item) -> bool {
        self.saved.iter().any(|s| same_article(s, item))
    }

    /// Add or remove an article from the saved list.
    pub fn toggle_saved(&mut self, item: &Item) {
        if let Some(pos) = self.saved.iter().position(|s| same_article(s, item)) {
            self.saved.remove(pos);
        } else {
            self.saved.insert(0, item.clone());
        }
    }
}

fn same_article(a: &Item, b: &Item) -> bool {
    if !a.link.is_empty() || !b.link.is_empty() {
        a.link == b.link
    } else {
        a.title == b.title
    }
}

/// Sortable key (Unix seconds) for a feed date string; `0` if unknown.
pub fn date_key(s: &str) -> i64 {
    parse_date(s).unwrap_or(0)
}

fn parse_date(s: &str) -> Option<i64> {
    let s = s.trim();
    let b = s.as_bytes();
    // ISO 8601: YYYY-MM-DD[THH:MM:SS]
    if s.len() >= 10 && b[4] == b'-' && b[7] == b'-' {
        let y: i32 = s[0..4].parse().ok()?;
        let m: u32 = s[5..7].parse().ok()?;
        let d: u32 = s[8..10].parse().ok()?;
        let mut secs = crate::dates::days_from_civil(y, m, d) * 86_400;
        if s.len() >= 19 && (b[10] == b'T' || b[10] == b' ') {
            let h: i64 = s[11..13].parse().ok()?;
            let mi: i64 = s[14..16].parse().ok()?;
            let se: i64 = s[17..19].parse().ok()?;
            secs += h * 3600 + mi * 60 + se;
        }
        return Some(secs);
    }
    // RFC 822: "Mon, 01 Jan 2024 00:00:00 GMT"
    let mut day = None;
    let mut mon = None;
    let mut year = None;
    let mut time: Option<String> = None;
    for p in s.split_whitespace() {
        let p = p.trim_end_matches(',');
        if mon.is_none() {
            if let Some(mi) = month_index(p) {
                mon = Some(mi);
                continue;
            }
        }
        if year.is_none() && p.len() == 4 && p.chars().all(|c| c.is_ascii_digit()) {
            year = p.parse::<i32>().ok();
            continue;
        }
        if day.is_none() && !p.is_empty() && p.len() <= 2 && p.chars().all(|c| c.is_ascii_digit()) {
            day = p.parse::<u32>().ok();
            continue;
        }
        if time.is_none() && p.contains(':') {
            time = Some(p.to_string());
        }
    }
    let mut secs = crate::dates::days_from_civil(year?, mon?, day?) * 86_400;
    if let Some(t) = time {
        let mut it = t.split(':');
        let h: i64 = it.next()?.parse().ok()?;
        let mi: i64 = it.next().unwrap_or("0").parse().unwrap_or(0);
        let se: i64 = it.next().unwrap_or("0").parse().unwrap_or(0);
        secs += h * 3600 + mi * 60 + se;
    }
    Some(secs)
}

fn month_index(m: &str) -> Option<u32> {
    Some(match m.to_ascii_lowercase().as_str() {
        "jan" => 1,
        "feb" => 2,
        "mar" => 3,
        "apr" => 4,
        "may" => 5,
        "jun" => 6,
        "jul" => 7,
        "aug" => 8,
        "sep" => 9,
        "oct" => 10,
        "nov" => 11,
        "dec" => 12,
        _ => return None,
    })
}

/// Download a URL with the system `curl` and return the body.
///
/// Hardened: only `http(s)` is allowed; `-q` ignores any local `.curlrc`; the
/// protocol set is restricted for both the initial request and redirects (so a
/// malicious server cannot redirect to `file://` and exfiltrate local files);
/// and `--` terminates options so `url` can never be interpreted as a curl flag.
pub fn http_get(url: &str) -> Result<Vec<u8>, String> {
    let url = url.trim();
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err("only http:// and https:// URLs are allowed".into());
    }
    let out = std::process::Command::new("curl")
        .args([
            "-q",
            "-sL",
            "--max-time",
            "25",
            "--proto",
            "=http,https",
            "--proto-redir",
            "=http,https",
            "--max-redirs",
            "5",
            "-A",
            "EncryptedNote/0.1 (RSS reader)",
            "--",
            url,
        ])
        .output()
        .map_err(|e| format!("curl: {e}"))?;
    if !out.status.success() {
        return Err(format!("curl exited with {}", out.status));
    }
    if out.stdout.is_empty() {
        return Err("empty response".into());
    }
    Ok(out.stdout)
}

/// Fetch and parse a feed, returning `(title, items)`.
pub fn fetch(url: &str) -> Result<(String, Vec<Item>), String> {
    let bytes = http_get(url)?;
    let xml = String::from_utf8_lossy(&bytes);
    parse(&xml)
}

/// Fetch an article page and extract its readable text.
pub fn fetch_article(url: &str) -> Result<String, String> {
    let bytes = http_get(url)?;
    let html = String::from_utf8_lossy(&bytes);
    let text = extract_readable(&html);
    if text.trim().is_empty() {
        Err("no readable text found".into())
    } else {
        Ok(text)
    }
}

/// Extract the main readable text from an HTML document.
pub fn extract_readable(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let region = slice_element(html, &lower, "article")
        .or_else(|| slice_element(html, &lower, "main"))
        .unwrap_or_else(|| html.to_string());
    let no_script = remove_element(&region, "script");
    let no_style = remove_element(&no_script, "style");
    strip_html(&no_style)
}

/// The slice covering `<tag ...>…</tag>` (case-insensitive), if present.
fn slice_element(html: &str, lower: &str, tag: &str) -> Option<String> {
    let open = lower.find(&format!("<{tag}"))?;
    let close_pat = format!("</{tag}>");
    let close = lower[open..].find(&close_pat)? + open + close_pat.len();
    Some(html[open..close].to_string())
}

fn remove_element(html: &str, tag: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let open_pat = format!("<{tag}");
    let close_pat = format!("</{tag}>");
    let mut out = String::with_capacity(html.len());
    let mut i = 0;
    while let Some(rel) = lower[i..].find(&open_pat) {
        let start = i + rel;
        out.push_str(&html[i..start]);
        match lower[start..].find(&close_pat) {
            Some(crel) => i = start + crel + close_pat.len(),
            None => return out,
        }
    }
    out.push_str(&html[i..]);
    out
}

/// Parse RSS 2.0, RSS 1.0 (RDF), or Atom into `(title, items)` (newest first).
pub fn parse(xml: &str) -> Result<(String, Vec<Item>), String> {
    let doc = roxmltree::Document::parse(xml).map_err(|e| format!("XML: {e}"))?;
    let root = doc.root_element();
    let (title, mut items) = match root.tag_name().name() {
        "rss" | "RDF" => parse_rss(&doc),
        "feed" => parse_atom(&doc),
        other => return Err(format!("unsupported feed root <{other}>")),
    };
    // Newest first (unknown dates keep their relative order at the end).
    items.sort_by_key(|it| std::cmp::Reverse(date_key(&it.date)));
    Ok((title, items))
}

fn child_text(node: roxmltree::Node, local: &str) -> Option<String> {
    node.children()
        .find(|c| c.is_element() && c.tag_name().name() == local)
        .and_then(|c| c.text())
        .map(|t| t.trim().to_string())
}

fn first_desc_text(node: roxmltree::Node, local: &str) -> Option<String> {
    node.descendants()
        .find(|c| c.is_element() && c.tag_name().name() == local)
        .and_then(|c| c.text())
        .map(|t| t.trim().to_string())
}

fn parse_rss(doc: &roxmltree::Document) -> (String, Vec<Item>) {
    let root = doc.root_element();
    let channel = root
        .descendants()
        .find(|n| n.is_element() && n.tag_name().name() == "channel")
        .unwrap_or(root);
    let title = child_text(channel, "title").unwrap_or_default();
    let mut items = Vec::new();
    for item in channel
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "item")
    {
        items.push(build_item(item));
    }
    (title, items)
}

fn parse_atom(doc: &roxmltree::Document) -> (String, Vec<Item>) {
    let root = doc.root_element();
    let title = child_text(root, "title").unwrap_or_default();
    let mut items = Vec::new();
    for entry in root
        .descendants()
        .filter(|n| n.is_element() && n.tag_name().name() == "entry")
    {
        items.push(build_item(entry));
    }
    (title, items)
}

fn build_item(node: roxmltree::Node) -> Item {
    let title = child_text(node, "title").unwrap_or_else(|| "(untitled)".into());
    let date = child_text(node, "pubDate")
        .or_else(|| child_text(node, "published"))
        .or_else(|| child_text(node, "updated"))
        .or_else(|| child_text(node, "dc:date"))
        .unwrap_or_default();
    let raw = child_text(node, "encoded")
        .or_else(|| first_desc_text(node, "content"))
        .or_else(|| child_text(node, "description"))
        .or_else(|| child_text(node, "summary"))
        .unwrap_or_default();

    // Link (RSS: <link>text</link>; Atom: <link href="..."/>).
    let link = child_text(node, "link")
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            node.children()
                .find(|c| c.is_element() && c.tag_name().name() == "link")
                .and_then(|c| c.attribute("href"))
                .unwrap_or("")
                .to_string()
        });

    // Media / enclosure.
    let mut enclosure = String::new();
    let mut kind = String::new();
    let mut image = first_img_src(&raw);
    for c in node.children().filter(|c| c.is_element()) {
        match c.tag_name().name() {
            "enclosure" | "content" => {
                if let Some(u) = c.attribute("url") {
                    let ty = c.attribute("type").unwrap_or("");
                    if enclosure.is_empty() {
                        enclosure = u.to_string();
                        kind = media_kind(ty, u);
                    }
                }
            }
            "thumbnail" => {
                if let Some(u) = c.attribute("url").filter(|_| image.is_empty()) {
                    image = u.to_string();
                }
            }
            _ => {}
        }
    }
    if !enclosure.is_empty() && kind.is_empty() {
        kind = media_kind("", &enclosure);
    }

    Item {
        title,
        link,
        date,
        text: strip_html(&raw),
        full: String::new(),
        image,
        enclosure,
        kind,
    }
}

fn media_kind(content_type: &str, url: &str) -> String {
    if content_type.starts_with("video/") {
        "video".into()
    } else if content_type.starts_with("audio/") {
        "audio".into()
    } else {
        let lower = url.to_ascii_lowercase();
        if [".mp4", ".webm", ".mkv", ".mov"]
            .iter()
            .any(|e| lower.ends_with(e) || lower.contains(e))
        {
            "video".into()
        } else if [".mp3", ".ogg", ".m4a", ".wav"]
            .iter()
            .any(|e| lower.ends_with(e))
        {
            "audio".into()
        } else {
            String::new()
        }
    }
}

/// First `<img src="...">` inside an HTML fragment.
fn first_img_src(html: &str) -> String {
    let lower = html.to_ascii_lowercase();
    let mut search = 0;
    while let Some(pos) = lower[search..].find("<img") {
        let start = search + pos;
        let end = lower[start..]
            .find('>')
            .map(|e| start + e)
            .unwrap_or(lower.len());
        let tag = &html[start..end];
        if let Some(src) = attr_value(tag, "src") {
            if !src.is_empty() {
                return src;
            }
        }
        search = end.max(start + 1);
    }
    String::new()
}

fn attr_value(tag: &str, attr: &str) -> Option<String> {
    let lower = tag.to_ascii_lowercase();
    let needle = format!("{attr}=");
    let pos = lower.find(&needle)? + needle.len();
    let rest = tag[pos..].trim_start();
    let quote = rest.chars().next()?;
    if quote == '"' || quote == '\'' {
        let rest = &rest[1..];
        let end = rest.find(quote)?;
        Some(rest[..end].to_string())
    } else {
        let end = rest
            .find(|c: char| c.is_whitespace() || c == '>')
            .unwrap_or(rest.len());
        Some(rest[..end].to_string())
    }
}

/// Block-level HTML tags become line breaks when stripping.
fn is_block_tag(name: &str) -> bool {
    matches!(
        name,
        "p" | "div"
            | "br"
            | "li"
            | "ul"
            | "ol"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "tr"
            | "table"
            | "blockquote"
            | "section"
            | "article"
            | "header"
            | "footer"
            | "pre"
            | "hr"
            | "figure"
            | "figcaption"
    )
}

/// Strip HTML tags, preserving paragraph breaks, and decode common entities.
pub fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len());
    let chars: Vec<char> = html.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '<' => {
                // Read the tag name.
                let mut j = i + 1;
                if j < chars.len() && chars[j] == '/' {
                    j += 1;
                }
                let name_start = j;
                while j < chars.len() && (chars[j].is_ascii_alphanumeric()) {
                    j += 1;
                }
                let name: String = chars[name_start..j]
                    .iter()
                    .collect::<String>()
                    .to_ascii_lowercase();
                if is_block_tag(&name) {
                    out.push('\n');
                }
                // Advance past this tag.
                while i < chars.len() && chars[i] != '>' {
                    i += 1;
                }
                let self_closing = i > 0 && chars.get(i - 1) == Some(&'/');
                if i < chars.len() {
                    i += 1;
                }
                // Drop the raw content of script/style/noscript/template tags
                // entirely (otherwise inline JS such as `(adsbygoogle=…).push({})`
                // leaks into the article text).
                if !self_closing
                    && matches!(name.as_str(), "script" | "style" | "noscript" | "template")
                {
                    let close: Vec<char> = format!("</{name}").chars().collect();
                    let mut k = i;
                    while k + close.len() <= chars.len() {
                        if chars[k..k + close.len()]
                            .iter()
                            .zip(close.iter())
                            .all(|(a, b)| a.eq_ignore_ascii_case(b))
                        {
                            let mut m = k;
                            while m < chars.len() && chars[m] != '>' {
                                m += 1;
                            }
                            i = if m < chars.len() { m + 1 } else { chars.len() };
                            break;
                        }
                        k += 1;
                    }
                }
            }
            '&' => {
                let mut j = i + 1;
                let mut ent = String::new();
                while j < chars.len() && chars[j] != ';' && ent.len() < 12 {
                    ent.push(chars[j]);
                    j += 1;
                }
                if j < chars.len() && chars[j] == ';' && !ent.is_empty() {
                    match entity_value(&ent) {
                        Some(c) => out.push(c),
                        None => {
                            out.push('&');
                            out.push_str(&ent);
                            out.push(';');
                        }
                    }
                    i = j + 1;
                } else {
                    out.push('&');
                    i += 1;
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    decode_entities(&normalize(&out))
}

/// Decode an HTML entity name/number (`amp`, `#8221`, `#x2019`, `rdquo`, …).
fn entity_value(ent: &str) -> Option<char> {
    if let Some(num) = ent.strip_prefix('#') {
        let code = if let Some(hex) = num.strip_prefix('x').or_else(|| num.strip_prefix('X')) {
            u32::from_str_radix(hex, 16).ok()?
        } else {
            num.parse::<u32>().ok()?
        };
        return char::from_u32(code);
    }
    Some(match ent {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" | "ensp" | "emsp" | "thinsp" => ' ',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "sbquo" => '‚',
        "bdquo" => '„',
        "laquo" => '«',
        "raquo" => '»',
        "lsaquo" => '‹',
        "rsaquo" => '›',
        "middot" => '·',
        "bull" => '•',
        "deg" => '°',
        "plusmn" => '±',
        "times" => '×',
        "divide" => '÷',
        "frac12" => '½',
        "frac14" => '¼',
        "frac34" => '¾',
        "sup1" => '¹',
        "sup2" => '²',
        "sup3" => '³',
        "micro" => 'µ',
        "para" => '¶',
        "sect" => '§',
        "cent" => '¢',
        "pound" => '£',
        "euro" => '€',
        "yen" => '¥',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "dagger" => '†',
        "Dagger" => '‡',
        "permil" => '‰',
        "prime" => '′',
        "Prime" => '″',
        "iexcl" => '¡',
        "iquest" => '¿',
        "shy" => '\u{00AD}',
        "macr" => '¯',
        "acute" => '´',
        "cedil" => '¸',
        "uml" => '¨',
        "ordf" => 'ª',
        "ordm" => 'º',
        "not" => '¬',
        _ => return None,
    })
}

/// Decode any remaining entities (e.g. double-encoded `&amp;#8221;`).
fn decode_entities(s: &str) -> String {
    if !s.contains('&') {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < s.len() {
        if bytes[i] == b'&' {
            if let Some(rel) = s[i + 1..].find(';') {
                let semi = i + 1 + rel;
                let ent = &s[i + 1..semi];
                if !ent.is_empty()
                    && ent.len() <= 12
                    && !ent.contains('&')
                    && !ent.chars().any(char::is_whitespace)
                {
                    if let Some(c) = entity_value(ent) {
                        out.push(c);
                        i = semi + 1;
                        continue;
                    }
                }
            }
        }
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    out
}

/// Whether a line is JavaScript / advertising machinery rather than prose.
fn is_junk_line(line: &str) -> bool {
    let l = line.trim();
    if l.is_empty() {
        return false;
    }
    let low = l.to_ascii_lowercase();
    const MARKERS: &[&str] = &[
        "adsbygoogle",
        "googletag",
        "doubleclick",
        "google_ad",
        "googlesyndication",
        "window.__",
        "document.queryselector",
        "tcfapi",
        "_taboola",
        "outbrain",
        "addthis",
        "sharethis",
    ];
    if MARKERS.iter().any(|m| low.contains(m)) {
        return true;
    }
    // A bare JS statement/expression like `(window.x || []).push({});`.
    if (l.starts_with('(') || l.starts_with("var ") || l.starts_with("function"))
        && (l.ends_with("});") || l.ends_with("})();") || l.ends_with(";"))
    {
        return true;
    }
    // Pure punctuation / brace noise.
    if l.chars()
        .all(|c| c.is_whitespace() || "(){}[];,.|&=%+-*/\\".contains(c))
    {
        return true;
    }
    // Common ad / boilerplate labels.
    matches!(
        low.as_str(),
        "advertisement" | "advertisements" | "reklam" | "sponsored" | "sponsored content"
    )
}

/// Trim each line, collapse runs of spaces, and keep at most one blank line.
fn normalize(raw: &str) -> String {
    let mut lines: Vec<String> = raw
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|l| !is_junk_line(l))
        .collect();
    while lines.first().is_some_and(|l| l.is_empty()) {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut prev_blank = false;
    for line in lines {
        if line.is_empty() {
            if !prev_blank {
                out.push(String::new());
            }
            prev_blank = true;
        } else {
            out.push(line);
            prev_blank = false;
        }
    }
    out.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_rss_2_0() {
        let xml = r#"<?xml version="1.0"?>
        <rss version="2.0" xmlns:media="http://search.yahoo.com/mrss/">
          <channel>
            <title>My Blog</title>
            <item>
              <title>Hello</title>
              <link>https://example.com/1</link>
              <pubDate>Mon, 01 Jan 2024 00:00:00 GMT</pubDate>
              <description><![CDATA[<p>Some <b>text</b> <img src="https://example.com/a.png"/></p>]]></description>
              <enclosure url="https://example.com/v.mp4" type="video/mp4"/>
              <media:thumbnail url="https://example.com/thumb.jpg"/>
            </item>
          </channel>
        </rss>"#;
        let (title, items) = parse(xml).unwrap();
        assert_eq!(title, "My Blog");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Hello");
        assert_eq!(items[0].link, "https://example.com/1");
        assert_eq!(items[0].text, "Some text");
        assert_eq!(items[0].image, "https://example.com/a.png");
        assert_eq!(items[0].enclosure, "https://example.com/v.mp4");
        assert_eq!(items[0].kind, "video");
    }

    #[test]
    fn parses_atom() {
        let xml = r#"<?xml version="1.0"?>
        <feed xmlns="http://www.w3.org/2005/Atom">
          <title>Atom Feed</title>
          <entry>
            <title>Post</title>
            <link href="https://example.com/post"/>
            <updated>2024-01-01T00:00:00Z</updated>
            <content>Body &amp; more</content>
          </entry>
        </feed>"#;
        let (title, items) = parse(xml).unwrap();
        assert_eq!(title, "Atom Feed");
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].title, "Post");
        assert_eq!(items[0].link, "https://example.com/post");
        assert_eq!(items[0].text, "Body & more");
    }

    #[test]
    fn strips_html_and_entities() {
        assert_eq!(strip_html("<p>a &amp; b</p>\n<p>c</p>"), "a & b\n\nc");
        assert_eq!(strip_html("plain"), "plain");
        assert_eq!(strip_html("<p>x</p><br><p>y</p>"), "x\n\ny");
    }

    #[test]
    fn extracts_readable_article() {
        let html = r#"<html><head><style>.a{}</style><script>var x=1;</script></head>
            <body><nav>menu</nav><article><h1>Title</h1>
            <p>First paragraph.</p><p>Second &amp; last.</p></article></body></html>"#;
        let text = extract_readable(html);
        assert!(text.contains("First paragraph."));
        assert!(text.contains("Second & last."));
        assert!(!text.contains("var x"));
        assert!(!text.contains(".a{"));
        assert!(!text.contains("menu"));
        assert!(text.contains("\n\n"));
    }

    #[test]
    fn decodes_numeric_and_named_entities() {
        // Numeric entities (including the right double quote) are decoded.
        assert_eq!(strip_html("say &#8221;hi&#8221;"), "say ”hi”");
        assert_eq!(strip_html("a &#x2019; b"), "a ’ b");
        // Double-encoded entities are decoded too.
        assert!(!strip_html("&amp;#8221;").contains("&#"));
    }

    #[test]
    fn strips_script_and_style_contents() {
        let html =
            "<p>Hello</p><script>(adsbygoogle = window.adsbygoogle || []).push({});</script>\
                    <style>.x{color:red}</style><p>World</p>";
        let text = strip_html(html);
        assert!(text.contains("Hello"));
        assert!(text.contains("World"));
        assert!(!text.contains("adsbygoogle"));
        assert!(!text.contains("color:red"));
    }

    #[test]
    fn drops_ad_junk_lines() {
        let html = "<p>Real paragraph.</p>(window.googletag || []).push({});<p>More text.</p>";
        let text = strip_html(html);
        assert!(text.contains("Real paragraph."));
        assert!(text.contains("More text."));
        assert!(!text.contains("googletag"));
    }

    #[test]
    fn parses_dates_and_sorts_newest_first() {
        assert!(date_key("2024-01-02T03:00:00Z") > date_key("2024-01-01T00:00:00Z"));
        assert!(date_key("Mon, 01 Jan 2024 00:00:00 GMT") > 0);
        assert_eq!(date_key("nonsense"), 0);

        let xml = r#"<rss><channel><title>t</title>
            <item><title>old</title><pubDate>Mon, 01 Jan 2024 00:00:00 GMT</pubDate></item>
            <item><title>new</title><pubDate>Wed, 03 Jan 2024 00:00:00 GMT</pubDate></item>
        </channel></rss>"#;
        let (_, items) = parse(xml).unwrap();
        assert_eq!(items[0].title, "new");
        assert_eq!(items[1].title, "old");
    }

    #[test]
    fn finds_first_image() {
        assert_eq!(
            first_img_src(r#"<p>x</p><img alt="y" src='http://i/s.png'>"#),
            "http://i/s.png"
        );
        assert_eq!(first_img_src("<p>no image</p>"), "");
    }
}
