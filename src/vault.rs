//! Vault model: a directory of markdown notes, scanned into a navigable tree.

use std::fs;
use std::path::{Path, PathBuf};

/// A single markdown note inside a vault.
#[derive(Clone, Debug)]
pub struct Note {
    /// Absolute path on disk.
    pub path: PathBuf,
    /// Path relative to the vault root.
    pub rel: PathBuf,
    /// Display title (file stem).
    pub title: String,
}

/// A directory node used to render the sidebar tree.
#[derive(Clone, Debug, Default)]
pub struct TreeNode {
    pub name: String,
    /// Path (relative to the vault root) identifying this node; used as a
    /// stable UI id for collapsing sections.
    pub rel: PathBuf,
    pub dirs: Vec<TreeNode>,
    pub files: Vec<Note>,
}

impl TreeNode {
    /// Number of notes contained in this subtree.
    pub fn note_count(&self) -> usize {
        self.files.len() + self.dirs.iter().map(TreeNode::note_count).sum::<usize>()
    }
}

/// An open vault.
pub struct Vault {
    pub root: PathBuf,
    pub notes: Vec<Note>,
}

impl Vault {
    /// Open a vault rooted at `root`, scanning it for markdown notes.
    pub fn open(root: PathBuf) -> std::io::Result<Self> {
        let mut vault = Vault {
            root,
            notes: Vec::new(),
        };
        vault.rescan()?;
        Ok(vault)
    }

    /// Display name for the vault (its directory name).
    pub fn name(&self) -> String {
        self.root
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| self.root.to_string_lossy().to_string())
    }

    /// Re-walk the vault, refreshing the note list.
    pub fn rescan(&mut self) -> std::io::Result<()> {
        self.notes.clear();
        let mut stack = vec![self.root.clone()];
        while let Some(dir) = stack.pop() {
            let entries = match fs::read_dir(&dir) {
                Ok(e) => e,
                Err(_) => continue,
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let file_name = entry.file_name();
                let name = file_name.to_string_lossy();
                // Skip hidden entries (including `.git`, `.obsidian`, ...).
                if name.starts_with('.') {
                    continue;
                }
                let file_type = match entry.file_type() {
                    Ok(t) => t,
                    Err(_) => continue,
                };
                if file_type.is_dir() {
                    stack.push(path);
                } else if is_markdown(&path) {
                    let rel = path.strip_prefix(&self.root).unwrap_or(&path).to_path_buf();
                    let title = path
                        .file_stem()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| name.to_string());
                    self.notes.push(Note { path, rel, title });
                }
            }
        }
        // Stable ordering for a predictable tree.
        self.notes.sort_by(|a, b| a.rel.cmp(&b.rel));
        Ok(())
    }

    /// Build the sidebar tree from the current note list.
    pub fn tree(&self) -> TreeNode {
        let mut root = TreeNode {
            name: self.name(),
            rel: PathBuf::new(),
            ..Default::default()
        };
        for note in &self.notes {
            insert(&mut root, note);
        }
        sort_tree(&mut root);
        root
    }

    /// Sanitize a user-supplied name into a safe file stem.
    /// Read a note's raw bytes from disk.
    pub fn read_bytes(path: &Path) -> std::io::Result<Vec<u8>> {
        fs::read(path)
    }

    /// Write raw bytes to a note path, creating parent directories as needed.
    /// The write goes to a temp file which is then renamed into place so a
    /// crash cannot leave a half-written (and thus undecryptable) note.
    pub fn write_bytes(path: &Path, contents: &[u8]) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("tmp-encd");
        fs::write(&tmp, contents)?;
        fs::rename(&tmp, path)?;
        Ok(())
    }

    /// Sanitize a user-supplied name into a safe file stem.
    pub fn sanitize_name(name: &str) -> String {
        let cleaned: String = name
            .trim()
            .chars()
            .map(|c| match c {
                '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '-',
                c if c.is_control() => '-',
                c => c,
            })
            .collect();
        if cleaned.is_empty() {
            "Untitled".to_string()
        } else {
            cleaned
        }
    }

    /// Create a new note with a unique name, writing the given (already
    /// encrypted) bytes, and returning its path.
    pub fn create_note(&self, name: &str, contents: &[u8]) -> std::io::Result<PathBuf> {
        let stem = Self::sanitize_name(name);
        let mut path = self.root.join(format!("{stem}.md"));
        let mut counter = 1;
        while path.exists() {
            path = self.root.join(format!("{stem} {counter}.md"));
            counter += 1;
        }
        Vault::write_bytes(&path, contents)?;
        Ok(path)
    }
}

impl Vault {
    /// Notes whose vault-relative path starts with `folder`.
    pub fn notes_under(&self, folder: &str) -> Vec<PathBuf> {
        self.notes
            .iter()
            .filter(|n| rel_under(&n.rel, folder))
            .map(|n| n.path.clone())
            .collect()
    }

