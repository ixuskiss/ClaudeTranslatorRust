//! Opt-in integration tests against the real Claude Code CLI.
//!
//! Skipped unless `RUN_CLAUDE_CLI_LIVE_TESTS=1`. Requires `claude` installed and signed in.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use ct_core::{
    Action, AppConfig, ClaudeCliConfig, CorrectionMode, CorrectionStatus, DictionaryEntry, Lang,
    LlmProviderKind, RuleBasedDetector, SheetsConfig, Tone, correct_text, explain_text,
    translate_text,
};
use ct_llm::prompts::POLISH_BANNED_WORDS;
use ct_llm::{BuiltProvider, ProviderOverrides, RealSpawner, build_llm_provider, fallback_models};

fn live_enabled() -> bool {
    if std::env::var("RUN_CLAUDE_CLI_LIVE_TESTS").as_deref() == Ok("1") {
        true
    } else {
        eprintln!("skipped");
        false
    }
}

fn build(cwd: &std::path::Path) -> BuiltProvider {
    let cfg = AppConfig {
        llm_provider: LlmProviderKind::ClaudeCli,
        claude_cli: ClaudeCliConfig {
            timeout: Duration::from_secs(120),
            ..ClaudeCliConfig::default()
        },
        sheets: SheetsConfig::default(),
    };
    let overrides = ProviderOverrides {
        cli_path: String::new(),
        model: "sonnet".to_string(),
        effort: "low".to_string(),
    };
    build_llm_provider(
        &cfg,
        cwd,
        &overrides,
        &fallback_models(),
        Arc::new(RealSpawner),
    )
}

fn has_hangul(s: &str) -> bool {
    s.chars().any(|c| ('가'..='힣').contains(&c))
}

fn tr(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn live_translate_multi_keeps_tokens() {
    if !live_enabled() {
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let built = build(dir.path());
    let entries = vec![
        DictionaryEntry::new("DA".into(), Action::Keep, BTreeMap::new()).unwrap(),
        DictionaryEntry::new(
            "논리 모델".into(),
            Action::Translate,
            tr(&[("en", "logical model"), ("ja", "論理モデル")]),
        )
        .unwrap(),
    ];
    let result = translate_text(
        "DA에서 논리 모델 확인해주세요.",
        &entries,
        &RuleBasedDetector,
        built.provider.as_ref(),
        Lang::Ko,
        &[Lang::En, Lang::Ja],
        Tone::Neutral,
    )
    .unwrap();
    assert_eq!(result.translations.len(), 2);
    for (_, t) in &result.translations {
        assert!(t.contains("DA"), "missing DA in {t}");
    }
    let en = &result.translations[0].1;
    let ja = &result.translations[1].1;
    assert!(en.contains("logical model"), "en: {en}");
    assert!(ja.contains("論理モデル"), "ja: {ja}");
}

#[test]
fn live_target_input_goes_to_base() {
    if !live_enabled() {
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let built = build(dir.path());
    let result = translate_text(
        "Please check the report.",
        &[],
        &RuleBasedDetector,
        built.provider.as_ref(),
        Lang::Ko,
        &[Lang::En],
        Tone::Neutral,
    )
    .unwrap();
    let langs: Vec<Lang> = result.translations.iter().map(|(l, _)| *l).collect();
    assert_eq!(langs, vec![Lang::Ko]);
    assert!(has_hangul(&result.translations[0].1));
}

#[test]
fn live_explain_multi() {
    if !live_enabled() {
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let built = build(dir.path());
    let result = explain_text(
        "안녕하세요. 오늘 날씨가 좋네요.",
        &[],
        &RuleBasedDetector,
        built.provider.as_ref(),
        Lang::Ko,
        &[Lang::En, Lang::Ja],
        Tone::Neutral,
    )
    .unwrap();
    assert!(!result.sentences.is_empty());
    for s in &result.sentences {
        for lang in [Lang::En, Lang::Ja] {
            let found = s
                .translations
                .iter()
                .find(|(l, _)| *l == lang)
                .map(|(_, t)| t.trim());
            assert!(
                found.is_some_and(|t| !t.is_empty()),
                "missing {lang:?} for {}",
                s.original
            );
        }
    }
}

#[test]
fn live_correct_spelling() {
    if !live_enabled() {
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let built = build(dir.path());
    let outcome = correct_text(
        "오늘 회의 는 3시 에 시작 합니다.",
        Lang::Ko,
        CorrectionMode::Spelling,
        &[],
        &RuleBasedDetector,
        built.provider.as_ref(),
    );
    assert_eq!(outcome.status, CorrectionStatus::Success);
    let text = outcome.corrected_text.unwrap_or_default();
    assert!(text.contains("회의는"), "corrected: {text}");
}

#[test]
fn live_correct_polish_rewrites_keeps_haeyo_no_banned_words() {
    if !live_enabled() {
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let built = build(dir.path());
    let source = "제가 생각했을 때에는 그 부분은 조금 더 확인을 해보는 것이 필요할 것 같아요.";
    let outcome = correct_text(
        source,
        Lang::Ko,
        CorrectionMode::Polish,
        &[],
        &RuleBasedDetector,
        built.provider.as_ref(),
    );
    assert_eq!(outcome.status, CorrectionStatus::Success);
    let text = outcome.corrected_text.unwrap_or_default();
    assert!(text.trim_end().ends_with("요."), "corrected: {text}");
    assert_ne!(text.trim(), source);
    for w in POLISH_BANNED_WORDS {
        assert!(
            source.contains(w) || !text.contains(w),
            "banned word {w} in {text}"
        );
    }
}

#[test]
fn live_list_models() {
    if !live_enabled() {
        return;
    }
    let dir = tempfile::TempDir::new().unwrap();
    let built = build(dir.path());
    let client = built.client.as_ref().unwrap();
    let models = client.list_models(Duration::from_secs(30)).unwrap();
    assert!(!models.is_empty());
    assert!(models.iter().all(|m| !m.value.is_empty()));
}
