//! Display-width text and list windows shared by the shell surfaces.
//!
//! Truncation and scrolling count terminal columns, not Unicode scalars.
//! A CJK character or emoji is two columns; cutting by `chars().take`
//! overflows the cell it was supposed to stay inside.

use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

pub(crate) fn display_width(text: &str) -> usize {
    UnicodeWidthStr::width(text)
}

/// Keep a prefix whose display width is at most `max_width`. When the text
/// does not fit, the last column is an ellipsis.
pub(crate) fn truncate_end(text: &str, max_width: usize) -> String {
    if display_width(text) <= max_width {
        return text.to_string();
    }
    if max_width == 0 {
        return String::new();
    }
    let ellipsis = "…";
    let ellipsis_width = display_width(ellipsis);
    if max_width <= ellipsis_width {
        return ellipsis.to_string();
    }
    let prefix = take_prefix_width(text, max_width - ellipsis_width);
    format!("{prefix}{ellipsis}")
}

pub(crate) fn take_prefix_width(text: &str, max_width: usize) -> String {
    let mut output = String::new();
    let mut width = 0usize;
    for ch in text.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if ch_width == 0 {
            output.push(ch);
            continue;
        }
        if width + ch_width > max_width {
            break;
        }
        output.push(ch);
        width += ch_width;
    }
    output
}

/// First row index so `selected` sits inside a window of `height` rows.
/// The window follows the selection: `j` to the last item brings that item
/// on screen instead of leaving the viewport on the first page.
pub(crate) fn scroll_to_reveal(len: usize, selected: usize, height: usize) -> usize {
    if height == 0 || len == 0 {
        return 0;
    }
    let selected = selected.min(len - 1);
    let max_start = len.saturating_sub(height);
    selected
        .saturating_sub(height.saturating_sub(1))
        .min(max_start)
}

/// A requested first row, clamped so `anchor` (first line, last line of the
/// selected block) stays in the window. A request that already shows the
/// block is kept. Without an anchor the request only clamps to the list.
pub(crate) fn scroll_window(
    len: usize,
    requested: usize,
    anchor: Option<(usize, usize)>,
    height: usize,
) -> usize {
    if height == 0 || len == 0 {
        return 0;
    }
    let max_start = len.saturating_sub(height);
    let requested = requested.min(max_start);
    let Some((first, last)) = anchor else {
        return requested;
    };
    let first = first.min(len - 1);
    let last = last.clamp(first, len - 1);
    let block = last - first + 1;
    if block >= height {
        return first.min(max_start);
    }
    let low = last.saturating_add(1).saturating_sub(height);
    let high = first.min(max_start);
    requested.clamp(low, high.max(low))
}

/// Characters whose display cells overlap `[start, start + count)`.
/// A wide character is kept whole when either of its columns is inside the span.
pub(crate) fn slice_display_span(line: &str, start: usize, count: usize) -> String {
    if count == 0 || line.is_empty() {
        return String::new();
    }
    let end = start.saturating_add(count);
    let mut out = String::new();
    let mut col = 0usize;
    let mut include_following_marks = false;
    for ch in line.chars() {
        let width = UnicodeWidthChar::width(ch).unwrap_or(0);
        if width == 0 {
            if include_following_marks {
                out.push(ch);
            }
            continue;
        }
        let overlaps = col < end && col + width > start;
        if overlaps {
            out.push(ch);
        }
        include_following_marks = overlaps;
        col += width;
    }
    out
}

/// The token under a terminal display column.
///
/// Columns are display cells, so the second column of a fullwidth comma is
/// still that comma. Whitespace and herdr's delimiter set split tokens.
/// URLs and quoted paths stay intact, including the spaces inside the quotes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WordSpan {
    pub text: String,
    pub start: u16,
    pub end: u16,
}

pub(crate) fn word_span_at_column(line: &str, col: u16) -> Option<WordSpan> {
    let (start, end) = word_bounds_at_column(line, col)?;
    let text = text_in_cell_range(line, start, end);
    if text.is_empty() {
        return None;
    }
    Some(WordSpan { text, start, end })
}

