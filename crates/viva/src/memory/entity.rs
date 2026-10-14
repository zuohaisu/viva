//! Lightweight entity extraction for native fact encoding (upstream store.py patterns).

pub fn extract_entities(content: &str) -> Vec<String> {
    let mut out = Vec::new();
    out.extend(quoted_entities(content, '"'));
    out.extend(quoted_entities(content, '\''));
    out.extend(capitalized_phrases(content));
    dedupe_preserve_order(out)
}

fn quoted_entities(content: &str, quote: char) -> Vec<String> {
    let mut entities = Vec::new();
    let mut rest = content;
    while let Some(start) = rest.find(quote) {
        let after = &rest[start + 1..];
        if let Some(end) = after.find(quote) {
            let inner = after[..end].trim();
            if !inner.is_empty() {
                entities.push(inner.to_string());
            }
            rest = &after[end + 1..];
        } else {
            break;
        }
    }
    entities
}

fn capitalized_phrases(content: &str) -> Vec<String> {
    let mut entities = Vec::new();
    for word in content.split_whitespace() {
        let clean: String = word
            .trim_matches(|c: char| !c.is_alphabetic())
            .chars()
            .collect();
        if clean.len() < 2 {
            continue;
        }
        if clean.chars().next().is_some_and(|c| c.is_uppercase())
            && clean.chars().skip(1).all(|c| c.is_lowercase() || c == '-')
        {
            entities.push(clean);
        }
    }
    // Pair consecutive Titlecase tokens into multi-word names.
    let words: Vec<&str> = content.split_whitespace().collect();
    let mut i = 0;
    while i + 1 < words.len() {
        let a = trim_alpha(words[i]);
        let b = trim_alpha(words[i + 1]);
        if is_title_word(a) && is_title_word(b) {
            entities.push(format!("{a} {b}"));
            i += 2;
        } else {
            i += 1;
        }
    }
    entities
}

fn trim_alpha(s: &str) -> &str {
    s.trim_matches(|c: char| !c.is_alphabetic())
}

fn is_title_word(s: &str) -> bool {
    s.len() >= 2
        && s.chars().next().is_some_and(|c| c.is_uppercase())
        && s.chars().skip(1).all(|c| c.is_lowercase())
}

fn dedupe_preserve_order(items: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for item in items {
        let key = item.to_lowercase();
        if seen.insert(key) {
            out.push(item);
        }
    }
    out
}
