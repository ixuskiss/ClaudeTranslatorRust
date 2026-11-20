//! Rule-based language detection from script / unicode-block ratios.

use crate::lang::Lang;

pub trait LanguageDetector: Send + Sync {
    fn detect(&self, text: &str) -> Option<Lang>;
}

/// Detects language from script ratios. Callers are expected to mask
/// dictionary terms (`⟦N⟧` placeholders) before detection.
pub struct RuleBasedDetector;

const HANGUL_RANGES: [(u32, u32); 3] = [(0xAC00, 0xD7A3), (0x1100, 0x11FF), (0x3130, 0x318F)];
const KANA_RANGES: [(u32, u32); 2] = [(0x3040, 0x309F), (0x30A0, 0x30FF)];
const VIETNAMESE_ONLY_RANGES: [(u32, u32); 1] = [(0x1E00, 0x1EFF)];

fn in_ranges(ch: char, ranges: &[(u32, u32)]) -> bool {
    let code = ch as u32;
    ranges.iter().any(|&(lo, hi)| lo <= code && code <= hi)
}

fn is_hangul(ch: char) -> bool {
    in_ranges(ch, &HANGUL_RANGES)
}

fn is_latin(ch: char) -> bool {
    ch.is_ascii_alphabetic()
}

/// Korean if a Hangul particle/ending is attached directly to an English word
/// ("PR의"), or if Hangul syllables are at least as many as English words.
/// A message that merely mentions a Korean name stays English.
fn is_korean_text(text: &str, hangul: usize) -> bool {
    let mut latin_words = 0usize;
    let mut has_latin = false;
    let mut has_hangul = false;
    let mut in_word = false;
    // Trailing sentinel closes the last word.
    for ch in text.chars().chain(std::iter::once(' ')) {
        if is_latin(ch) || is_hangul(ch) {
            in_word = true;
            has_latin |= is_latin(ch);
            has_hangul |= is_hangul(ch);
        } else if in_word {
            if has_latin && has_hangul {
                return true;
            }
            if has_latin {
                latin_words += 1;
            }
            in_word = false;
            has_latin = false;
            has_hangul = false;
        }
    }
    hangul >= latin_words
}

impl LanguageDetector for RuleBasedDetector {
    fn detect(&self, text: &str) -> Option<Lang> {
        let hangul = text.chars().filter(|&c| is_hangul(c)).count();
        let kana = text.chars().filter(|&c| in_ranges(c, &KANA_RANGES)).count();
        let vi_only = text
            .chars()
            .filter(|&c| in_ranges(c, &VIETNAMESE_ONLY_RANGES))
            .count();
        let latin = text.chars().filter(|&c| is_latin(c)).count();

        if hangul > 0 && is_korean_text(text, hangul) {
            return Some(Lang::Ko);
        }
        if kana > 0 {
            return Some(Lang::Ja);
        }
        if vi_only > 0 {
            return Some(Lang::Vi);
        }
        if latin > 0 {
            return Some(Lang::En);
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> Option<Lang> {
        RuleBasedDetector.detect(text)
    }

    #[test]
    fn detects_pure_korean_text() {
        assert_eq!(d("오늘 회의 자료를 검토해 주세요"), Some(Lang::Ko));
    }

    #[test]
    fn detects_pure_english_text() {
        assert_eq!(d("Please review the logical model today"), Some(Lang::En));
    }

    #[test]
    fn detects_pure_vietnamese_text() {
        assert_eq!(d("Vui lòng kiểm tra mô hình logic hôm nay"), Some(Lang::Vi));
    }

    #[test]
    fn detects_pure_japanese_text() {
        assert_eq!(d("今日中に確認をお願いします"), Some(Lang::Ja));
    }

    #[test]
    fn mixed_sentence_with_masked_terms_detects_as_korean() {
        assert_eq!(
            d("⟦0⟧에서 ⟦1⟧ 확인하고 ⟦2⟧ ⟦3⟧에 적용해주세요."),
            Some(Lang::Ko)
        );
    }

    #[test]
    fn korean_sentences_with_english_words_detect_as_korean() {
        for text in [
            "DA에서 logical model 확인하고 CLT infrastructure에 적용해주세요.",
            "내일 meeting에서 Q4 roadmap and budget planning review 할게요",
            "이 PR의 unit test coverage report를 CI pipeline에 upload해주세요",
            "오늘 회의 자료 review 부탁드립니다",
            "Thanks 안정현",
        ] {
            assert_eq!(d(text), Some(Lang::Ko), "{text}");
        }
    }

    #[test]
    fn english_text_with_a_korean_name_detects_as_english() {
        for text in [
            "Hi @Kate(Jeong-Hyeon AN) 안정현 ,\nI apologize for contacting you this late. Please help us check the following morning.\nThere were 500 attempts trying to access to JapanTrust and Senwa servers by a suspicious IP.",
            "Hi 안정현, please check the attached file",
            "Hi @Kate(Jeong-Hyeon AN)안정현, please check the attached file and reply",
        ] {
            assert_eq!(d(text), Some(Lang::En), "{text}");
        }
    }

    #[test]
    fn ambiguous_or_empty_text_fails_safely() {
        assert_eq!(d(""), None);
        assert_eq!(d("123 456 !!!"), None);
    }

    #[test]
    fn trait_is_object_safe() {
        let det: &dyn LanguageDetector = &RuleBasedDetector;
        assert_eq!(det.detect("hello"), Some(Lang::En));
    }
}
