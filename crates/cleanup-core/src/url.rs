//! Cleaning for AI-provider attribution parameters embedded in URLs.
//!
//! These parameters are transport metadata, not part of the cited page.  They occur both in
//! visible prose and in structural locations (Markdown destinations and DOCX relationships),
//! so the small, dependency-free implementation lives in core and is shared by rules and
//! format adapters.  Only exact provider values are removed: a generic `utm_source` is often
//! intentional analytics metadata and must not be classified as an AI fingerprint.

use std::ops::Range;

/// A whole-URL replacement whose query string contained AI-provider attribution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlReplacement {
    pub range: Range<usize>,
    pub replacement: String,
}

/// Provider names and hostnames as they appear in `utm_*`/`referrer` values, refreshed for
/// the 2026 model crop. Values are compared exactly (ASCII-case-insensitively): a generic
/// `utm_source=newsletter` is analytics, while `utm_source=deepseek.com` is attribution.
const AI_PROVIDERS: &[&str] = &[
    // OpenAI
    "chatgpt.com",
    "chatgpt",
    "openai",
    // Microsoft
    "copilot.com",
    "copilot",
    // Perplexity
    "perplexity",
    "perplexity.ai",
    // Anthropic
    "claude",
    "claude.ai",
    "anthropic",
    // Google
    "gemini",
    "gemini.google.com",
    // xAI
    "grok",
    "grok.com",
    "x.ai",
    // 2026 additions: the rest of the current landscape.
    "deepseek",
    "deepseek.com",
    "kimi.com",
    "moonshot.ai",
    "meta.ai",
    "mistral.ai",
    "chat.mistral.ai",
    "lechat",
    "chat.qwen.ai",
    "qwen.ai",
    "qwen",
    "chatglm",
    "z.ai",
    // Aggregators and answer engines that carry their own attribution.
    "poe.com",
    "you.com",
    "phind.com",
];

fn is_ai_provider(value: &str) -> bool {
    AI_PROVIDERS
        .iter()
        .any(|provider| value.eq_ignore_ascii_case(provider))
}

fn is_ai_tracking_param(param: &str) -> bool {
    let Some((name, value)) = param.split_once('=') else {
        return false;
    };
    if name.eq_ignore_ascii_case("referrer") || name.eq_ignore_ascii_case("referer") {
        return is_ai_provider(value);
    }
    (name.eq_ignore_ascii_case("utm_source")
        || name.eq_ignore_ascii_case("utm_medium")
        || name.eq_ignore_ascii_case("utm_campaign"))
        && is_ai_provider(value)
}

/// Return the length of the query separator at `at`: either `&` or its common XML/HTML
/// spelling `&amp;`.  Keeping the original separator lets DOCX relationships remain valid XML
/// without decoding and re-serializing the attribute.
fn separator_len(query: &str, at: usize) -> Option<usize> {
    if query.as_bytes().get(at) != Some(&b'&') {
        return None;
    }
    if query
        .get(at..at + 5)
        .is_some_and(|s| s.eq_ignore_ascii_case("&amp;"))
    {
        Some(5)
    } else {
        Some(1)
    }
}

/// Remove only AI-provider attribution parameters from one URL, preserving all other query
/// parameters, their original spelling/separators, and the fragment.  Returns `None` when the
/// URL has no matching parameter.
///
/// ```
/// use cleanup_core::url::clean_ai_tracking_url;
/// assert_eq!(
///     clean_ai_tracking_url("https://e.test/?a=1&utm_source=chatgpt.com&b=2#top").as_deref(),
///     Some("https://e.test/?a=1&b=2#top")
/// );
/// ```
pub fn clean_ai_tracking_url(url: &str) -> Option<String> {
    let query_at = url.find('?')?;
    let query_start = query_at + 1;
    let query_end = url[query_start..]
        .find('#')
        .map_or(url.len(), |relative| query_start + relative);
    let query = &url[query_start..query_end];

    // `(separator before this item, item text)`.  The first separator is empty.  Splitting
    // manually rather than with `str::split` handles `&amp;` without changing any other bytes.
    let mut parts: Vec<(&str, &str)> = Vec::new();
    let mut separator = "";
    let mut item_start = 0usize;
    let mut cursor = 0usize;
    while cursor < query.len() {
        if let Some(len) = separator_len(query, cursor) {
            parts.push((separator, &query[item_start..cursor]));
            separator = &query[cursor..cursor + len];
            cursor += len;
            item_start = cursor;
        } else {
            // Advance by a whole UTF-8 character, not one byte: query values and paths may
            // legitimately contain non-ASCII text.
            cursor += query[cursor..].chars().next().map_or(1, char::len_utf8);
        }
    }
    parts.push((separator, &query[item_start..]));

    let mut cleaned_query = String::new();
    let mut kept = 0usize;
    let mut removed = false;
    for (sep, param) in parts {
        if is_ai_tracking_param(param) {
            removed = true;
            continue;
        }
        if kept > 0 {
            // An empty separator is possible only for a malformed query.  `&` is the least
            // surprising repair when a removed first item preceded such an item.
            cleaned_query.push_str(if sep.is_empty() { "&" } else { sep });
        }
        cleaned_query.push_str(param);
        kept += 1;
    }
    if !removed {
        return None;
    }

    let mut output = String::with_capacity(url.len());
    output.push_str(&url[..query_at]);
    if kept > 0 {
        output.push('?');
        output.push_str(&cleaned_query);
    }
    output.push_str(&url[query_end..]);
    Some(output)
}

