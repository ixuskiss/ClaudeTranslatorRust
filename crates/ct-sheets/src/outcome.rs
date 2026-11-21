//! Outcomes of Sheets operations and the table result.

use ct_core::Lang;

use crate::model::SheetLocation;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetsStatus {
    Success,
    UnsupportedRange,
    NoText,
    TranslationFailed,
    TargetChanged,
    WriteFailed,
    WriteUnverified,
    CaptureFailed,
    WrongInputState,
    NoKoreanText,
    NoChanges,
    CorrectionFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnsupportedReason {
    UnidentifiedTarget,
    MultipleRanges,
    WholeRowOrColumn,
    MergedCells,
    HiddenOrFiltered,
    RichTextOrLink,
    UnknownCellType,
    RangeTooLarge,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutcomeReason {
    Unsupported(UnsupportedReason),
    CellTooLarge,
    Other,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetsOutcome {
    pub status: SheetsStatus,
    pub reason: Option<OutcomeReason>,
}

const UNSUPPORTED_PREFIX: &str = "이 선택 범위는 지원하지 않습니다";
const CELL_TOO_LARGE_MESSAGE: &str = "셀 내용이 너무 깁니다. 더 작은 텍스트를 선택해주세요.";

impl UnsupportedReason {
    fn detail(self) -> &'static str {
        match self {
            UnsupportedReason::UnidentifiedTarget => "선택 위치를 확인할 수 없습니다",
            UnsupportedReason::MultipleRanges => "떨어진 여러 범위는 선택할 수 없습니다",
            UnsupportedReason::WholeRowOrColumn => "행·열 전체 선택은 지원하지 않습니다",
            UnsupportedReason::MergedCells => "병합된 셀이 포함되어 있습니다",
            UnsupportedReason::HiddenOrFiltered => {
                "숨겨진 행·열 또는 필터로 가려진 행이 포함되어 있습니다"
            }
            UnsupportedReason::RichTextOrLink => {
                "글자별 서식이나 링크가 있는 셀이 포함되어 있습니다. 해당 셀을 제외하고 다시 선택해주세요"
            }
            UnsupportedReason::UnknownCellType => "셀 유형을 확인할 수 없는 셀이 포함되어 있습니다",
            UnsupportedReason::RangeTooLarge => {
                "선택 범위가 너무 큽니다. 더 작은 범위를 선택해주세요"
            }
        }
    }
}

impl SheetsOutcome {
    /// User-facing message (UI-ref 3.5). Empty for `Success`.
    pub fn user_message(&self) -> String {
        let too_large = self.reason == Some(OutcomeReason::CellTooLarge);
        match self.status {
            SheetsStatus::Success => String::new(),
            SheetsStatus::UnsupportedRange => match self.reason {
                Some(OutcomeReason::Unsupported(r)) => {
                    format!("{UNSUPPORTED_PREFIX}: {}", r.detail())
                }
                _ => format!("{UNSUPPORTED_PREFIX}."),
            },
            SheetsStatus::NoText => "번역할 텍스트가 없습니다.".into(),
            SheetsStatus::TranslationFailed if too_large => CELL_TOO_LARGE_MESSAGE.into(),
            SheetsStatus::TranslationFailed => "번역에 실패하여 원본을 변경하지 않았습니다.".into(),
            SheetsStatus::TargetChanged => {
                "번역 중 시트·선택 범위 또는 내용이 바뀌어 교체를 취소했습니다. 다시 실행해주세요."
                    .into()
            }
            SheetsStatus::WriteFailed => "붙여넣기에 실패했습니다. 시트를 확인해주세요.".into(),
            SheetsStatus::WriteUnverified => {
                "교체 결과를 확인할 수 없습니다. 시트 내용을 직접 확인해주세요.".into()
            }
            SheetsStatus::CaptureFailed => "선택한 셀을 읽을 수 없습니다.".into(),
            SheetsStatus::WrongInputState => {
                "Sheets에서는 셀을 선택하거나 셀을 편집하는 중에 실행해주세요.".into()
            }
            SheetsStatus::NoKoreanText => "선택한 셀에 한국어 텍스트가 없습니다.".into(),
            SheetsStatus::NoChanges => "수정할 부분이 없습니다.".into(),
            SheetsStatus::CorrectionFailed if too_large => CELL_TOO_LARGE_MESSAGE.into(),
            SheetsStatus::CorrectionFailed => "교정에 실패하여 원본을 변경하지 않았습니다.".into(),
        }
    }
}

/// One cell of the table view. `results` holds one entry per requested result column
/// (`None` language = a non-language column such as the corrected text).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableCell {
    pub row: usize,
    pub col: usize,
    pub original: String,
    pub results: Vec<(Option<Lang>, String)>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SheetsTable {
    pub location: SheetLocation,
    pub rows: usize,
    pub cols: usize,
    pub cells: Vec<TableCell>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_matches_ui_ref_table() {
        let generic = "이 선택 범위는 지원하지 않습니다.";
        let msg = |status, reason| SheetsOutcome { status, reason }.user_message();
        let plain = |status| msg(status, None);
        assert_eq!(plain(SheetsStatus::Success), "");
        assert_eq!(plain(SheetsStatus::NoText), "번역할 텍스트가 없습니다.");
        assert_eq!(
            plain(SheetsStatus::TranslationFailed),
            "번역에 실패하여 원본을 변경하지 않았습니다."
        );
        assert_eq!(
            plain(SheetsStatus::TargetChanged),
            "번역 중 시트·선택 범위 또는 내용이 바뀌어 교체를 취소했습니다. 다시 실행해주세요."
        );
        assert_eq!(
            plain(SheetsStatus::WriteFailed),
            "붙여넣기에 실패했습니다. 시트를 확인해주세요."
        );
        assert_eq!(
            plain(SheetsStatus::WriteUnverified),
            "교체 결과를 확인할 수 없습니다. 시트 내용을 직접 확인해주세요."
        );
        assert_eq!(
            plain(SheetsStatus::CaptureFailed),
            "선택한 셀을 읽을 수 없습니다."
        );
        assert_eq!(
            plain(SheetsStatus::WrongInputState),
            "Sheets에서는 셀을 선택하거나 셀을 편집하는 중에 실행해주세요."
        );
        assert_eq!(
            plain(SheetsStatus::NoKoreanText),
            "선택한 셀에 한국어 텍스트가 없습니다."
        );
        assert_eq!(plain(SheetsStatus::NoChanges), "수정할 부분이 없습니다.");
        assert_eq!(
            plain(SheetsStatus::CorrectionFailed),
            "교정에 실패하여 원본을 변경하지 않았습니다."
        );

        // UNSUPPORTED_RANGE: 8 reasons + no reason.
        assert_eq!(plain(SheetsStatus::UnsupportedRange), generic);
        let unsupported = |r| {
            msg(
                SheetsStatus::UnsupportedRange,
                Some(OutcomeReason::Unsupported(r)),
            )
        };
        let expected = [
            (
                UnsupportedReason::UnidentifiedTarget,
                "선택 위치를 확인할 수 없습니다",
            ),
            (
                UnsupportedReason::MultipleRanges,
                "떨어진 여러 범위는 선택할 수 없습니다",
            ),
            (
                UnsupportedReason::WholeRowOrColumn,
                "행·열 전체 선택은 지원하지 않습니다",
            ),
            (
                UnsupportedReason::MergedCells,
                "병합된 셀이 포함되어 있습니다",
            ),
            (
                UnsupportedReason::HiddenOrFiltered,
                "숨겨진 행·열 또는 필터로 가려진 행이 포함되어 있습니다",
            ),
            (
                UnsupportedReason::RichTextOrLink,
                "글자별 서식이나 링크가 있는 셀이 포함되어 있습니다. 해당 셀을 제외하고 다시 선택해주세요",
            ),
            (
                UnsupportedReason::UnknownCellType,
                "셀 유형을 확인할 수 없는 셀이 포함되어 있습니다",
            ),
            (
                UnsupportedReason::RangeTooLarge,
                "선택 범위가 너무 큽니다. 더 작은 범위를 선택해주세요",
            ),
        ];
        for (reason, detail) in expected {
            assert_eq!(
                unsupported(reason),
                format!("이 선택 범위는 지원하지 않습니다: {detail}")
            );
        }
        // Non-unsupported reasons are ignored for UNSUPPORTED_RANGE.
        assert_eq!(
            msg(SheetsStatus::UnsupportedRange, Some(OutcomeReason::Other)),
            generic
        );

        // CellTooLarge only matters for the two failure statuses.
        let too_large = "셀 내용이 너무 깁니다. 더 작은 텍스트를 선택해주세요.";
        for status in [
            SheetsStatus::TranslationFailed,
            SheetsStatus::CorrectionFailed,
        ] {
            assert_eq!(msg(status, Some(OutcomeReason::CellTooLarge)), too_large);
        }
        assert_eq!(
            msg(SheetsStatus::TranslationFailed, Some(OutcomeReason::Other)),
            "번역에 실패하여 원본을 변경하지 않았습니다."
        );
        assert_eq!(
            msg(SheetsStatus::NoText, Some(OutcomeReason::CellTooLarge)),
            "번역할 텍스트가 없습니다."
        );
    }
}
