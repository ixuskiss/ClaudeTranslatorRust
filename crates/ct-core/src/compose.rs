//! Assembles the replacement text: original block (if any) followed by translation blocks.

/// Joins the original text (when present) and the translations into one replacement string.
///
/// Each block has trailing `\r`/`\n` removed, then blocks are joined with a blank line (`\n\n`).
/// Inner line breaks and leading whitespace are preserved.
pub fn compose_replacement(original: Option<&str>, translations: &[String]) -> String {
    original
        .into_iter()
        .chain(translations.iter().map(String::as_str))
        .map(|block| block.trim_end_matches(['\r', '\n']))
        .filter(|block| !block.is_empty())
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_without_original_single_is_identity() {
        assert_eq!(compose_replacement(None, &["Hello".into()]), "Hello");
    }

    #[test]
    fn compose_with_original_and_two_translations() {
        assert_eq!(
            compose_replacement(Some("안녕\n"), &["Hello\n\n".into(), "こんにちは".into()]),
            "안녕\n\nHello\n\nこんにちは"
        );
    }

    #[test]
    fn compose_keeps_inner_line_breaks() {
        assert_eq!(
            compose_replacement(None, &["a\nb".into(), "c".into()]),
            "a\nb\n\nc"
        );
    }

    #[test]
    fn compose_trims_crlf_only_at_block_end() {
        // Review Focus 2
        assert_eq!(
            compose_replacement(Some("가\r\n나\r\n"), &["A\r\nB\r\n".into()]),
            "가\r\n나\n\nA\r\nB"
        );
    }

    #[test]
    fn compose_keeps_leading_whitespace() {
        assert_eq!(
            compose_replacement(Some("  가"), &["A".into()]),
            "  가\n\nA"
        );
    }
}
