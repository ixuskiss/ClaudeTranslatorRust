//! CF_HTML (Windows "HTML Format") header helpers. Offsets are UTF-8 byte offsets.

use crate::model::SheetsParseError;

const NO_OFFSETS: &str = "CF_HTML header has no fragment offsets";
const OUT_OF_RANGE: &str = "CF_HTML fragment offsets out of range";
const PREFIX: &str = "<html><body>\r\n<!--StartFragment-->";
const SUFFIX: &str = "<!--EndFragment-->\r\n</body></html>";

/// First line of the form `{name}:{digits}` (trailing whitespace allowed).
fn header_offset(text: &str, name: &str) -> Option<Result<usize, SheetsParseError>> {
    for line in text.split('\n') {
        let Some(rest) = line.strip_prefix(name).and_then(|r| r.strip_prefix(':')) else {
            continue;
        };
        let rest = rest.trim_end();
        if !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()) {
            return Some(
                rest.parse::<usize>()
                    .map_err(|_| SheetsParseError(OUT_OF_RANGE)),
            );
        }
    }
    None
}

pub fn extract_cf_html_fragment(cf_html: &str) -> Result<String, SheetsParseError> {
    let (Some(start), Some(end)) = (
        header_offset(cf_html, "StartFragment"),
        header_offset(cf_html, "EndFragment"),
    ) else {
        return Err(SheetsParseError(NO_OFFSETS));
    };
    let (start, end) = (start?, end?);
    let bytes = cf_html.as_bytes();
    if start > end || end > bytes.len() {
        return Err(SheetsParseError(OUT_OF_RANGE));
    }
    String::from_utf8(bytes[start..end].to_vec())
        .map_err(|_| SheetsParseError("CF_HTML fragment is not valid UTF-8"))
}

pub fn build_cf_html(fragment: &str) -> String {
    // Header has four fixed 10-digit fields, so its length does not depend on the values.
    let header_len = header(0, 0, 0, 0).len();
    let start_fragment = header_len + PREFIX.len();
    let end_fragment = start_fragment + fragment.len();
    let end_html = end_fragment + SUFFIX.len();
    let mut out = header(header_len, end_html, start_fragment, end_fragment);
    out.push_str(PREFIX);
    out.push_str(fragment);
    out.push_str(SUFFIX);
    out
}

fn header(start_html: usize, end_html: usize, start_frag: usize, end_frag: usize) -> String {
    format!(
        "Version:0.9\r\nStartHTML:{start_html:010}\r\nEndHTML:{end_html:010}\r\nStartFragment:{start_frag:010}\r\nEndFragment:{end_frag:010}\r\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf_html_roundtrip_with_korean() {
        let frag = "<table><tr><td>한글</td></tr></table>";
        assert_eq!(
            extract_cf_html_fragment(&build_cf_html(frag)).as_deref(),
            Ok(frag)
        );
    }

    #[test]
    fn cf_html_offsets_are_utf8_bytes() {
        let frag = "<td>한글😀</td>";
        let html = build_cf_html(frag);
        let field = |n: &str| header_offset(&html, n).unwrap().unwrap();
        let (sf, ef, eh, sh) = (
            field("StartFragment"),
            field("EndFragment"),
            field("EndHTML"),
            field("StartHTML"),
        );
        assert_eq!(ef - sf, frag.len());
        assert!(frag.len() > frag.chars().count());
        assert_eq!(eh, html.len());
        assert_eq!(&html.as_bytes()[sf..ef], frag.as_bytes());
        assert_eq!(&html[sh..sh + 6], "<html>");
        assert!(html.starts_with("Version:0.9\r\nStartHTML:0000000"));
        assert_eq!(extract_cf_html_fragment(&html).as_deref(), Ok(frag));
    }

    #[test]
    fn cf_html_missing_offsets_rejected() {
        assert_eq!(
            extract_cf_html_fragment("Version:0.9\r\nStartFragment:0000000010\r\n"),
            Err(SheetsParseError("CF_HTML header has no fragment offsets"))
        );
        assert_eq!(
            extract_cf_html_fragment("<b>plain</b>"),
            Err(SheetsParseError("CF_HTML header has no fragment offsets"))
        );
    }

    #[test]
    fn cf_html_offsets_out_of_range_rejected() {
        let phrase = Err(SheetsParseError("CF_HTML fragment offsets out of range"));
        assert_eq!(
            extract_cf_html_fragment("StartFragment:0000000005\nEndFragment:0000000003\nabcdef"),
            phrase
        );
        assert_eq!(
            extract_cf_html_fragment("StartFragment:0000000001\nEndFragment:0000009999\n"),
            phrase
        );
        assert_eq!(
            extract_cf_html_fragment("StartFragment:99999999999999999999999\nEndFragment:1\n"),
            phrase
        );
    }
}
