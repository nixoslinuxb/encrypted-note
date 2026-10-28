//! A small, dependency-free markdown renderer for `egui`.
//!
//! Supported: headings, bold/italic/strikethrough/inline-code, bullet and
//! numbered lists, task-list checkboxes, blockquotes, callouts (`> [!note]`),
//! fenced code blocks, GitHub-style tables, footnotes, horizontal rules,
//! markdown links `[text](url)`, Obsidian `[[wikilinks]]` (incl. `[[t|alias]]`),
//! and `![[embeds]]`.

use eframe::egui::{self, Color32, RichText, Sense, Vec2};

/// A clickable thing the preview produced this frame.
pub enum LinkClick {
    /// A `[[wikilink]]` target (to be resolved to a note).
    Wiki(String),
    /// An external `[text](url)` link.
    External(String),
}

/// One styled run within a line of markdown.
enum Inline {
    Text {
        text: String,
        bold: bool,
        italic: bool,
        strike: bool,
        code: bool,
    },
    Wiki {
        target: String,
        alias: String,
    },
    External {
        text: String,
        url: String,
    },
    Footnote(String),
    Math(String),
}

/// Extract all wikilink targets appearing in a block of markdown.
pub fn wikilinks(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find("[[") {
        let after = &rest[start + 2..];
        match after.find("]]") {
            Some(end) => {
                let inner = &after[..end];
                let target = inner.split('|').next().unwrap_or(inner).trim();
                if !target.is_empty() {
                    out.push(target.to_string());
                }
                rest = &after[end + 2..];
            }
            None => break,
        }
    }
    out
}

/// Collect document headings as `(level, text)` pairs.
pub fn headings(text: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut in_code = false;
    for line in text.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }
        let hashes = trimmed.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) {
            let rest = &trimmed[hashes..];
            if rest.is_empty() || rest.starts_with(' ') {
                out.push((hashes, rest.trim().to_string()));
            }
        }
    }
    out
}

/// Parse a single line into styled inline runs.
fn parse_inline(line: &str) -> Vec<Inline> {
    let mut out: Vec<Inline> = Vec::new();
    let mut buf = String::new();
    let mut bold = false;
    let mut italic = false;
    let mut strike = false;
    let mut code = false;

    macro_rules! flush {
        () => {
            if !buf.is_empty() {
                out.push(Inline::Text {
                    text: std::mem::take(&mut buf),
                    bold,
                    italic,
                    strike,
                    code,
                });
            }
        };
    }

    let chars: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        // Wikilink: [[target]] or [[target|alias]]
        if c == '[' && i + 1 < chars.len() && chars[i + 1] == '[' {
            if let Some(close) = find_close(&chars, i) {
                let inner: String = chars[i + 2..close].iter().collect();
                let mut split = inner.splitn(2, '|');
                let target = split.next().unwrap_or("").trim().to_string();
                let alias = split
                    .next()
                    .map(|a| a.trim().to_string())
                    .unwrap_or_else(|| target.clone());
                if !target.is_empty() {
                    flush!();
                    out.push(Inline::Wiki { target, alias });
                    i = close + 2;
                    continue;
                }
            }
        }
        // Footnote reference: [^id]
        if c == '[' && i + 1 < chars.len() && chars[i + 1] == '^' {
            if let Some(close) = chars[i + 2..].iter().position(|&x| x == ']') {
                let id: String = chars[i + 2..i + 2 + close].iter().collect();
                flush!();
                out.push(Inline::Footnote(id));
                i = i + 2 + close + 1;
                continue;
            }
        }
        // Markdown link: [text](url)
        if c == '[' {
            if let Some((text, url, next)) = parse_md_link(&chars, i) {
                flush!();
                out.push(Inline::External { text, url });
                i = next;
                continue;
            }
        }
        // Inline math: $...$
        if c == '$' {
            if let Some(close) = chars[i + 1..].iter().position(|&x| x == '$') {
                let inner: String = chars[i + 1..i + 1 + close].iter().collect();
                if !inner.is_empty() && !inner.starts_with(' ') {
                    flush!();
                    out.push(Inline::Math(inner));
                    i = i + 1 + close + 1;
                    continue;
                }
            }
        }
        match c {
            '`' => {
                flush!();
                code = !code;
                i += 1;
            }
            '~' if i + 1 < chars.len() && chars[i + 1] == '~' => {
                flush!();
                strike = !strike;
                i += 2;
            }
            '*' => {
                if i + 1 < chars.len() && chars[i + 1] == '*' {
                    flush!();
                    bold = !bold;
                    i += 2;
                } else {
                    flush!();
                    italic = !italic;
                    i += 1;
                }
            }
            '_' => {
                let prev_word = i > 0 && chars[i - 1].is_alphanumeric();
                let next_word = i + 1 < chars.len() && chars[i + 1].is_alphanumeric();
                if !(prev_word && next_word) {
                    flush!();
                    italic = !italic;
                    i += 1;
                } else {
                    buf.push(c);
                    i += 1;
                }
            }
            _ => {
                buf.push(c);
                i += 1;
            }
        }
    }
    flush!();
    out
}

