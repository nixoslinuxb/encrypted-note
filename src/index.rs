//! In-memory indexes over a vault: titles, aliases, tags, and links.
//!
//! Built once per vault load/refresh, so tag lookups, alias resolution and the
//! graph view are all O(1)/O(log n) instead of re-reading every note.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// One note's contribution to the index.
#[derive(Clone, Debug)]
pub struct Entry {
    pub path: PathBuf,
    pub title: String,
    pub aliases: Vec<String>,
    pub tags: Vec<String>,
    /// Outgoing `[[wikilink]]` targets (raw, as written).
    pub links: Vec<String>,
}

#[derive(Default, Debug)]
pub struct NoteIndex {
    titles: BTreeMap<String, PathBuf>,
    aliases: BTreeMap<String, PathBuf>,
    tags: BTreeMap<String, Vec<PathBuf>>,
    tag_display: BTreeMap<String, String>,
    links: BTreeMap<PathBuf, Vec<String>>,
}

impl NoteIndex {
    pub fn build(entries: Vec<Entry>) -> Self {
        let mut idx = NoteIndex::default();
        for e in entries {
            idx.titles
                .insert(e.title.to_ascii_lowercase(), e.path.clone());
            for alias in &e.aliases {
                idx.aliases
                    .insert(alias.to_ascii_lowercase(), e.path.clone());
            }
            for tag in &e.tags {
                let key = tag.to_ascii_lowercase();
                idx.tag_display
                    .entry(key.clone())
                    .or_insert_with(|| tag.clone());
                let list = idx.tags.entry(key).or_default();
                if !list.contains(&e.path) {
                    list.push(e.path.clone());
                }
            }
            idx.links.insert(e.path, e.links);
        }
        for list in idx.tags.values_mut() {
            list.sort();
        }
        idx
    }

    /// Resolve a wikilink target (which may carry `#heading` / `#^block` and is
    /// case-insensitive) to a note path, trying titles then aliases.
    pub fn resolve(&self, target: &str) -> Option<PathBuf> {
        let base = base_target(target).to_ascii_lowercase();
        self.titles
            .get(&base)
            .or_else(|| self.aliases.get(&base))
            .cloned()
    }

    /// Notes carrying a tag (case-insensitive).
    pub fn notes_with_tag(&self, tag: &str) -> Vec<PathBuf> {
        self.tags
            .get(&tag.to_ascii_lowercase())
            .cloned()
            .unwrap_or_default()
    }

    /// All tags with their note counts, sorted by tag name.
    pub fn all_tags(&self) -> Vec<(String, usize)> {
        self.tags
            .iter()
            .map(|(k, v)| {
                (
                    self.tag_display
                        .get(k)
                        .cloned()
                        .unwrap_or_else(|| k.clone()),
                    v.len(),
                )
            })
            .collect()
    }

    /// Resolved undirected edges between known notes, for the graph view.
    pub fn edges(&self) -> Vec<(PathBuf, PathBuf)> {
        let mut out = Vec::new();
        for (from, links) in &self.links {
            for link in links {
                if let Some(to) = self.resolve(link) {
                    if &to != from {
                        out.push((from.clone(), to));
                    }
                }
            }
        }
        out
    }
}

/// Strip a `#heading` / `#^block` suffix from a wikilink target.
pub fn base_target(target: &str) -> &str {
    target.split('#').next().unwrap_or(target).trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, title: &str, aliases: &[&str], tags: &[&str], links: &[&str]) -> Entry {
        Entry {
            path: PathBuf::from(path),
            title: title.into(),
            aliases: aliases.iter().map(|s| s.to_string()).collect(),
            tags: tags.iter().map(|s| s.to_string()).collect(),
            links: links.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn resolves_titles_and_aliases_case_insensitively() {
        let idx = NoteIndex::build(vec![entry("/v/a.md", "Alpha", &["first"], &[], &[])]);
        assert_eq!(idx.resolve("alpha"), Some(PathBuf::from("/v/a.md")));
        assert_eq!(idx.resolve("ALPHA"), Some(PathBuf::from("/v/a.md")));
        assert_eq!(idx.resolve("first"), Some(PathBuf::from("/v/a.md")));
        assert_eq!(idx.resolve("missing"), None);
    }

    #[test]
    fn resolves_link_with_anchor() {
        let idx = NoteIndex::build(vec![entry("/v/a.md", "Alpha", &[], &[], &[])]);
        assert_eq!(idx.resolve("Alpha#^block1"), Some(PathBuf::from("/v/a.md")));
        assert_eq!(idx.resolve("Alpha#Section"), Some(PathBuf::from("/v/a.md")));
    }

    #[test]
    fn indexes_tags_with_counts() {
        let idx = NoteIndex::build(vec![
            entry("/v/a.md", "A", &[], &["work", "rust"], &[]),
            entry("/v/b.md", "B", &[], &["work"], &[]),
        ]);
        assert_eq!(idx.notes_with_tag("WORK").len(), 2);
        let tags = idx.all_tags();
        assert!(tags.contains(&("work".to_string(), 2)));
        assert!(tags.contains(&("rust".to_string(), 1)));
    }

    #[test]
    fn builds_undirected_edges() {
        let idx = NoteIndex::build(vec![
            entry("/v/a.md", "A", &[], &[], &["B"]),
            entry("/v/b.md", "B", &[], &[], &[]),
        ]);
        let edges = idx.edges();
        assert_eq!(edges.len(), 1);
        assert_eq!(
            edges[0],
            (PathBuf::from("/v/a.md"), PathBuf::from("/v/b.md"))
        );
    }
}
