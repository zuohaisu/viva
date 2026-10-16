//! Keyboard copy mode for one pane.
//!
//! Movement, search, and selection follow the herdr keyboard page. The pane
//! process is never paused: this state only reads the current text.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchDir {
    Forward,
    Backward,
}

#[derive(Debug, Clone)]
struct Search {
    query: String,
    dir: SearchDir,
}

#[derive(Debug, Clone)]
struct QueryEdit {
    dir: SearchDir,
    text: String,
}

/// Cursor and selection over a live text grid. Coordinates are character
/// indexes into `lines`, not screen cells of a frozen snapshot.
#[derive(Debug, Clone, Default)]
pub struct CopyMode {
    row: usize,
    col: usize,
    anchor: Option<(usize, usize)>,
    linewise: bool,
    search: Option<Search>,
    query: Option<QueryEdit>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CopyEffect {
    Stay,
    Exit,
    Copied(String),
}

impl CopyMode {
    pub fn place_at_end(&mut self, lines: &[String]) {
        if lines.is_empty() {
            self.row = 0;
            self.col = 0;
            return;
        }
        self.row = lines.len() - 1;
        self.col = lines[self.row].chars().count();
    }

    #[allow(dead_code)]
    pub fn cursor(&self) -> (usize, usize) {
        (self.row, self.col)
    }

    #[allow(dead_code)]
    pub fn query_text(&self) -> Option<&str> {
        self.query.as_ref().map(|query| query.text.as_str())
    }

    /// Handle one key that is not the workbench prefix. `page` is the viewport
    /// height in rows.
    pub fn on_key(&mut self, key: KeyEvent, lines: &[String], page: usize) -> CopyEffect {
        if let Some(query) = self.query.as_mut() {
            match key.code {
                KeyCode::Esc => {
                    self.query = None;
                    return CopyEffect::Stay;
                }
                KeyCode::Enter => {
                    let text = query.text.clone();
                    let dir = query.dir;
                    self.query = None;
                    if !text.is_empty() {
                        self.search = Some(Search { query: text, dir });
                        self.jump_search(lines, false);
                    }
                    return CopyEffect::Stay;
                }
                KeyCode::Backspace => {
                    query.text.pop();
                    return CopyEffect::Stay;
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    query.text.push(c);
                    return CopyEffect::Stay;
                }
                _ => return CopyEffect::Stay,
            }
        }
        let page = page.max(1);
        match key.code {
            KeyCode::Char('q') if key.modifiers.is_empty() => return CopyEffect::Exit,
            KeyCode::Esc => {
                if self.search.take().is_some() || self.anchor.take().is_some() {
                    self.linewise = false;
                    return CopyEffect::Stay;
                }
                return CopyEffect::Exit;
            }
            KeyCode::Char('/') if key.modifiers.is_empty() => {
                self.query = Some(QueryEdit {
                    dir: SearchDir::Forward,
                    text: String::new(),
                });
                return CopyEffect::Stay;
            }
            KeyCode::Char('?')
                if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
            {
                self.query = Some(QueryEdit {
                    dir: SearchDir::Backward,
                    text: String::new(),
                });
                return CopyEffect::Stay;
            }
            KeyCode::Char('n') if key.modifiers.is_empty() => {
                self.jump_search(lines, false);
                return CopyEffect::Stay;
            }
            KeyCode::Char('N')
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || key.code == KeyCode::Char('N') =>
            {
                self.jump_search(lines, true);
                return CopyEffect::Stay;
            }
            KeyCode::Char('v') | KeyCode::Char(' ') if key.modifiers.is_empty() => {
                self.anchor = Some((self.row, self.col));
                self.linewise = false;
                return CopyEffect::Stay;
            }
            KeyCode::Char('V')
                if key.modifiers.contains(KeyModifiers::SHIFT)
                    || matches!(key.code, KeyCode::Char('V')) =>
            {
                self.anchor = Some((self.row, 0));
                self.linewise = true;
                return CopyEffect::Stay;
            }
            KeyCode::Char('y') if key.modifiers.is_empty() => {
                return CopyEffect::Copied(self.selection_text(lines));
            }
            KeyCode::Enter => return CopyEffect::Copied(self.selection_text(lines)),
            _ => {}
        }
        self.move_key(key, lines, page);
        CopyEffect::Stay
    }