/// Try to parse `[text](url)` starting at `start` (which must point at `[`).
fn parse_md_link(chars: &[char], start: usize) -> Option<(String, String, usize)> {
    let close = chars[start..].iter().position(|&c| c == ']')? + start;
    if chars.get(close + 1) != Some(&'(') {
        return None;
    }
    let end = chars[close + 1..].iter().position(|&c| c == ')')? + close + 1;
    let text: String = chars[start + 1..close].iter().collect();
    let url: String = chars[close + 2..end].iter().collect();
    if url.contains(' ') || url.is_empty() {
        // Not a real link (e.g. `[a] (b)` with a space).
        return None;
    }
    Some((text, url, end + 1))
}

fn find_close(chars: &[char], start: usize) -> Option<usize> {
    let mut i = start + 2;
    while i + 1 < chars.len() {
        if chars[i] == ']' && chars[i + 1] == ']' {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Render markdown into `ui`. `embed` resolves `![[target]]` to
/// `(title, body)`; `depth` guards against recursive embeds.
pub fn render(
    ui: &mut egui::Ui,
    text: &str,
    base_size: f32,
    embed: &dyn Fn(&str) -> Option<(String, String)>,
    fence: &dyn Fn(&mut egui::Ui, &str, &str) -> bool,
    depth: usize,
) -> Vec<LinkClick> {
    let mut clicked: Vec<LinkClick> = Vec::new();
    let lines: Vec<&str> = text.split('\n').collect();
    let mut i = 0;

    while i < lines.len() {
        let raw = lines[i];
        let line = raw.trim_end();
        let trimmed = line.trim_start();

        // Fenced code block (or a special block handled by `fence`).
        if trimmed.starts_with("```") {
            let lang = trimmed.trim_start_matches('`').trim().to_string();
            let mut block = String::new();
            i += 1;
            while i < lines.len() && !lines[i].trim_start().starts_with("```") {
                block.push_str(lines[i]);
                block.push('\n');
                i += 1;
            }
            i += 1; // consume closing fence
            if !fence(ui, &lang, &block) {
                code_block(ui, &block, base_size);
            }
            continue;
        }

        // Display math: $$ ... $$, either on one line or fenced by $$ lines.
        if trimmed.starts_with("$$") {
            let mut tex = String::new();
            if trimmed.len() > 4 && trimmed.ends_with("$$") {
                tex = trimmed[2..trimmed.len() - 2].to_string();
                i += 1;
            } else {
                let first = trimmed.strip_prefix("$$").unwrap_or("").to_string();
                if !first.trim().is_empty() {
                    tex.push_str(&first);
                }
                i += 1;
                while i < lines.len() && !lines[i].trim().ends_with("$$") {
                    tex.push('\n');
                    tex.push_str(lines[i]);
                    i += 1;
                }
                if i < lines.len() {
                    let body = lines[i].trim().trim_end_matches("$$");
                    if !body.trim().is_empty() {
                        tex.push('\n');
                        tex.push_str(body);
                    }
                    i += 1;
                }
            }
            display_math(ui, &tex, base_size);
            continue;
        }

        // Embed: ![[target]]
        if trimmed.starts_with("![[") {
            if let Some(end) = trimmed.find("]]") {
                let target = trimmed[3..end].trim();
                render_embed(ui, target, base_size, embed, fence, depth, &mut clicked);
                i += 1;
                continue;
            }
        }

        // Table: a header row followed by a separator row.
        if trimmed.starts_with('|') && i + 1 < lines.len() && is_table_sep(lines[i + 1]) {
            let mut rows: Vec<Vec<String>> = Vec::new();
            rows.push(parse_table_row(line));
            i += 2; // skip header + separator
            while i < lines.len() && lines[i].trim_start().starts_with('|') {
                rows.push(parse_table_row(lines[i]));
                i += 1;
            }
            render_table(ui, &rows, base_size, &mut clicked, i);
            continue;
        }

        // Callout: > [!type] Title
        if trimmed.starts_with("> [!") {
            let header = &trimmed[2..];
            let title = header
                .split_once(']')
                .map(|(_, t)| t.trim().to_string())
                .unwrap_or_default();
            let kind = header
                .trim_start_matches('[')
                .trim_start_matches('!')
                .split(']')
                .next()
                .unwrap_or("note")
                .trim()
                .to_string();
            let mut body = String::new();
            i += 1;
            while i < lines.len() && lines[i].trim_start().starts_with('>') {
                body.push_str(lines[i].trim_start().trim_start_matches('>').trim_start());
                body.push('\n');
                i += 1;
            }
            render_callout(
                ui,
                &kind,
                &title,
                &body,
                base_size,
                embed,
                fence,
                depth,
                &mut clicked,
            );
            continue;
        }

        if trimmed.is_empty() {
            ui.add_space(base_size * 0.5);
            i += 1;
            continue;
        }

        // Horizontal rule.
        if trimmed == "---" || trimmed == "***" || trimmed == "___" {
            ui.separator();
            i += 1;
            continue;
        }

        // Footnote definition: [^id]: text
        if trimmed.starts_with("[^") {
            if let Some(rest) = trimmed.strip_prefix("[^") {
                if let Some(colon) = rest.find("]:") {
                    let id = &rest[..colon];
                    let body = rest[colon + 2..].trim();
                    ui.horizontal_wrapped(|ui| {
                        ui.label(RichText::new(format!("[^{id}]")).small().weak());
                        render_segments(ui, body, base_size * 0.9, &mut clicked);
                    });
                    i += 1;
                    continue;
                }
            }
        }

        // Headings.
        let hashes = line.chars().take_while(|c| *c == '#').count();
        if (1..=6).contains(&hashes) {
            let rest = &line[hashes..];
            if rest.is_empty() || rest.starts_with(' ') {
                let size = match hashes {
                    1 => base_size * 1.9,
                    2 => base_size * 1.6,
                    3 => base_size * 1.35,
                    4 => base_size * 1.2,
                    5 => base_size * 1.1,
                    _ => base_size,
                };
                ui.add_space(4.0);
                ui.label(RichText::new(rest.trim()).size(size).strong());
                ui.add_space(2.0);
                i += 1;
                continue;
            }
        }

        // Blockquote.
        if let Some(rest) = trimmed.strip_prefix("> ") {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("▏").color(ui.visuals().hyperlink_color));
                render_segments(ui, rest, base_size, &mut clicked);
            });
            i += 1;
            continue;
        }

        // Task list checkbox.
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("- [ ] ") || lower.starts_with("- [x] ") {
            let checked = lower.starts_with("- [x] ");
            let rest = trimmed[6..].to_string();
            ui.horizontal_wrapped(|ui| {
                ui.label(if checked { "☑" } else { "☐" });
                render_segments(ui, &rest, base_size, &mut clicked);
            });
            i += 1;
            continue;
        }

        // Bullet list.
        if let Some(rest) = bullet_prefix(trimmed) {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("•").color(ui.visuals().hyperlink_color));
                render_segments(ui, rest, base_size, &mut clicked);
            });
            i += 1;
            continue;
        }

        // Numbered list.
        if let Some((num, rest)) = numbered_prefix(trimmed) {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(num).color(ui.visuals().hyperlink_color));
                render_segments(ui, rest, base_size, &mut clicked);
            });
            i += 1;
            continue;
        }

        // Regular paragraph.
        let segs = parse_inline(trimmed);
        if segs.iter().all(|s| matches!(s, Inline::Text { .. })) {
            if let Some(Inline::Text {
                text,
                bold,
                italic,
                strike,
                code,
            }) = segs.into_iter().next()
            {
                let mut rt = RichText::new(text).size(base_size);
                if bold {
                    rt = rt.strong();
                }
                if italic {
                    rt = rt.italics();
                }
                if strike {
                    rt = rt.strikethrough();
                }
                if code {
                    rt = rt
                        .monospace()
                        .background_color(ui.visuals().extreme_bg_color);
                }
                ui.add(egui::Label::new(rt).wrap(true));
            }
        } else {
            ui.horizontal_wrapped(|ui| {
                render_segments(ui, trimmed, base_size, &mut clicked);
            });
        }
        i += 1;
    }

    clicked
}

