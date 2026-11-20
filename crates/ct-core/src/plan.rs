//! Translation plan resolution: decides the source language and destination languages.

use crate::lang::Lang;

/// Resolved translation plan for one piece of input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranslationPlan {
    pub source: Lang,
    pub destinations: Vec<Lang>,
}

/// Resolves the plan for input detected as `detected`.
///
/// - Base-language input goes to every target, in order.
/// - A target-language input goes back to the base language only.
/// - Any other language (or no detection) yields `None`.
pub fn resolve_plan(
    detected: Option<Lang>,
    base: Lang,
    targets: &[Lang],
) -> Option<TranslationPlan> {
    let detected = detected?;
    if detected == base {
        return Some(TranslationPlan {
            source: base,
            destinations: targets.to_vec(),
        });
    }
    if targets.contains(&detected) {
        return Some(TranslationPlan {
            source: detected,
            destinations: vec![base],
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base_input_goes_to_all_targets_in_order() {
        let p = resolve_plan(Some(Lang::Ko), Lang::Ko, &[Lang::Ja, Lang::En]).unwrap();
        assert_eq!(
            (p.source, p.destinations),
            (Lang::Ko, vec![Lang::Ja, Lang::En])
        );
    }

    #[test]
    fn target_input_goes_to_base_only() {
        let p = resolve_plan(Some(Lang::En), Lang::Ko, &[Lang::En, Lang::Ja]).unwrap();
        assert_eq!((p.source, p.destinations), (Lang::En, vec![Lang::Ko]));
    }

    #[test]
    fn other_or_unknown_language_is_none() {
        assert!(resolve_plan(Some(Lang::Vi), Lang::Ko, &[Lang::En]).is_none());
        assert!(resolve_plan(None, Lang::Ko, &[Lang::En]).is_none());
    }

    #[test]
    fn single_target_matches_v5_bidirectional_table() {
        // Ported from v5 tests/unit/language/test_direction.py (base ko).
        // (target, detected, expected_source, expected_destination)
        let cases = [
            (Lang::En, Lang::Ko, Lang::Ko, Lang::En),
            (Lang::En, Lang::En, Lang::En, Lang::Ko),
            (Lang::Vi, Lang::Ko, Lang::Ko, Lang::Vi),
            (Lang::Vi, Lang::Vi, Lang::Vi, Lang::Ko),
            (Lang::Ja, Lang::Ko, Lang::Ko, Lang::Ja),
            (Lang::Ja, Lang::Ja, Lang::Ja, Lang::Ko),
        ];
        for (target, detected, source, destination) in cases {
            let p = resolve_plan(Some(detected), Lang::Ko, &[target]).unwrap();
            assert_eq!(
                (p.source, p.destinations),
                (source, vec![destination]),
                "target={target:?} detected={detected:?}"
            );
        }
    }
}