    /// The path of the daily note for `(y, m, d)` inside `folder`, named with
    /// the given moment-style `format`.
    pub fn daily_note_path(&self, folder: &str, format: &str, y: i32, m: u32, d: u32) -> PathBuf {
        let mut p = self.root.clone();
        if !folder.trim().is_empty() {
            p.push(folder.trim());
        }
        p.join(format!("{}.md", crate::dates::format(format, y, m, d)))
    }

    /// Template note paths inside `folder`.
    pub fn template_paths(&self, folder: &str) -> Vec<PathBuf> {
        self.notes_under(folder)
    }
}

/// Whether a vault-relative path lives under `folder` (first component match).
fn rel_under(rel: &Path, folder: &str) -> bool {
    let folder = folder.trim().trim_matches('/');
    if folder.is_empty() {
        return false;
    }
    rel.components()
        .next()
        .map(|c| c.as_os_str().to_string_lossy().eq_ignore_ascii_case(folder))
        .unwrap_or(false)
}

fn is_markdown(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some("md") | Some("markdown")
    )
}

fn insert(node: &mut TreeNode, note: &Note) {
    let mut parts: Vec<String> = note
        .rel
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_string())
        .collect();
    // Drop the file name; remaining components are directories.
    parts.pop();
    let mut cursor = node;
    let mut rel = std::path::PathBuf::new();
    for part in parts {
        rel.push(&part);
        if let Some(idx) = cursor.dirs.iter().position(|d| d.name == part) {
            cursor = &mut cursor.dirs[idx];
        } else {
            cursor.dirs.push(TreeNode {
                name: part,
                rel: rel.clone(),
                ..Default::default()
            });
            let last = cursor.dirs.len() - 1;
            cursor = &mut cursor.dirs[last];
        }
    }
    cursor.files.push(note.clone());
}

fn sort_tree(node: &mut TreeNode) {
    node.dirs.sort_by_key(|a| a.name.to_lowercase());
    node.files.sort_by_key(|a| a.title.to_lowercase());
    for dir in &mut node.dirs {
        sort_tree(dir);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_dir(tag: &str) -> PathBuf {
        let mut dir = std::env::temp_dir();
        dir.push(format!("encrypted-note-test-{tag}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn sanitize_replaces_path_separators() {
        assert_eq!(Vault::sanitize_name("a/b:c"), "a-b-c");
        assert_eq!(Vault::sanitize_name("   "), "Untitled");
    }

    #[test]
    fn scans_markdown_and_skips_hidden() {
        let root = temp_dir("scan");
        fs::write(root.join("a.md"), "# a").unwrap();
        fs::write(root.join("notes.txt"), "nope").unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub").join("b.md"), "# b").unwrap();
        fs::create_dir_all(root.join(".hidden")).unwrap();
        fs::write(root.join(".hidden").join("c.md"), "# c").unwrap();

        let vault = Vault::open(root.clone()).unwrap();
        let titles: Vec<_> = vault.notes.iter().map(|n| n.title.clone()).collect();
        assert!(titles.contains(&"a".to_string()));
        assert!(titles.contains(&"b".to_string()));
        assert!(!titles.contains(&"c".to_string()));
        assert_eq!(vault.notes.len(), 2);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn builds_tree_with_folder_counts() {
        let root = temp_dir("tree");
        fs::write(root.join("a.md"), "# a").unwrap();
        fs::create_dir_all(root.join("sub")).unwrap();
        fs::write(root.join("sub").join("b.md"), "# b").unwrap();

        let vault = Vault::open(root.clone()).unwrap();
        let tree = vault.tree();
        assert_eq!(tree.files.len(), 1);
        assert_eq!(tree.dirs.len(), 1);
        assert_eq!(tree.dirs[0].note_count(), 1);

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn create_note_avoids_collisions() {
        let root = temp_dir("create");
        let vault = Vault::open(root.clone()).unwrap();
        let p1 = vault.create_note("Hello", b"# a").unwrap();
        let p2 = vault.create_note("Hello", b"# b").unwrap();
        assert_ne!(p1, p2);
        assert!(p1.exists() && p2.exists());
        assert_eq!(Vault::read_bytes(&p1).unwrap(), b"# a");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn finds_notes_under_a_folder() {
        let root = temp_dir("folder");
        fs::create_dir_all(root.join("Daily")).unwrap();
        fs::write(root.join("Daily").join("2024-01-01.md"), "x").unwrap();
        fs::write(root.join("top.md"), "x").unwrap();
        let vault = Vault::open(root.clone()).unwrap();
        assert_eq!(vault.notes_under("Daily").len(), 1);
        assert_eq!(vault.notes_under("daily").len(), 1); // case-insensitive
        assert_eq!(vault.notes_under("Missing").len(), 0);
        let _ = fs::remove_dir_all(&root);
    }
}
