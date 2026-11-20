//! 데이터 폴더의 YAML 파일을 읽다가 생긴 문제를 사용자에게 보여줄 문구로 만든다.
//! 메시지에는 파일 이름과 개수만 담고 사전·설정 내용은 담지 않는다.

/// 문제 목록을 시작 경고 문구로 만든다. 문제가 없으면 `None`.
pub fn file_problems_warning(problems: &[String]) -> Option<String> {
    if problems.is_empty() {
        return None;
    }
    let lines = problems
        .iter()
        .map(|p| format!("- {p}"))
        .collect::<Vec<_>>()
        .join("\n");
    Some(format!(
        "설정·사전 파일 일부를 읽지 못해 해당 내용 없이 시작했습니다.\n\n{lines}\n\n프로그램을 종료(트레이 → Exit)한 뒤 파일을 고치고 다시 시작하세요. 형식은 README의 '사전·설정 파일 샘플과 설정 방법'을 참고하세요."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_problems_means_no_warning() {
        assert_eq!(file_problems_warning(&[]), None);
    }

    #[test]
    fn warning_text_exact() {
        assert_eq!(
            file_problems_warning(&["a".into()]).as_deref(),
            Some(
                "설정·사전 파일 일부를 읽지 못해 해당 내용 없이 시작했습니다.\n\n- a\n\n프로그램을 종료(트레이 → Exit)한 뒤 파일을 고치고 다시 시작하세요. 형식은 README의 '사전·설정 파일 샘플과 설정 방법'을 참고하세요."
            )
        );
    }

    #[test]
    fn warning_lists_each_problem_and_how_to_fix() {
        let m = file_problems_warning(&[
            "user.yaml: 파일 형식 오류로 읽지 못했습니다.".into(),
            "user.yaml: 항목 2개를 읽지 못해 건너뛰었습니다.".into(),
        ])
        .unwrap();
        assert!(
            m.contains("- user.yaml: 파일 형식 오류로 읽지 못했습니다.\n- user.yaml: 항목 2개")
        );
        assert!(m.contains("다시 시작"));
    }
}