fn display_math(ui: &mut egui::Ui, tex: &str, base_size: f32) {
    let s = crate::math::latex_to_unicode(tex.trim());
    ui.add_space(2.0);
    ui.vertical_centered(|ui| {
        ui.label(RichText::new(s).italics().size(base_size * 1.25));
    });
    ui.add_space(2.0);
}

fn code_block(ui: &mut egui::Ui, block: &str, base_size: f32) {
    egui::Frame::none()
        .fill(ui.visuals().extreme_bg_color)
        .inner_margin(egui::Margin::same(6.0))
        .rounding(egui::Rounding::same(4.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(RichText::new(block.trim_end()).monospace().size(base_size))
                    .wrap(true),
            );
        });
    ui.add_space(4.0);
}

fn render_embed(
    ui: &mut egui::Ui,
    target: &str,
    base_size: f32,
    embed: &dyn Fn(&str) -> Option<(String, String)>,
    fence: &dyn Fn(&mut egui::Ui, &str, &str) -> bool,
    depth: usize,
    clicked: &mut Vec<LinkClick>,
) {
    let Some((title, body)) = embed(target) else {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("!").color(ui.visuals().warn_fg_color));
            ui.label(RichText::new(format!("[[{target}]] not found")).weak());
        });
        return;
    };
    egui::Frame::none()
        .fill(ui.visuals().faint_bg_color)
        .stroke(egui::Stroke::new(
            1.0_f32,
            ui.visuals().hyperlink_color.linear_multiply(0.4),
        ))
        .inner_margin(egui::Margin::same(10.0))
        .rounding(egui::Rounding::same(6.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(&title).strong().small());
            ui.add_space(2.0);
            if depth < 2 {
                let inner = render(ui, &body, base_size * 0.92, embed, fence, depth + 1);
                clicked.extend(inner);
            } else {
                ui.label(RichText::new("(embed depth limit reached)").weak().small());
            }
        });
    ui.add_space(4.0);
}

