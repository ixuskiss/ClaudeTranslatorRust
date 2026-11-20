//! Prompt builders for translation, explanation and Korean correction.
//!
//! Wording is ported verbatim from v5 (`llm/claude_cli.py`) except where the polish mode is
//! deliberately strengthened (full rewrite allowed, banned hype words).

use ct_core::{
    CorrectionMode, CorrectionRequest, ExplanationRequest, Lang, Tone, TranslationRequest,
};

/// Words the polish prompt forbids introducing unless the original already uses them.
pub const POLISH_BANNED_WORDS: &[&str] = &[
    "혁신적인",
    "획기적인",
    "압도적인",
    "놀라운",
    "탁월한",
    "완벽한",
    "극대화",
    "최적화된",
    "시너지",
    "패러다임",
    "여정",
    "도약",
    "게임 체인저",
    "핵심적인 역할",
    "필수적인",
    "원활한",
    "한층 더",
    "매우 중요한",
    "진정한",
    "깊이 있는",
];

const STRICT_FORMATTING_INSTRUCTION: &str = concat!(
    "Preserve the original text's line breaks and non-alphabetic formatting ",
    "(e.g. \"->\", bullet points, indentation) exactly -- translate only the ",
    "words; never reformat, summarize, merge lines, or drop characters. "
);
const SOFT_FORMATTING_INSTRUCTION: &str = "Keep the same line breaks. ";
/// Ruling R13: correction prompts must not use the translation-oriented strict line.
const CORRECTION_FORMATTING_INSTRUCTION: &str = concat!(
    "Keep the paragraph and line-break structure and non-alphabetic formatting ",
    "(e.g. \"->\", bullet points, indentation) exactly; never summarize or drop characters. "
);

const SPELLING_INSTRUCTION: &str = concat!(
    "Fix only typos, spelling mistakes and Korean spacing (띄어쓰기) errors. ",
    "Do not change word choice, word order, tone, politeness level or meaning. "
);
const POLISH_MERGE_SPLIT_INSTRUCTION: &str = "You may merge or split sentences. ";
const POLISH_ONE_TO_ONE_INSTRUCTION: &str = concat!(
    "Return exactly one corrected sentence for each original sentence; ",
    "within that sentence you may rewrite it completely. "
);

fn tone_instruction(tone: Tone) -> &'static str {
    match tone {
        Tone::Neutral => "",
        Tone::Casual => "Use a casual, conversational tone. ",
        Tone::Formal => "Use a formal, polite tone suitable for official company emails. ",
        Tone::Plain => "Use simple, plain vocabulary and short sentences where possible. ",
    }
}

fn formatting_instruction(model: &str) -> &'static str {
    if model == "haiku" {
        SOFT_FORMATTING_INSTRUCTION
    } else {
        STRICT_FORMATTING_INSTRUCTION
    }
}

fn correction_formatting_instruction(model: &str) -> &'static str {
    if model == "haiku" {
        SOFT_FORMATTING_INSTRUCTION
    } else {
        CORRECTION_FORMATTING_INSTRUCTION
    }
}

