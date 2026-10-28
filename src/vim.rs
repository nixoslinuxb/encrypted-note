//! A small, editor-agnostic Vim engine: modal key handling over a `String`.
//!
//! `Vim::handle` mutates the buffer and the cursor and reports whether it
//! consumed the key. It has no `egui` dependency, so it's fully unit-testable.
//! Supported: motions `h j k l 0 ^ $ w b e gg G`; operators `x dd dw d$ D yy yw`;
//! `i a I A o O`, `p P`, `r`, visual `v` + `d`/`y`, and `u` (undo).

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Normal,
    Insert,
    Visual,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    Esc,
    Enter,
    Backspace,
    Tab,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    Delete,
}

pub struct Vim {
    pub mode: Mode,
    /// Cursor as a character index into the buffer.
    pub cursor: usize,
    desired_col: Option<usize>,
    pending: Option<Operator>,
    count: u32,
    register: String,
    register_linewise: bool,
    anchor: usize,
    undo: Vec<(String, usize)>,
    /// Some(inner) while waiting for a text-object char (`i`/`a`).
    awaiting_object: Option<bool>,
    /// Text accumulated during the current insert session (for `.`-repeat).
    insert_buf: String,
    /// The last change, replayed by `.`.
    last_change: Option<Change>,
}

#[derive(Clone, Debug)]
enum Target {
    Motion(char),
    Obj(bool, char),
}

#[derive(Clone, Debug)]
enum Change {
    Insert(String),
    Op {
        op: Operator,
        count: u32,
        target: Target,
    },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Operator {
    Delete,
    Yank,
    Change,
    G,
    Replace,
}

impl Default for Vim {
    fn default() -> Self {
        Self::new()
    }
}

impl Vim {
    pub fn new() -> Self {
        Vim {
            mode: Mode::Normal,
            cursor: 0,
            desired_col: None,
            pending: None,
            count: 0,
            register: String::new(),
            register_linewise: false,
            anchor: 0,
            undo: Vec::new(),
            awaiting_object: None,
            insert_buf: String::new(),
            last_change: None,
        }
    }

    /// A short status line for the UI.
    pub fn status(&self) -> String {
        match self.mode {
            Mode::Normal => "-- NORMAL --".to_string(),
            Mode::Insert => "-- INSERT --".to_string(),
            Mode::Visual => "-- VISUAL --".to_string(),
        }
    }

    fn count(&mut self) -> u32 {
        let n = self.count.max(1);
        self.count = 0;
        n
    }

    fn snapshot(&mut self, text: &str) {
        self.undo.push((text.to_string(), self.cursor));
        if self.undo.len() > 200 {
            self.undo.remove(0);
        }
    }

    /// Handle one key. Returns `true` if it was consumed.
    pub fn handle(&mut self, key: Key, text: &mut String) -> bool {
        match self.mode {
            Mode::Insert => self.handle_insert(key, text),
            Mode::Normal => self.handle_normal(key, text),
            Mode::Visual => self.handle_visual(key, text),
        }
    }

    // ------------------------------------------------------------- insert

