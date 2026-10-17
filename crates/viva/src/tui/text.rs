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
}