    fn move_key(&mut self, key: KeyEvent, lines: &[String], page: usize) {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Char('h') | KeyCode::Left if key.modifiers.is_empty() => {
                self.step_col(lines, -1)
            }
            KeyCode::Char('l') | KeyCode::Right if key.modifiers.is_empty() => {
                self.step_col(lines, 1)
            }
            KeyCode::Char('j') | KeyCode::Down if key.modifiers.is_empty() => {
                self.step_row(lines, 1)
            }
            KeyCode::Char('k') | KeyCode::Up if key.modifiers.is_empty() => {
                self.step_row(lines, -1)
            }
            KeyCode::Char('w') if key.modifiers.is_empty() => self.word(lines, true, false, false),
            KeyCode::Char('b') if key.modifiers.is_empty() => self.word(lines, false, false, false),
            KeyCode::Char('e') if key.modifiers.is_empty() => self.word(lines, true, false, true),
            KeyCode::Char('W') => self.word(lines, true, true, false),
            KeyCode::Char('B') => self.word(lines, false, true, false),
            KeyCode::Char('E') => self.word(lines, true, true, true),
            KeyCode::Char('{') => self.paragraph(lines, -1),
            KeyCode::Char('}') => self.paragraph(lines, 1),
            KeyCode::PageUp => self.page(lines, -(page as isize)),
            KeyCode::PageDown => self.page(lines, page as isize),
            KeyCode::Char('f') if ctrl => self.page(lines, page as isize),
            KeyCode::Char('b') if ctrl => self.page(lines, -(page as isize)),
            KeyCode::Char('u') if ctrl => self.page(lines, -((page / 2).max(1) as isize)),
            KeyCode::Char('d') if ctrl => self.page(lines, (page / 2).max(1) as isize),
            _ => {}
        }
    }

    fn step_col(&mut self, lines: &[String], delta: isize) {
        let width = self.line_len(lines, self.row);
        let next = self.col as isize + delta;
        self.col = next.clamp(0, width as isize) as usize;
    }

    fn step_row(&mut self, lines: &[String], delta: isize) {
        if lines.is_empty() {
            return;
        }
        let next = self.row as isize + delta;
        self.row = next.clamp(0, lines.len() as isize - 1) as usize;
        self.col = self.col.min(self.line_len(lines, self.row));
    }

    fn page(&mut self, lines: &[String], delta: isize) {
        self.step_row(lines, delta);
    }

    fn paragraph(&mut self, lines: &[String], delta: isize) {
        if lines.is_empty() {
            return;
        }
        let mut row = self.row as isize;
        loop {
            row += delta;
            if row < 0 || row >= lines.len() as isize {
                self.row = row.clamp(0, lines.len() as isize - 1) as usize;
                self.col = 0;
                return;
            }
            if lines[row as usize].trim().is_empty() {
                self.row = row as usize;
                self.col = 0;
                return;
            }
        }
    }

    fn word(&mut self, lines: &[String], forward: bool, big: bool, end: bool) {
        if lines.is_empty() {
            return;
        }
        let mut row = self.row;
        let mut col = self.col;
        let is_word = |c: char| {
            if big {
                !c.is_whitespace()
            } else {
                c.is_ascii_alphanumeric() || c == '_'
            }
        };
        if forward {
            if end {
                let chars: Vec<char> = lines[row].chars().collect();
                if col < chars.len() && is_word(chars[col]) {
                    while col + 1 < chars.len() && is_word(chars[col + 1]) {
                        col += 1;
                    }
                }
                let mut next = col + 1;
                while row < lines.len() {
                    let chars: Vec<char> = lines[row].chars().collect();
                    while next < chars.len() && !is_word(chars[next]) {
                        next += 1;
                    }
                    if next < chars.len() {
                        while next + 1 < chars.len() && is_word(chars[next + 1]) {
                            next += 1;
                        }
                        self.row = row;
                        self.col = next;
                        return;
                    }
                    row += 1;
                    next = 0;
                }
            } else {
                let mut next_row = row;
                let mut next_col = col;
                let current: Vec<char> = lines[row].chars().collect();
                if next_col < current.len() && is_word(current[next_col]) {
                    while next_col < current.len() && is_word(current[next_col]) {
                        next_col += 1;
                    }
                } else {
                    next_col += 1;
                }
                while next_row < lines.len() {
                    let chars: Vec<char> = lines[next_row].chars().collect();
                    while next_col < chars.len() && !is_word(chars[next_col]) {
                        next_col += 1;
                    }
                    if next_col < chars.len() {
                        self.row = next_row;
                        self.col = next_col;
                        return;
                    }
                    next_row += 1;
                    next_col = 0;
                }
            }
        } else {
            if col == 0 {
                if row == 0 {
                    return;
                }
                row -= 1;
                col = self.line_len(lines, row);
            }
            let chars: Vec<char> = lines[row].chars().collect();
            let mut index = col.min(chars.len());
            while index > 0 && !is_word(chars[index - 1]) {
                index -= 1;
            }
            while index > 0 && is_word(chars[index - 1]) {
                index -= 1;
            }
            self.row = row;
            self.col = index;
        }
    }