    fn handle_insert(&mut self, key: Key, text: &mut String) -> bool {
        let chars: Vec<char> = text.chars().collect();
        match key {
            Key::Esc => {
                self.mode = Mode::Normal;
                if !self.insert_buf.is_empty() {
                    self.last_change = Some(Change::Insert(std::mem::take(&mut self.insert_buf)));
                }
                // Place the cursor on the last inserted character.
                self.cursor = clamp_normal(&chars, self.cursor.saturating_sub(1));
                true
            }
            Key::Char(c) => {
                insert_char(text, self.cursor, c);
                self.cursor += 1;
                self.insert_buf.push(c);
                true
            }
            Key::Enter => {
                insert_char(text, self.cursor, '\n');
                self.cursor += 1;
                self.insert_buf.push('\n');
                self.desired_col = None;
                true
            }
            Key::Tab => {
                for _ in 0..4 {
                    insert_char(text, self.cursor, ' ');
                    self.cursor += 1;
                }
                self.insert_buf.push_str("    ");
                true
            }
            Key::Backspace => {
                if self.cursor > 0 {
                    self.cursor -= 1;
                    remove_range(text, self.cursor, self.cursor + 1);
                    self.insert_buf.pop();
                }
                true
            }
            Key::Delete => {
                if self.cursor < chars.len() {
                    remove_range(text, self.cursor, self.cursor + 1);
                }
                true
            }
            Key::Left => {
                self.cursor = self.cursor.saturating_sub(1);
                true
            }
            Key::Right => {
                self.cursor = (self.cursor + 1).min(chars.len());
                true
            }
            Key::Up => {
                self.cursor = move_vertical(&chars, self.cursor, -1, true);
                true
            }
            Key::Down => {
                self.cursor = move_vertical(&chars, self.cursor, 1, true);
                true
            }
            Key::Home => {
                self.cursor = line_bounds(&chars, self.cursor).0;
                true
            }
            Key::End => {
                self.cursor = line_bounds(&chars, self.cursor).1;
                true
            }
        }
    }

    // ------------------------------------------------------------- normal