fn is_url_end(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '<' | '>' | '"' | '\'' | '`' | '(' | ')' | '[' | ']' | '{' | '}'
        )
}

/// Locate HTTP(S) URLs in prose and propose replacements for those carrying known AI-provider
/// attribution.  Punctuation delimiters are excluded so ordinary sentence punctuation and XML
/// attribute quotes stay untouched.
pub fn find_ai_tracking_urls(text: &str) -> Vec<UrlReplacement> {
    // ASCII lowercasing preserves byte offsets, including when the rest of the text is Unicode.
    let lower = text.to_ascii_lowercase();
    let mut out = Vec::new();
    let mut from = 0usize;
    while let Some(relative) = lower[from..].find("http") {
        let start = from + relative;
        let is_http =
            lower[start..].starts_with("http://") || lower[start..].starts_with("https://");
        let word_prefix = start > 0
            && (lower.as_bytes()[start - 1].is_ascii_alphanumeric()
                || lower.as_bytes()[start - 1] == b'_');
        if !is_http || word_prefix {
            from = start + 4;
            continue;
        }

        let mut end = text[start..]
            .char_indices()
            .find_map(|(offset, ch)| is_url_end(ch).then_some(start + offset))
            .unwrap_or(text.len());
        // Sentence punctuation is not part of the URL in the contexts this scanner handles.
        // Keep balanced-path parentheses conservative: `is_url_end` already stops at `)`.
        while end > start && matches!(text.as_bytes()[end - 1], b'.' | b',' | b';' | b':' | b'!') {
            end -= 1;
        }
        if end > start
            && let Some(replacement) = clean_ai_tracking_url(&text[start..end])
        {
            out.push(UrlReplacement {
                range: start..end,
                replacement,
            });
        }
        from = end.max(start + 4);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_only_ai_attribution_and_preserves_url_structure() {
        for (input, expected) in [
            (
                "https://example.test/page?utm_source=chatgpt.com",
                "https://example.test/page",
            ),
            (
                "https://example.test/page?utm_source=chatgpt.com&x=1",
                "https://example.test/page?x=1",
            ),
            (
                "https://example.test/page?x=1&utm_source=copilot.com&y=2#frag",
                "https://example.test/page?x=1&y=2#frag",
            ),
            (
                "https://example.test/page?x=1&amp;utm_medium=chatgpt&amp;y=2",
                "https://example.test/page?x=1&amp;y=2",
            ),
            (
                "https://example.test/page?utm_source=chatgpt.com#frag",
                "https://example.test/page#frag",
            ),
        ] {
            assert_eq!(clean_ai_tracking_url(input).as_deref(), Some(expected));
        }
    }

    #[test]
    fn handles_unicode_query_values_without_losing_utf8_boundaries() {
        let input = "https://e.test/é?x=café&utm_source=chatgpt.com&y=東京";
        assert_eq!(
            clean_ai_tracking_url(input).as_deref(),
            Some("https://e.test/é?x=café&y=東京")
        );
    }

    #[test]
    fn leaves_ordinary_analytics_and_non_ai_urls_alone() {
        for input in [
            "https://example.test/page?utm_source=newsletter",
            "https://example.test/page?next=utm_source%3Dchatgpt.com",
            "https://example.test/page?not_utm_source=chatgpt.com",
            "https://example.test/page?x=1&y=2",
        ] {
            assert_eq!(clean_ai_tracking_url(input), None, "changed {input}");
        }
    }

    #[test]
    fn covers_the_2026_provider_set() {
        for value in [
            "deepseek.com",
            "kimi.com",
            "meta.ai",
            "mistral.ai",
            "qwen",
            "z.ai",
            "poe.com",
            "chatglm",
        ] {
            let input = format!("https://e.test/?utm_source={value}&x=1");
            assert_eq!(
                clean_ai_tracking_url(&input).as_deref(),
                Some("https://e.test/?x=1"),
                "did not clean {value}"
            );
        }
        // A provider referrer is the same kind of attribution as a provider utm_source.
        assert_eq!(
            clean_ai_tracking_url("https://e.test/?referrer=claude.ai").as_deref(),
            Some("https://e.test/")
        );
        // Bare vendor words are not enough: keep ordinary analytics.
        for input in [
            "https://e.test/?utm_source=meta",
            "https://e.test/?referrer=newsletter",
            "https://e.test/?utm_source=moonshot",
        ] {
            assert_eq!(clean_ai_tracking_url(input), None, "changed {input}");
        }
    }

    #[test]
    fn finds_urls_in_prose_without_eating_punctuation() {
        let text = "See (https://e.test/?a=1&utm_source=chatgpt.com), then \
                    https://e.test/?utm_source=newsletter.";
        let found = find_ai_tracking_urls(text);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].replacement, "https://e.test/?a=1");
        assert_eq!(
            &text[found[0].range.clone()],
            "https://e.test/?a=1&utm_source=chatgpt.com"
        );
    }
}