    fn jump_search(&mut self, lines: &[String], opposite: bool) {
        let Some(search) = self.search.clone() else {
            return;
        };
        let forward = matches!(
            (search.dir, opposite),
            (SearchDir::Forward, false) | (SearchDir::Backward, true)
        );
        let sensitive = search.query.chars().any(|c| c.is_uppercase());
        let needle = if sensitive {
            search.query.clone()
        } else {
            search.query.to_lowercase()
        };
        if needle.is_empty() || lines.is_empty() {
            return;
        }
        let start = self.row;
        let mut row = start;
        loop {
            let delta: isize = if forward { 1 } else { -1 };
            let next = row as isize + delta;
            if next < 0 || next >= lines.len() as isize {
                return;
            }
            row = next as usize;
            let hay = if sensitive {
                lines[row].clone()
            } else {
                lines[row].to_lowercase()
            };
            if let Some(index) = hay.find(&needle) {
                self.row = row;
                self.col = lines[row][..index].chars().count();
                return;
            }
            if row == start {
                return;
            }
        }
    }

    fn selection_text(&self, lines: &[String]) -> String {
        if lines.is_empty() {
            return String::new();
        }
        let (mut r1, mut c1) = self.anchor.unwrap_or((self.row, self.col));
        let (mut r2, mut c2) = (self.row, self.col);
        if (r1, c1) > (r2, c2) {
            std::mem::swap(&mut r1, &mut r2);
            std::mem::swap(&mut c1, &mut c2);
        }
        if self.linewise {
            c1 = 0;
            c2 = self.line_len(lines, r2);
        }
        if r1 == r2 {
            return slice_chars(&lines[r1.min(lines.len() - 1)], c1, c2);
        }
        let mut out = Vec::new();
        out.push(slice_chars(&lines[r1], c1, usize::MAX));
        for line in lines.iter().take(r2).skip(r1 + 1) {
            out.push(line.clone());
        }
        if r2 < lines.len() {
            out.push(slice_chars(&lines[r2], 0, c2));
        }
        out.join("\n")
    }

    fn line_len(&self, lines: &[String], row: usize) -> usize {
        lines.get(row).map(|line| line.chars().count()).unwrap_or(0)
    }
}

fn slice_chars(line: &str, start: usize, end: usize) -> String {
    line.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::empty())
    }

    fn lines() -> Vec<String> {
        vec![
            "alpha beta".into(),
            String::new(),
            "Gamma delta".into(),
            "tail".into(),
        ]
    }

    #[test]
    fn motions_search_and_copy_read_live_text() {
        let mut mode = CopyMode::default();
        let mut text = lines();
        mode.on_key(key(KeyCode::Char('l')), &text, 2);
        assert_eq!(mode.cursor(), (0, 1));
        mode.on_key(key(KeyCode::Char('w')), &text, 2);
        assert_eq!(mode.cursor(), (0, 6));
        mode.on_key(key(KeyCode::Char('}')), &text, 2);
        assert_eq!(mode.cursor().0, 1);
        mode.on_key(key(KeyCode::Char('/')), &text, 2);
        mode.on_key(key(KeyCode::Char('g')), &text, 2);
        assert_eq!(mode.query_text(), Some("g"));
        mode.on_key(key(KeyCode::Enter), &text, 2);
        assert_eq!(mode.cursor().0, 2, "lowercase query matches Gamma");
        mode.on_key(key(KeyCode::Char('n')), &text, 2);
        mode.on_key(key(KeyCode::Char('v')), &text, 2);
        mode.on_key(key(KeyCode::Char('e')), &text, 2);
        text[2] = "Gamma other".into();
        match mode.on_key(key(KeyCode::Char('y')), &text, 2) {
            CopyEffect::Copied(copied) => assert!(copied.starts_with("Gamma"), "{copied}"),
            other => panic!("expected a copy, got {other:?}"),
        }
    }

    #[test]
    fn escape_clears_selection_before_exit_and_q_exits() {
        let mut mode = CopyMode::default();
        let text = lines();
        mode.on_key(key(KeyCode::Char('v')), &text, 2);
        assert_eq!(mode.on_key(key(KeyCode::Esc), &text, 2), CopyEffect::Stay);
        assert_eq!(
            mode.on_key(key(KeyCode::Char('q')), &text, 2),
            CopyEffect::Exit
        );
    }
}