    fn handle_normal(&mut self, key: Key, text: &mut String) -> bool {
        // Count digits (but not a leading 0).
        if let Key::Char(c) = key {
            if c.is_ascii_digit() && (self.count > 0 || c != '0') {
                self.count = self
                    .count
                    .saturating_mul(10)
                    .saturating_add(c as u32 - '0' as u32);
                return true;
            }
        }
        if let Some(op) = self.pending {
            return self.apply_operator(op, key, text);
        }

        let chars: Vec<char> = text.chars().collect();
        match key {
            Key::Esc => {
                self.count = 0;
                true
            }
            Key::Left | Key::Char('h') => {
                let n = self.count();
                self.cursor = clamp_normal(&chars, self.cursor.saturating_sub(n as usize));
                self.desired_col = None;
                true
            }
            Key::Right | Key::Char('l') => {
                let n = self.count();
                let (_, end) = line_bounds(&chars, self.cursor);
                let last = if end > 0 { end - 1 } else { 0 };
                self.cursor = (self.cursor + n as usize).min(last);
                self.desired_col = None;
                true
            }
            Key::Down | Key::Char('j') => {
                let n = self.count();
                self.cursor = move_vertical(&chars, self.cursor, n as i32, false);
                true
            }
            Key::Up | Key::Char('k') => {
                let n = self.count();
                self.cursor = move_vertical(&chars, self.cursor, -(n as i32), false);
                true
            }
            Key::Home | Key::Char('0') => {
                self.cursor = line_bounds(&chars, self.cursor).0;
                true
            }
            Key::Char('^') => {
                let (start, end) = line_bounds(&chars, self.cursor);
                self.cursor = first_non_blank(&chars, start, end);
                true
            }
            Key::End | Key::Char('$') => {
                let n = self.count();
                let mut i = self.cursor;
                for _ in 1..n {
                    i = next_line(&chars, i);
                }
                let (_, end) = line_bounds(&chars, i);
                self.cursor = if end > 0 { end - 1 } else { 0 };
                true
            }
            Key::Char('w') => {
                let n = self.count();
                for _ in 0..n {
                    self.cursor = next_word_start(&chars, self.cursor);
                }
                self.desired_col = None;
                true
            }
            Key::Char('b') => {
                let n = self.count();
                for _ in 0..n {
                    self.cursor = prev_word_start(&chars, self.cursor);
                }
                self.desired_col = None;
                true
            }
            Key::Char('e') => {
                let n = self.count();
                for _ in 0..n {
                    self.cursor = word_end(&chars, self.cursor);
                }
                self.desired_col = None;
                true
            }
            Key::Char('G') => {
                if self.count > 0 {
                    let n = self.count();
                    self.cursor = nth_line_start(&chars, n as usize - 1);
                } else {
                    self.cursor = nth_line_start(&chars, total_lines(&chars).saturating_sub(1));
                }
                self.desired_col = None;
                true
            }
            Key::Char('i') => {
                self.insert_buf.clear();
                self.mode = Mode::Insert;
                true
            }
            Key::Char('a') => {
                let (_, end) = line_bounds(&chars, self.cursor);
                self.cursor = (self.cursor + 1).min(if end > 0 { end } else { 0 });
                self.insert_buf.clear();
                self.mode = Mode::Insert;
                true
            }
            Key::Char('I') => {
                let (start, end) = line_bounds(&chars, self.cursor);
                self.cursor = first_non_blank(&chars, start, end);
                self.insert_buf.clear();
                self.mode = Mode::Insert;
                true
            }
            Key::Char('A') => {
                let (_, end) = line_bounds(&chars, self.cursor);
                self.cursor = end;
                self.insert_buf.clear();
                self.mode = Mode::Insert;
                true
            }
            Key::Char('o') => {
                self.snapshot(text);
                let (_, end) = line_bounds(&chars, self.cursor);
                insert_char(text, end, '\n');
                self.cursor = end + 1;
                self.insert_buf.clear();
                self.mode = Mode::Insert;
                true
            }
            Key::Char('O') => {
                self.snapshot(text);
                let (start, _) = line_bounds(&chars, self.cursor);
                insert_char(text, start, '\n');
                self.cursor = start;
                self.insert_buf.clear();
                self.mode = Mode::Insert;
                true
            }
            Key::Char('v') => {
                self.mode = Mode::Visual;
                self.anchor = self.cursor;
                true
            }
            Key::Char('.') => {
                if let Some(ch) = self.last_change.clone() {
                    self.replay(ch, text);
                }
                true
            }
            Key::Char('x') => {
                let n = self.count();
                self.snapshot(text);
                let (_, end) = line_bounds(&chars, self.cursor);
                let to = (self.cursor + n as usize).min(end);
                let from = self.cursor.min(to);
                remove_range(text, from, to);
                self.cursor = clamp_normal(&text.chars().collect::<Vec<_>>(), from);
                true
            }
            Key::Char('d') => {
                self.pending = Some(Operator::Delete);
                true
            }
            Key::Char('y') => {
                self.pending = Some(Operator::Yank);
                true
            }
            Key::Char('c') => {
                self.pending = Some(Operator::Change);
                true
            }
            Key::Char('r') => {
                self.pending = Some(Operator::Replace);
                true
            }
            Key::Char('g') => {
                self.pending = Some(Operator::G);
                true
            }
            Key::Char('p') => {
                self.snapshot(text);
                let at = if self.register_linewise {
                    let (_, end) = line_bounds(&chars, self.cursor);
                    end + usize::from(end < chars.len())
                } else {
                    (self.cursor + 1).min(chars.len())
                };
                let reg = self.register.clone();
                insert_str(text, at, &reg);
                self.cursor = clamp_normal(&text.chars().collect::<Vec<_>>(), at);
                true
            }
            Key::Char('P') => {
                self.snapshot(text);
                let at = if self.register_linewise {
                    line_bounds(&chars, self.cursor).0
                } else {
                    self.cursor
                };
                let reg = self.register.clone();
                insert_str(text, at, &reg);
                self.cursor = clamp_normal(&text.chars().collect::<Vec<_>>(), at);
                true
            }
            Key::Char('u') => {
                if let Some((prev, cur)) = self.undo.pop() {
                    *text = prev;
                    self.cursor = cur;
                }
                true
            }
            _ => false,
        }
    }