fn code_list(langs: &[Lang]) -> String {
    langs
        .iter()
        .map(|l| l.code())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `"ja": str, "en": str` in destination order.
fn key_shape(langs: &[Lang]) -> String {
    langs
        .iter()
        .map(|l| format!("\"{}\": str", l.code()))
        .collect::<Vec<_>>()
        .join(", ")
}

fn polish_instruction() -> String {
    format!(
        concat!(
            "Fix typos, spelling mistakes and Korean spacing (띄어쓰기) errors, and rewrite the text ",
            "into natural, easy-to-read Korean. As long as the meaning and facts stay the same, ",
            "you may rewrite a sentence completely: change word order, structure and word choice; ",
            "the length may change. Even when little seems wrong, use a more natural expression ",
            "if there is one. Keep the meaning and facts exactly (do not add or drop information), ",
            "keep the politeness level and sentence endings (합니다체, 해요체 or 반말), and keep ",
            "the paragraph and line-break structure. Do not add emphasis, exaggerated modifiers ",
            "or emotional words that are not in the original; use plain, matter-of-fact wording ",
            "suitable for work email and chat. In particular, do not introduce these words unless ",
            "the original already uses them: {}. "
        ),
        POLISH_BANNED_WORDS.join(", ")
    )
}

fn is_polish(mode: CorrectionMode) -> bool {
    mode == CorrectionMode::Polish
}

fn correction_preamble(mode: CorrectionMode) -> String {
    let instruction = if is_polish(mode) {
        polish_instruction()
    } else {
        SPELLING_INSTRUCTION.to_string()
    };
    format!(
        "Proofread the following Korean text. {instruction}Leave non-Korean words exactly as \
         written. Preserve any tokens that look like ⟦N⟧ exactly as-is -- do not change or \
         remove them. "
    )
}

pub fn translation_prompt(req: &TranslationRequest, model: &str) -> String {
    format!(
        "Translate the following text from {src} into each of these languages: {dests}. \
         Translate into each language independently, directly from the original text. \
         Preserve any tokens that look like ⟦N⟧ exactly as-is -- do not translate or remove \
         them. {formatting}{tone}Respond with only JSON matching this shape: \
         {{\"translations\": {{{shape}}}}} \n\nText:\n{text}",
        src = req.source.code(),
        dests = code_list(&req.destinations),
        formatting = formatting_instruction(model),
        tone = tone_instruction(req.tone),
        shape = key_shape(&req.destinations),
        text = req.text,
    )
}

pub fn explanation_prompt(req: &ExplanationRequest) -> String {
    format!(
        "Split the following {src} text into sentences and, for each sentence, provide a \
         translation into each of these languages: {dests}. Preserve any tokens that look like \
         ⟦N⟧ exactly as-is in both original and translations -- do not translate or remove \
         them. {tone}Respond with only JSON matching this shape: \
         {{\"sentences\": [{{\"original\": str, \"translations\": {{{shape}}}}}]}} \
         \n\nText:\n{text}",
        src = req.source.code(),
        dests = code_list(&req.destinations),
        tone = tone_instruction(req.tone),
        shape = key_shape(&req.destinations),
        text = req.text,
    )
}

pub fn correction_prompt(req: &CorrectionRequest, model: &str) -> String {
    let extra = if is_polish(req.mode) {
        POLISH_MERGE_SPLIT_INSTRUCTION
    } else {
        ""
    };
    format!(
        "{}{}{}Respond with only JSON matching this shape: {{\"corrected\": str}} \n\nText:\n{}",
        correction_preamble(req.mode),
        extra,
        correction_formatting_instruction(model),
        req.text
    )
}

pub fn sentence_correction_prompt(req: &CorrectionRequest) -> String {
    let extra = if is_polish(req.mode) {
        POLISH_ONE_TO_ONE_INSTRUCTION
    } else {
        ""
    };
    format!(
        "{}{}Split the text into sentences and, for each sentence, provide the original \
         sentence and its corrected version. Respond with only JSON matching this shape: \
         {{\"sentences\": [{{\"original\": str, \"corrected\": str}}]}} \n\nText:\n{}",
        correction_preamble(req.mode),
        extra,
        req.text
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(text: &str, source: Lang, dests: &[Lang]) -> TranslationRequest {
        TranslationRequest {
            text: text.into(),
            source,
            destinations: dests.to_vec(),
            tone: Tone::Neutral,
        }
    }

    fn corr(mode: CorrectionMode) -> CorrectionRequest {
        CorrectionRequest {
            text: "x".into(),
            mode,
        }
    }

    #[test]
    fn translation_prompt_lists_languages_in_order_and_shape() {
        let p = translation_prompt(&req("x", Lang::Ko, &[Lang::Ja, Lang::En]), "sonnet");
        assert!(p.contains("from ko into each of these languages: ja, en."));
        assert!(p.contains(r#"{"translations": {"ja": str, "en": str}}"#));
        assert!(p.ends_with("\n\nText:\nx"));
    }

    #[test]
    fn translation_prompt_keeps_tokens_and_formatting() {
        let p = translation_prompt(&req("⟦0⟧ done", Lang::En, &[Lang::Ko]), "sonnet");
        assert!(p.contains("⟦N⟧"));
        assert!(p.to_lowercase().contains("line breaks"));
        assert!(p.contains("->"));
        assert!(p.to_lowercase().contains("never reformat"));
    }

    #[test]
    fn haiku_gets_soft_formatting_and_unset_model_strict() {
        let r = req("x", Lang::En, &[Lang::Ko]);
        let haiku = translation_prompt(&r, "haiku").to_lowercase();
        assert!(haiku.contains("keep the same line breaks"));
        assert!(!haiku.contains("never reformat"));
        assert!(
            translation_prompt(&r, "")
                .to_lowercase()
                .contains("never reformat")
        );
    }

    #[test]
    fn neutral_tone_adds_nothing() {
        let p = translation_prompt(&req("x", Lang::En, &[Lang::Ko]), "sonnet");
        assert!(!p.to_lowercase().contains("tone"));
    }

    #[test]
    fn tone_instructions_are_included() {
        for (tone, snippet) in [
            (Tone::Casual, "casual"),
            (Tone::Formal, "formal, polite tone"),
            (Tone::Plain, "simple, plain vocabulary"),
        ] {
            let mut r = req("x", Lang::En, &[Lang::Ko]);
            r.tone = tone;
            assert!(
                translation_prompt(&r, "sonnet")
                    .to_lowercase()
                    .contains(snippet)
            );
            assert!(explanation_prompt(&r).to_lowercase().contains(snippet));
        }
    }

    #[test]
    fn explanation_prompt_shape() {
        let p = explanation_prompt(&req("⟦0⟧ 확인", Lang::Ko, &[Lang::En]));
        assert!(p.contains(r#"{"sentences": [{"original": str, "translations": {"en": str}}]}"#));
        assert!(p.contains("⟦N⟧"));
        assert!(p.contains("Split the following ko text"));
        assert!(p.ends_with("\n\nText:\n⟦0⟧ 확인"));
    }

    #[test]
    fn spelling_text_is_v5_and_keeps_tokens() {
        let p = correction_prompt(&corr(CorrectionMode::Spelling), "sonnet");
        assert!(p.contains("⟦N⟧"));
        assert!(p.contains("Fix only typos, spelling mistakes"));
        assert!(p.contains(r#"{"corrected": str}"#));
        assert!(p.ends_with("\n\nText:\nx"));
        assert!(!p.contains("awkward"));
    }

    #[test]
    fn polish_prompt_allows_full_rewrite_and_bans_words() {
        let p = correction_prompt(&corr(CorrectionMode::Polish), "sonnet");
        assert!(p.contains("you may rewrite a sentence completely"));
        assert!(p.contains("keep the politeness level"));
        for w in POLISH_BANNED_WORDS {
            assert!(p.contains(w));
        }
        assert!(p.contains("You may merge or split sentences."));
        assert!(p.contains("⟦N⟧"));
    }

    #[test]
    fn sentence_polish_prompt_is_one_to_one() {
        let p = sentence_correction_prompt(&corr(CorrectionMode::Polish));
        assert!(p.contains("Return exactly one corrected sentence for each original sentence"));
        assert!(!p.contains("merge or split"));
        assert!(p.contains("⟦N⟧"));
        assert!(p.contains(r#"{"sentences": [{"original": str, "corrected": str}]}"#));
    }

    const CORRECTION_FORMATTING: &str = "Keep the paragraph and line-break structure and \
        non-alphabetic formatting (e.g. \"->\", bullet points, indentation) exactly; never \
        summarize or drop characters. ";

    #[test]
    fn correction_prompts_use_correction_formatting_line() {
        for mode in [CorrectionMode::Spelling, CorrectionMode::Polish] {
            for model in ["sonnet", "", "opus"] {
                let p = correction_prompt(&corr(mode), model);
                assert!(p.contains(CORRECTION_FORMATTING), "{mode:?} {model}");
                assert!(
                    !p.contains("Keep the same line breaks."),
                    "{mode:?} {model}"
                );
            }
            let haiku = correction_prompt(&corr(mode), "haiku");
            assert!(haiku.contains("Keep the same line breaks. "), "{mode:?}");
            assert!(!haiku.contains(CORRECTION_FORMATTING), "{mode:?}");
        }
    }

    #[test]
    fn correction_prompts_never_mention_translation() {
        for mode in [CorrectionMode::Spelling, CorrectionMode::Polish] {
            for model in ["sonnet", "haiku", ""] {
                let p = correction_prompt(&corr(mode), model).to_lowercase();
                assert!(!p.contains("translate"), "{mode:?} {model}: {p}");
            }
            let p = sentence_correction_prompt(&corr(mode)).to_lowercase();
            assert!(!p.contains("translate"), "{mode:?}: {p}");
        }
    }

    #[test]
    fn translation_prompt_keeps_strict_formatting_line() {
        let p = translation_prompt(&req("x", Lang::En, &[Lang::Ko]), "sonnet");
        assert!(p.contains(STRICT_FORMATTING_INSTRUCTION));
        assert!(!p.contains(CORRECTION_FORMATTING));
    }

    #[test]
    fn sentence_polish_prompt_bans_every_word() {
        let p = sentence_correction_prompt(&corr(CorrectionMode::Polish));
        for w in POLISH_BANNED_WORDS {
            assert!(p.contains(w), "{w}");
        }
    }

    #[test]
    fn spelling_prompt_has_no_rewrite_or_banned_list() {
        let single = correction_prompt(&corr(CorrectionMode::Spelling), "sonnet");
        let sentences = sentence_correction_prompt(&corr(CorrectionMode::Spelling));
        for p in [single, sentences] {
            assert!(!p.contains("rewrite"));
            assert!(!p.contains(POLISH_BANNED_WORDS[0]));
            assert!(!p.contains("merge or split"));
        }
    }
}