#[derive(Clone, Copy)]
struct TextCell {
    ch: char,
    start_col: u16,
    end_col: u16,
}

struct CellSpan {
    start: usize,
    end: usize,
}

impl CellSpan {
    fn contains(&self, idx: usize) -> bool {
        idx >= self.start && idx <= self.end
    }

    fn columns(&self, cells: &[TextCell]) -> (u16, u16) {
        (cells[self.start].start_col, cells[self.end].end_col)
    }
}

fn word_bounds_at_column(line: &str, col: u16) -> Option<(u16, u16)> {
    let cells = text_cells(line);
    let clicked = cell_index_at_column(&cells, col)?;
    let span = url_span_at_column(&cells, clicked)
        .or_else(|| quoted_path_span_at_column(&cells, clicked))
        .or_else(|| token_span_at_column(&cells, clicked))?;
    Some(span.columns(&cells))
}

fn text_in_cell_range(line: &str, start_col: u16, end_col: u16) -> String {
    text_cells(line)
        .into_iter()
        .filter(|cell| cell.start_col >= start_col && cell.end_col <= end_col)
        .map(|cell| cell.ch)
        .collect()
}

fn text_cells(line: &str) -> Vec<TextCell> {
    let mut next_col = 0u16;
    line.chars()
        .map(|ch| {
            let width = u16::try_from(UnicodeWidthChar::width(ch).unwrap_or(0)).unwrap_or(u16::MAX);
            let start_col = if width == 0 {
                next_col.saturating_sub(1)
            } else {
                next_col
            };
            if width > 0 {
                next_col = next_col.saturating_add(width);
            }
            TextCell {
                ch,
                start_col,
                end_col: next_col.saturating_sub(1),
            }
        })
        .collect()
}

fn cell_index_at_column(cells: &[TextCell], col: u16) -> Option<usize> {
    cells
        .iter()
        .position(|cell| cell.start_col <= col && col <= cell.end_col)
}

fn token_span_at_column(cells: &[TextCell], clicked: usize) -> Option<CellSpan> {
    if is_word_separator(cells[clicked].ch) {
        return None;
    }
    let mut start = clicked;
    while start > 0 && !is_word_separator(cells[start - 1].ch) {
        start -= 1;
    }
    let mut end = clicked;
    while end + 1 < cells.len() && !is_word_separator(cells[end + 1].ch) {
        end += 1;
    }
    trim_token_edges(cells, CellSpan { start, end }).filter(|span| span.contains(clicked))
}

fn is_word_separator(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '|' | '('
                | ')'
                | '['
                | ']'
                | '{'
                | '}'
                | ','
                | ';'
                | '!'
                | '（'
                | '）'
                | '：'
                | '、'
                | '。'
                | '，'
        )
}

fn trim_token_edges(cells: &[TextCell], span: CellSpan) -> Option<CellSpan> {
    let mut start = span.start;
    let mut end = span.end;
    while start <= end && is_leading_token_wrapper(cells[start].ch) {
        start += 1;
    }
    if start < end && cells[end].ch == '$' && is_trailing_token_wrapper(cells[end - 1].ch) {
        end -= 1;
    }
    while start <= end && is_trailing_token_wrapper(cells[end].ch) {
        if end == 0 {
            return None;
        }
        end -= 1;
    }
    (start <= end).then_some(CellSpan { start, end })
}

fn is_leading_token_wrapper(ch: char) -> bool {
    matches!(ch, '(' | '[' | '{' | '<' | '"' | '\'' | '`')
}

fn is_trailing_token_wrapper(ch: char) -> bool {
    matches!(
        ch,
        ')' | ']' | '}' | '>' | '"' | '\'' | '`' | '.' | ',' | ';' | ':' | '!' | '?'
    )
}