#[allow(clippy::too_many_arguments)]
fn render_callout(
    ui: &mut egui::Ui,
    kind: &str,
    title: &str,
    body: &str,
    base_size: f32,
    embed: &dyn Fn(&str) -> Option<(String, String)>,
    fence: &dyn Fn(&mut egui::Ui, &str, &str) -> bool,
    depth: usize,
    clicked: &mut Vec<LinkClick>,
) {
    let accent = ui.visuals().hyperlink_color;
    egui::Frame::none()
        .fill(ui.visuals().faint_bg_color)
        .inner_margin(egui::Margin::symmetric(10.0, 8.0))
        .rounding(egui::Rounding::same(6.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                let (rect, _) = ui.allocate_exact_size(Vec2::new(3.0, 16.0), Sense::hover());
                ui.painter().rect_filled(rect, 1.5, accent);
                let label = if title.is_empty() {
                    kind.to_uppercase()
                } else {
                    title.to_string()
                };
                ui.label(RichText::new(label).strong().color(accent));
            });
            ui.add_space(2.0);
            let inner = render(ui, body, base_size * 0.95, embed, fence, depth + 1);
            clicked.extend(inner);
        });
    ui.add_space(4.0);
}

fn is_table_sep(line: &str) -> bool {
    let cells = parse_table_row(line);
    !cells.is_empty()
        && cells.iter().all(|c| {
            let c = c.trim_matches(':').trim();
            c.len() >= 2 && c.chars().all(|ch| ch == '-')
        })
}