    fn apply_operator(&mut self, op: Operator, key: Key, text: &mut String) -> bool {
        let chars: Vec<char> = text.chars().collect();

        // Awaiting a text-object character (`i`/`a` was pressed).
        if let Some(inner) = self.awaiting_object.take() {
            self.pending = None;
            let n = self.count();
            if let Key::Char(oc) = key {
                if let Some((a, b)) = find_text_object(&chars, self.cursor, inner, oc) {
                    let (a, b) = (a.min(chars.len()), b.min(chars.len()));
                    self.register = chars[a..b].iter().collect();
                    self.register_linewise = false;
                    if op != Operator::Yank {
                        self.snapshot(text);
                        remove_range(text, a, b);
                        self.cursor = clamp_normal(&text.chars().collect::<Vec<_>>(), a);
                    }
                    self.last_change = Some(Change::Op {
                        op,
                        count: n,
                        target: Target::Obj(inner, oc),
                    });
                    if op == Operator::Change {
                        self.insert_buf.clear();
                        self.mode = Mode::Insert;
                    }
                }
            }
            return true;
        }

        match op {
            Operator::G => {
                self.pending = None;
                if key == Key::Char('g') {
                    let n = self.count.max(1) as usize;
                    self.cursor = nth_line_start(&chars, n - 1);
                }
                self.count = 0;
                true
            }
            Operator::Replace => {
                self.pending = None;
                if let Key::Char(c) = key {
                    if self.cursor < chars.len() {
                        self.snapshot(text);
                        remove_range(text, self.cursor, self.cursor + 1);
                        insert_char(text, self.cursor, c);
                        self.last_change = Some(Change::Op {
                            op,
                            count: 1,
                            target: Target::Motion(c),
                        });
                    }
                }
                true
            }
            Operator::Delete | Operator::Yank | Operator::Change => {
                // Text-object prefix.
                if matches!(key, Key::Char('i') | Key::Char('a')) {
                    self.awaiting_object = Some(key == Key::Char('i'));
                    return true;
                }
                let n = self.count();
                // Linewise: dd / yy / cc
                if key == Key::Char('d') || key == Key::Char('y') || key == Key::Char('c') {
                    let same = matches!(
                        (op, key),
                        (Operator::Delete, Key::Char('d'))
                            | (Operator::Yank, Key::Char('y'))
                            | (Operator::Change, Key::Char('c'))
                    );
                    if !same {
                        self.pending = None;
                        self.count = 0;
                        return true;
                    }
                    self.pending = None;
                    let (start, _) = line_bounds(&chars, self.cursor);
                    let mut end = start;
                    for _ in 0..n {
                        let (_, e) = line_bounds(&chars, end);
                        end = e + usize::from(e < chars.len());
                    }
                    self.register = chars[start..end.min(chars.len())].iter().collect();
                    self.register_linewise = true;
                    if op != Operator::Yank {
                        self.snapshot(text);
                        remove_range(text, start, end.min(chars.len()));
                        self.cursor = clamp_normal(&text.chars().collect::<Vec<_>>(), start);
                    }
                    self.last_change = Some(Change::Op {
                        op,
                        count: n,
                        target: Target::Motion('d'),
                    });
                    if op == Operator::Change {
                        self.insert_buf.clear();
                        self.mode = Mode::Insert;
                    }
                    return true;
                }
                let from = self.cursor;
                let motion = match key {
                    Key::Char(c) => c,
                    _ => {
                        self.pending = None;
                        self.count = 0;
                        return true;
                    }
                };
                let to = match motion {
                    'w' => {
                        let mut t = self.cursor;
                        for _ in 0..n {
                            t = next_word_start(&chars, t);
                        }
                        t
                    }
                    '$' => line_bounds(&chars, self.cursor).1,
                    '0' => line_bounds(&chars, self.cursor).0,
                    'e' => {
                        let mut t = self.cursor;
                        for _ in 0..n {
                            t = word_end(&chars, t) + 1;
                        }
                        t.min(chars.len())
                    }
                    _ => {
                        self.pending = None;
                        self.count = 0;
                        return true;
                    }
                };
                let (a, b) = (from.min(to), from.max(to));
                self.register = chars[a..b].iter().collect();
                self.register_linewise = false;
                self.pending = None;
                if op != Operator::Yank {
                    self.snapshot(text);
                    remove_range(text, a, b);
                    self.cursor = clamp_normal(&text.chars().collect::<Vec<_>>(), a);
                }
                self.last_change = Some(Change::Op {
                    op,
                    count: n,
                    target: Target::Motion(motion),
                });
                if op == Operator::Change {
                    self.insert_buf.clear();
                    self.mode = Mode::Insert;
                }
                true
            }
        }
    }