fn url_span_at_column(cells: &[TextCell], clicked: usize) -> Option<CellSpan> {
    let mut start = 0;
    while start < cells.len() {
        if starts_with_chars(&cells[start..], "http://")
            || starts_with_chars(&cells[start..], "https://")
        {
            let mut end = start;
            while end + 1 < cells.len() && !cells[end + 1].ch.is_whitespace() {
                end += 1;
            }
            if clicked >= start && clicked <= end {
                let span = trim_url_edges(cells, CellSpan { start, end })?;
                return span.contains(clicked).then_some(span);
            }
            start = end + 1;
        } else {
            start += 1;
        }
    }
    None
}

fn trim_url_edges(cells: &[TextCell], span: CellSpan) -> Option<CellSpan> {
    let start = span.start;
    let mut end = span.end;
    while start <= end && should_trim_trailing_url_cell(cells, start, end) {
        if end == 0 {
            return None;
        }
        end -= 1;
    }
    (start <= end).then_some(CellSpan { start, end })
}

fn should_trim_trailing_url_cell(cells: &[TextCell], start: usize, end: usize) -> bool {
    match cells[end].ch {
        '"' | '\'' | '`' | '.' | ',' | ';' | ':' | '!' | '?' => true,
        ')' => !trailing_closer_is_balanced(cells, start, end, '(', ')'),
        ']' => !trailing_closer_is_balanced(cells, start, end, '[', ']'),
        '}' => !trailing_closer_is_balanced(cells, start, end, '{', '}'),
        _ => false,
    }
}

fn trailing_closer_is_balanced(
    cells: &[TextCell],
    start: usize,
    end: usize,
    open: char,
    close: char,
) -> bool {
    let mut balance = 0i32;
    for cell in &cells[start..end] {
        if cell.ch == open {
            balance += 1;
        } else if cell.ch == close {
            balance -= 1;
        }
    }
    balance > 0
}

fn quoted_path_span_at_column(cells: &[TextCell], clicked: usize) -> Option<CellSpan> {
    let clicked_ch = cells.get(clicked)?.ch;
    if clicked_ch == '"' || clicked_ch == '\'' || clicked_ch == '`' {
        return None;
    }
    for quote in ['"', '\'', '`'] {
        let mut open = None;
        for (idx, cell) in cells.iter().copied().enumerate() {
            if cell.ch != quote || escaped(cells, idx) {
                continue;
            }
            if let Some(start) = open {
                if clicked > start
                    && clicked < idx
                    && cells[start + 1..idx].iter().any(|cell| cell.ch == '/')
                {
                    return Some(CellSpan {
                        start: start + 1,
                        end: idx - 1,
                    });
                }
                open = None;
            } else {
                open = Some(idx);
            }
        }
    }
    None
}

fn escaped(cells: &[TextCell], idx: usize) -> bool {
    let mut slashes = 0;
    let mut cursor = idx;
    while cursor > 0 && cells[cursor - 1].ch == '\\' {
        slashes += 1;
        cursor -= 1;
    }
    slashes % 2 == 1
}