fn parse_table_row(line: &str) -> Vec<String> {
    let t = line.trim();
    let t = t.strip_prefix('|').unwrap_or(t);
    let t = t.strip_suffix('|').unwrap_or(t);
    t.split('|').map(|c| c.trim().to_string()).collect()
}

fn render_table(
    ui: &mut egui::Ui,
    rows: &[Vec<String>],
    base_size: f32,
    clicked: &mut Vec<LinkClick>,
    salt: usize,
) {
    egui::Grid::new(("md-table", salt))
        .striped(true)
        .spacing([14.0, 4.0])
        .show(ui, |ui| {
            for (r, row) in rows.iter().enumerate() {
                for cell in row {
                    ui.horizontal_wrapped(|ui| {
                        if r == 0 {
                            render_segments_bold(ui, cell, base_size, clicked);
                        } else {
                            render_segments(ui, cell, base_size, clicked);
                        }
                    });
                }
                ui.end_row();
            }
        });
    ui.add_space(4.0);
}

fn render_segments_bold(
    ui: &mut egui::Ui,
    line: &str,
    base_size: f32,
    clicked: &mut Vec<LinkClick>,
) {
    for seg in parse_inline(line) {
        match seg {
            Inline::Text {
                text, code, strike, ..
            } => {
                let mut rt = RichText::new(text).size(base_size).strong();
                if code {
                    rt = rt.monospace();
                }
                if strike {
                    rt = rt.strikethrough();
                }
                ui.label(rt);
            }
            other => render_one(ui, other, base_size, clicked),
        }
    }
}

fn render_segments(ui: &mut egui::Ui, line: &str, base_size: f32, clicked: &mut Vec<LinkClick>) {
    for seg in parse_inline(line) {
        render_one(ui, seg, base_size, clicked);
    }
}

fn render_one(ui: &mut egui::Ui, seg: Inline, base_size: f32, clicked: &mut Vec<LinkClick>) {
    match seg {
        Inline::Text {
            text,
            bold,
            italic,
            strike,
            code,
        } => {
            let mut rt = RichText::new(text).size(base_size);
            if bold {
                rt = rt.strong();
            }
            if italic {
                rt = rt.italics();
            }
            if strike {
                rt = rt.strikethrough();
            }
            if code {
                rt = rt
                    .monospace()
                    .background_color(ui.visuals().extreme_bg_color);
            }
            ui.label(rt);
        }
        Inline::Wiki { target, alias } => {
            let color = ui.visuals().hyperlink_color;
            if ui
                .link(RichText::new(alias).size(base_size).color(color))
                .clicked()
            {
                clicked.push(LinkClick::Wiki(target));
            }
        }
        Inline::External { text, url } => {
            let color = ui.visuals().hyperlink_color;
            let shown = if text.is_empty() { url.clone() } else { text };
            if ui
                .link(
                    RichText::new(shown)
                        .size(base_size)
                        .color(color)
                        .underline(),
                )
                .clicked()
            {
                clicked.push(LinkClick::External(url));
            }
        }
        Inline::Footnote(id) => {
            ui.label(
                RichText::new(format!("[{id}]"))
                    .size(base_size * 0.7)
                    .color(ui.visuals().hyperlink_color),
            );
        }
        Inline::Math(tex) => {
            let s = crate::math::latex_to_unicode(&tex);
            ui.label(RichText::new(s).italics().size(base_size * 1.05));
        }
    }
}