    /// Replay a recorded change (`.`).
    fn replay(&mut self, change: Change, text: &mut String) {
        match change {
            Change::Insert(s) => {
                self.snapshot(text);
                let at = (self.cursor + 1).min(text.chars().count());
                insert_str(text, at, &s);
                let n = s.chars().count();
                let end = (at + n).min(text.chars().count());
                self.cursor =
                    clamp_normal(&text.chars().collect::<Vec<_>>(), end.saturating_sub(1));
            }
            Change::Op { op, count, target } => {
                self.count = count;
                self.pending = Some(op);
                match target {
                    Target::Motion('d') => {
                        self.apply_operator(op, Key::Char('d'), text);
                    }
                    Target::Motion(c) => {
                        self.apply_operator(op, Key::Char(c), text);
                    }
                    Target::Obj(inner, oc) => {
                        self.awaiting_object = Some(inner);
                        self.apply_operator(op, Key::Char(oc), text);
                    }
                }
            }
        }
    }

    // ------------------------------------------------------------- visual

    fn handle_visual(&mut self, key: Key, text: &mut String) -> bool {
        let mut chars: Vec<char> = text.chars().collect();
        match key {
            Key::Esc => {
                self.mode = Mode::Normal;
                true
            }
            Key::Left | Key::Char('h') => {
                self.cursor = clamp_normal(&chars, self.cursor.saturating_sub(1));
                true
            }
            Key::Right | Key::Char('l') => {
                let (_, end) = line_bounds(&chars, self.cursor);
                self.cursor = (self.cursor + 1).min(if end > 0 { end - 1 } else { 0 });
                true
            }
            Key::Down | Key::Char('j') => {
                self.cursor = move_vertical(&chars, self.cursor, 1, false);
                true
            }
            Key::Up | Key::Char('k') => {
                self.cursor = move_vertical(&chars, self.cursor, -1, false);
                true
            }
            Key::Char('w') => {
                self.cursor = next_word_start(&chars, self.cursor);
                true
            }
            Key::Char('b') => {
                self.cursor = prev_word_start(&chars, self.cursor);
                true
            }
            Key::Char('0') => {
                self.cursor = line_bounds(&chars, self.cursor).0;
                true
            }
            Key::Char('$') => {
                let (_, end) = line_bounds(&chars, self.cursor);
                self.cursor = if end > 0 { end - 1 } else { 0 };
                true
            }
            Key::Char('d') | Key::Char('x') => {
                let (a, b) = ordered(self.anchor, self.cursor);
                let b = (b + 1).min(chars.len());
                self.register = chars[a..b].iter().collect();
                self.register_linewise = false;
                self.snapshot(text);
                remove_range(text, a, b);
                self.cursor = clamp_normal(&text.chars().collect::<Vec<_>>(), a);
                self.mode = Mode::Normal;
                true
            }
            Key::Char('y') => {
                let (a, b) = ordered(self.anchor, self.cursor);
                let b = (b + 1).min(chars.len());
                self.register = chars[a..b].iter().collect();
                self.register_linewise = false;
                self.mode = Mode::Normal;
                true
            }
            _ => {
                // Ignore unknown keys but keep selection valid.
                chars.clear();
                true
            }
        }
    }
}

fn ordered(a: usize, b: usize) -> (usize, usize) {
    (a.min(b), a.max(b))
}

fn clamp_normal(chars: &[char], i: usize) -> usize {
    if chars.is_empty() {
        0
    } else {
        i.min(chars.len() - 1)
    }
}

fn line_bounds(chars: &[char], i: usize) -> (usize, usize) {
    let i = i.min(chars.len());
    let start = chars[..i]
        .iter()
        .rposition(|&c| c == '\n')
        .map_or(0, |p| p + 1);
    let end = chars[i..]
        .iter()
        .position(|&c| c == '\n')
        .map_or(chars.len(), |p| i + p);
    (start, end)
}

fn total_lines(chars: &[char]) -> usize {
    if chars.is_empty() {
        return 1;
    }
    chars.iter().filter(|&&c| c == '\n').count() + 1
}

fn next_line(chars: &[char], i: usize) -> usize {
    let (_, end) = line_bounds(chars, i);
    if end < chars.len() {
        end + 1
    } else {
        end
    }
}

fn nth_line_start(chars: &[char], n: usize) -> usize {
    let mut idx = 0;
    for _ in 0..n {
        let nl = next_line(chars, idx);
        if nl == idx {
            break;
        }
        idx = nl;
    }
    idx
}

fn first_non_blank(chars: &[char], start: usize, end: usize) -> usize {
    (start..end)
        .find(|&i| !chars[i].is_whitespace())
        .unwrap_or(start)
}

fn move_vertical(chars: &[char], i: usize, delta: i32, allow_end: bool) -> usize {
    let (start, _) = line_bounds(chars, i);
    let col = i - start;
    let last_line = total_lines(chars).saturating_sub(1);
    let mut line = 0;
    {
        let mut idx = 0;
        while idx < start && line < last_line {
            idx = next_line(chars, idx);
            line += 1;
        }
    }
    let target_line = (line as i32 + delta).clamp(0, last_line as i32) as usize;
    let tstart = nth_line_start(chars, target_line);
    let (_, tend) = line_bounds(chars, tstart);
    let max_col = if allow_end {
        tend - tstart
    } else {
        (tend - tstart).saturating_sub(1)
    };
    (tstart + col.min(max_col)).min(chars.len())
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

fn next_word_start(chars: &[char], i: usize) -> usize {
    let n = chars.len();
    let mut j = i;
    // Skip the current word.
    while j < n && is_word(chars[j]) {
        j += 1;
    }
    // Skip non-word (spaces/punct) to next word start.
    while j < n && !is_word(chars[j]) {
        if chars[j] == '\n' {
            j += 1;
            break;
        }
        j += 1;
    }
    j.min(n)
}

fn prev_word_start(chars: &[char], i: usize) -> usize {
    let mut j = i;
    while j > 0 && !is_word(chars[j - 1]) {
        j -= 1;
    }
    while j > 0 && is_word(chars[j - 1]) {
        j -= 1;
    }
    j
}

fn word_end(chars: &[char], i: usize) -> usize {
    let n = chars.len();
    let mut j = i;
    if j >= n {
        return n.saturating_sub(1);
    }
    while j < n && !is_word(chars[j]) {
        j += 1;
    }
    while j + 1 < n && is_word(chars[j + 1]) {
        j += 1;
    }
    j.min(n.saturating_sub(1))
}

fn insert_char(text: &mut String, at: usize, c: char) {
    let mut chars: Vec<char> = text.chars().collect();
    let at = at.min(chars.len());
    chars.insert(at, c);
    *text = chars.into_iter().collect();
}

fn insert_str(text: &mut String, at: usize, s: &str) {
    let mut chars: Vec<char> = text.chars().collect();
    let at = at.min(chars.len());
    for (k, c) in s.chars().enumerate() {
        chars.insert(at + k, c);
    }
    *text = chars.into_iter().collect();
}

fn remove_range(text: &mut String, a: usize, b: usize) {
    let mut chars: Vec<char> = text.chars().collect();
    let a = a.min(chars.len());
    let b = b.min(chars.len());
    if a < b {
        chars.drain(a..b);
    }
    *text = chars.into_iter().collect();
}

/// Find the range `[start, end)` of a text object.
fn find_text_object(
    chars: &[char],
    cursor: usize,
    inner: bool,
    obj: char,
) -> Option<(usize, usize)> {
    match obj {
        'w' => word_object(chars, cursor, inner),
        '"' | '\'' | '`' => quote_object(chars, cursor, inner, obj),
        '(' | ')' | 'b' => bracket_object(chars, cursor, inner, '(', ')'),
        '[' | ']' => bracket_object(chars, cursor, inner, '[', ']'),
        '{' | '}' | 'B' => bracket_object(chars, cursor, inner, '{', '}'),
        '<' | '>' => bracket_object(chars, cursor, inner, '<', '>'),
        'p' => paragraph_object(chars, cursor, inner),
        _ => None,
    }
}

fn word_object(chars: &[char], cursor: usize, inner: bool) -> Option<(usize, usize)> {
    let n = chars.len();
    if n == 0 {
        return None;
    }
    let mut i = cursor.min(n - 1);
    while i < n && !is_word(chars[i]) {
        i += 1;
    }
    if i >= n {
        return None;
    }
    let mut s = i;
    while s > 0 && is_word(chars[s - 1]) {
        s -= 1;
    }
    let mut e = i;
    while e < n && is_word(chars[e]) {
        e += 1;
    }
    if inner {
        Some((s, e))
    } else {
        let mut ee = e;
        while ee < n && chars[ee].is_whitespace() {
            ee += 1;
        }
        if ee == e {
            let mut ss = s;
            while ss > 0 && chars[ss - 1].is_whitespace() {
                ss -= 1;
            }
            Some((ss, e))
        } else {
            Some((s, ee))
        }
    }
}

fn quote_object(chars: &[char], cursor: usize, inner: bool, q: char) -> Option<(usize, usize)> {
    let n = chars.len();
    let mut open = None;
    let mut j = (cursor + 1).min(n);
    while j > 0 {
        j -= 1;
        if chars[j] == q {
            open = Some(j);
            break;
        }
        if chars[j] == '\n' {
            break;
        }
    }
    let open = open?;
    let mut close = None;
    let mut k = open + 1;
    while k < n {
        if chars[k] == q {
            close = Some(k);
            break;
        }
        if chars[k] == '\n' {
            break;
        }
        k += 1;
    }
    let close = close?;
    if inner {
        Some((open + 1, close))
    } else {
        Some((open, close + 1))
    }
}

fn bracket_object(
    chars: &[char],
    cursor: usize,
    inner: bool,
    open: char,
    close: char,
) -> Option<(usize, usize)> {
    let n = chars.len();
    let mut o = None;
    let mut depth = 0i32;
    let mut j = (cursor + 1).min(n);
    while j > 0 {
        j -= 1;
        if chars[j] == close {
            depth += 1;
        } else if chars[j] == open {
            if depth == 0 {
                o = Some(j);
                break;
            }
            depth -= 1;
        }
    }
    let o = o?;
    let mut depth = 0i32;
    let mut c = None;
    let mut k = o + 1;
    while k < n {
        if chars[k] == open {
            depth += 1;
        } else if chars[k] == close {
            if depth == 0 {
                c = Some(k);
                break;
            }
            depth -= 1;
        }
        k += 1;
    }
    let c = c?;
    if inner {
        Some((o + 1, c))
    } else {
        Some((o, c + 1))
    }
}

fn paragraph_object(chars: &[char], cursor: usize, inner: bool) -> Option<(usize, usize)> {
    let n = chars.len();
    if n == 0 {
        return None;
    }
    let blank = |a: usize, b: usize| chars[a..b].iter().all(|c| c.is_whitespace());
    let (mut s, _) = line_bounds(chars, cursor);
    while s > 0 {
        let (pls, ple) = line_bounds(chars, s - 1);
        if blank(pls, ple) {
            break;
        }
        s = pls;
    }
    let (_, mut e) = line_bounds(chars, cursor);
    while e < n {
        let nls = e + 1;
        if nls >= n {
            e = n;
            break;
        }
        let (nls2, nle) = line_bounds(chars, nls);
        if blank(nls2, nle) {
            break;
        }
        e = nle;
    }
    if inner {
        Some((s, e))
    } else {
        Some((s, (e + 1).min(n)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(v: &mut Vim, text: &mut String, seq: &str) {
        for c in seq.chars() {
            let k = match c {
                '\n' => Key::Enter,
                '\u{1b}' => Key::Esc,
                other => Key::Char(other),
            };
            v.handle(k, text);
        }
    }

    #[test]
    fn insert_then_escape() {
        let mut v = Vim::new();
        let mut t = String::new();
        keys(&mut v, &mut t, "ihello\u{1b}");
        assert_eq!(t, "hello");
        assert_eq!(v.mode, Mode::Normal);
        assert_eq!(v.cursor, 4);
    }

    #[test]
    fn motions_w_b_e_and_bounds() {
        let mut v = Vim::new();
        let mut t = "foo bar baz".to_string();
        keys(&mut v, &mut t, "w");
        assert_eq!(v.cursor, 4); // 'b' of bar
        keys(&mut v, &mut t, "w");
        assert_eq!(v.cursor, 8); // 'b' of baz
        keys(&mut v, &mut t, "b");
        assert_eq!(v.cursor, 4);
        keys(&mut v, &mut t, "0");
        assert_eq!(v.cursor, 0);
        keys(&mut v, &mut t, "$");
        assert_eq!(v.cursor, 10); // last char 'z'
    }

    #[test]
    fn delete_word_and_line() {
        let mut v = Vim::new();
        let mut t = "foo bar baz".to_string();
        keys(&mut v, &mut t, "dw");
        assert_eq!(t, "bar baz");
        keys(&mut v, &mut t, "dd");
        assert_eq!(t, "");
    }

    #[test]
    fn delete_char_with_count() {
        let mut v = Vim::new();
        let mut t = "hello".to_string();
        keys(&mut v, &mut t, "3x");
        assert_eq!(t, "lo");
    }

    #[test]
    fn yank_and_paste_linewise() {
        let mut v = Vim::new();
        let mut t = "a\nb".to_string();
        keys(&mut v, &mut t, "yy");
        keys(&mut v, &mut t, "p");
        assert_eq!(t, "a\na\nb");
    }

    #[test]
    fn open_below_and_type() {
        let mut v = Vim::new();
        let mut t = "a".to_string();
        keys(&mut v, &mut t, "ob\u{1b}");
        assert_eq!(t, "a\nb");
    }

    #[test]
    fn undo_restores_previous_text() {
        let mut v = Vim::new();
        let mut t = "hello".to_string();
        keys(&mut v, &mut t, "dd");
        assert_eq!(t, "");
        keys(&mut v, &mut t, "u");
        assert_eq!(t, "hello");
    }

    #[test]
    fn change_inner_word() {
        let mut v = Vim::new();
        let mut t = "foo bar".to_string();
        keys(&mut v, &mut t, "ciwX\u{1b}");
        assert_eq!(t, "X bar");
    }

    #[test]
    fn delete_inside_parens() {
        let mut v = Vim::new();
        let mut t = "a(bc)d".to_string();
        // Move to just after '(' then di(
        keys(&mut v, &mut t, "l");
        keys(&mut v, &mut t, "di(");
        assert_eq!(t, "a()d");
    }

    #[test]
    fn delete_around_quotes() {
        let mut v = Vim::new();
        let mut t = "say \"hi\" now".to_string();
        // cursor starts on 's'; one `w` lands on the opening quote
        keys(&mut v, &mut t, "w");
        keys(&mut v, &mut t, "da\"");
        assert_eq!(t, "say  now");
    }

    #[test]
    fn dot_repeats_delete() {
        let mut v = Vim::new();
        let mut t = "foo bar baz".to_string();
        keys(&mut v, &mut t, "dw");
        assert_eq!(t, "bar baz");
        keys(&mut v, &mut t, ".");
        assert_eq!(t, "baz");
    }

    #[test]
    fn dot_repeats_insert() {
        let mut v = Vim::new();
        let mut t = "ab".to_string();
        keys(&mut v, &mut t, "iX\u{1b}");
        assert_eq!(t, "Xab");
        keys(&mut v, &mut t, ".");
        assert_eq!(t, "XXab");
    }
}