fn starts_with_chars(cells: &[TextCell], prefix: &str) -> bool {
    prefix
        .chars()
        .enumerate()
        .all(|(idx, expected)| cells.get(idx).is_some_and(|cell| cell.ch == expected))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_end_counts_cjk_and_emoji_columns() {
        let text = truncate_end("提交 herdr 的反馈", 16);
        assert_eq!(text, "提交 herdr 的反…");
        assert!(display_width(&text) <= 16);

        let marked = truncate_end("BRSEL项目🎉项目🎉项目", 10);
        assert!(display_width(&marked) <= 10, "{marked}");
        assert!(marked.ends_with('…'));
    }

    #[test]
    fn scroll_to_reveal_keeps_the_selected_row_in_the_window() {
        assert_eq!(scroll_to_reveal(60, 0, 20), 0);
        assert_eq!(scroll_to_reveal(60, 59, 20), 40);
        assert_eq!(scroll_to_reveal(40, 39, 40), 0);
        assert_eq!(scroll_to_reveal(10, 9, 0), 0);
    }

    #[test]
    fn scroll_window_moves_until_the_selection_would_leave() {
        assert_eq!(scroll_window(40, 0, Some((4, 5)), 10), 0);
        assert_eq!(scroll_window(40, 3, Some((4, 5)), 10), 3);
        assert_eq!(scroll_window(40, 30, Some((4, 5)), 10), 4);
        assert_eq!(scroll_window(40, 0, Some((30, 31)), 10), 22);
        assert_eq!(scroll_window(40, 9, None, 10), 9);
    }

    fn col_of(line: &str, needle: &str) -> u16 {
        let byte = line.find(needle).expect(needle);
        u16::try_from(display_width(&line[..byte])).unwrap_or(u16::MAX)
    }

    fn selected(line: &str, needle: &str) -> Option<String> {
        word_span_at_column(line, col_of(line, needle)).map(|span| span.text)
    }

    #[test]
    fn word_span_uses_display_columns_and_cjk_delimiters() {
        let line = "alpha，beta";
        let comma = col_of(line, "，");
        assert_eq!(selected(line, "beta").as_deref(), Some("beta"));
        assert_eq!(
            word_span_at_column(line, comma),
            None,
            "the comma is a delimiter"
        );
        assert_eq!(
            word_span_at_column(line, comma + 1),
            None,
            "the second column of ， is still the comma, not beta"
        );

        let wide = "echo 你好-world done";
        let good = col_of(wide, "好");
        assert_eq!(
            word_span_at_column(wide, good + 1).map(|span| span.text),
            Some("你好-world".into()),
            "the second column of 好 stays inside the word"
        );

        for delimiter in ['（', '）', '：', '、', '。', '，'] {
            let row = format!("left{delimiter}right");
            assert_eq!(selected(&row, "left").as_deref(), Some("left"));
            assert_eq!(selected(&row, "right").as_deref(), Some("right"));
            let at = col_of(&row, &delimiter.to_string());
            assert_eq!(word_span_at_column(&row, at), None);
            assert_eq!(word_span_at_column(&row, at + 1), None);
        }
    }

    #[test]
    fn word_span_keeps_urls_paths_and_splits_on_herdr_delimiters() {
        let cases = [
            (
                "see https://example.com/a-b_c?q=x@y.",
                "example.com",
                "https://example.com/a-b_c?q=x@y",
            ),
            (
                "see https://en.wikipedia.org/wiki/Foo_(bar_(baz)),",
                "wikipedia",
                "https://en.wikipedia.org/wiki/Foo_(bar_(baz))",
            ),
            (
                "cat \"/tmp/build output/log.txt\"",
                "output",
                "/tmp/build output/log.txt",
            ),
            (
                "open ./src/app/actions.rs:795",
                "actions",
                "./src/app/actions.rs:795",
            ),
            (
                "edit src/app/actions.rs,then",
                "actions",
                "src/app/actions.rs",
            ),
            (
                "render_status_line(app, area)",
                "render",
                "render_status_line",
            ),
            ("render_status_line(app, area)", "app", "app"),
            ("tmux|newhoo|fixhoo", "newhoo", "newhoo"),
            ("regex foo$", "foo", "foo$"),
        ];
        for (line, click, expected) in cases {
            assert_eq!(selected(line, click).as_deref(), Some(expected), "{line}");
        }
        assert_eq!(selected("alpha,beta;gamma", ","), None);
        assert_eq!(selected("alpha,beta;gamma", ";"), None);
        assert_eq!(selected("tmux|newhoo", "|"), None);
    }

    #[test]
    fn slice_display_span_keeps_wide_characters_whole() {
        assert_eq!(slice_display_span("abcdef", 0, 2), "ab");
        assert_eq!(slice_display_span("你好world", 0, 4), "你好");
        assert_eq!(slice_display_span("你好world", 1, 2), "你好");
        assert_eq!(slice_display_span("你好world", 4, 5), "world");
    }
}