fn bullet_prefix(line: &str) -> Option<&str> {
    for prefix in ["- ", "* ", "+ "] {
        if let Some(rest) = line.strip_prefix(prefix) {
            return Some(rest);
        }
    }
    None
}

fn numbered_prefix(line: &str) -> Option<(String, &str)> {
    let digits: String = line.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    let rest = &line[digits.len()..];
    if let Some(rest) = rest.strip_prefix(". ") {
        Some((format!("{digits}."), rest))
    } else {
        None
    }
}

/// A no-op colour used by tests only.
#[allow(dead_code)]
const _UNUSED: Color32 = Color32::WHITE;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_plain_wikilinks() {
        assert_eq!(
            wikilinks("see [[Alpha]] and [[Beta]]"),
            vec!["Alpha", "Beta"]
        );
    }

    #[test]
    fn extracts_aliased_wikilinks() {
        assert_eq!(wikilinks("[[Target|shown text]]"), vec!["Target"]);
    }

    #[test]
    fn ignores_unterminated_wikilinks() {
        assert!(wikilinks("[[oops").is_empty());
    }

    #[test]
    fn collects_headings_with_levels() {
        let md = "# Title\n\ntext\n\n## Sub\n### Deep\n";
        assert_eq!(
            headings(md),
            vec![
                (1, "Title".to_string()),
                (2, "Sub".to_string()),
                (3, "Deep".to_string()),
            ]
        );
    }

    #[test]
    fn ignores_headings_inside_code_fences() {
        let md = "```\n# not a heading\n```\n# real\n";
        assert_eq!(headings(md), vec![(1, "real".to_string())]);
    }

    #[test]
    fn parses_bold_italic_code_and_strike() {
        let segs = parse_inline("a **b** c *d* `e` ~~f~~");
        let texts: Vec<(String, bool, bool, bool, bool)> = segs
            .into_iter()
            .filter_map(|s| match s {
                Inline::Text {
                    text,
                    bold,
                    italic,
                    strike,
                    code,
                } => Some((text, bold, italic, strike, code)),
                _ => None,
            })
            .collect();
        assert!(texts.iter().any(|(t, b, _, _, _)| t == "b" && *b));
        assert!(texts.iter().any(|(t, _, i, _, _)| t == "d" && *i));
        assert!(texts.iter().any(|(t, _, _, _, c)| t == "e" && *c));
        assert!(texts.iter().any(|(t, _, _, s, _)| t == "f" && *s));
    }

    #[test]
    fn parses_markdown_links() {
        let segs = parse_inline("go [here](https://example.com) now");
        assert!(segs.iter().any(|s| matches!(
            s,
            Inline::External { url, .. } if url == "https://example.com"
        )));
    }

    #[test]
    fn table_separator_detected() {
        assert!(is_table_sep("|---|:---:|---|"));
        assert!(!is_table_sep("| a | b |"));
        assert!(is_table_sep("|---|---|"));
    }

    #[test]
    fn table_rows_parse_cells() {
        assert_eq!(parse_table_row("| a | b |"), vec!["a", "b"]);
        assert_eq!(parse_table_row("a | b"), vec!["a", "b"]);
    }
}
