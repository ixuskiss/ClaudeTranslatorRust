# 계획 1 후속 항목 (계획 2 이후로 넘김)

계획 1(기반)의 작업별 리뷰와 최종 리뷰에서 나왔지만 이번에 고치지 않은 항목이다. 계획 2를 쓸 때 반영한다.

## 계획 2 초반에 처리할 것

- **CLI 프로세스 트리 종료 (R12, R14)**: `claude.cmd`를 종료하면 자식 node 프로세스가 남을 수 있다. 그 프로세스가 stdin을 잡고 있으면 막힌 writer 스레드도 함께 남는다. Windows Job Object로 프로세스 트리 전체를 종료한다 (`crates/ct-llm/src/spawner.rs`의 `RealChild::terminate`).
- **CLI stderr 수집**: 지금은 stderr를 버리므로, 잘못된 `--model`/`--effort`나 CLI 시작 오류가 "process ended unexpectedly"로만 보인다. stderr 끝부분을 제한된 길이로 모으고, 로그에는 오류 종류와 종료 코드만 남긴다.
- **모델 값 "default" 매핑**: v5 Settings는 "default"를 빈 값으로 바꿔 `--model`을 넘기지 않았다. 앱에서 Settings를 연결할 때 같은 규칙을 적용한다.
- **간격 값 검증**: `alt_alt_interval_seconds`/`ctrl_ctrl_interval_seconds`에 NaN·음수·inf 같은 값이 들어올 수 있다(v5와 같음). 불러올 때나 훅 라우터에서 값의 범위를 제한한다.
- **스파이크 결과**: `tests/manual/spike.md`의 S1~S5를 사용자가 직접 실행해 결과를 채운 뒤 계획 2를 확정한다. S2 배지는 Win32로 보이기·숨기기를 하므로, 두 번째 표시부터 내용이 제대로 그려지는지도 확인한다.
- **사용자 메시지 매핑**: `TranslateError::Llm` 같은 영어 오류를 v5의 한국어 사용자 문구로 바꾸는 일은 앱 디스패치에서 한다.

## 언제 해도 되는 것

- `ClaudeCliClient::list_models`: initialize 응답이 사라지면 그 프로세스에서는 계속 Pending 상태로 남는다. 잘못된 결과는 나오지 않고, 호출할 때마다 10초씩 기다리는 비용만 생긴다. 두 번째 타임아웃에서 프로세스를 정리하는 방안을 검토한다.
- `ClaudeCliClient`: poison된 락을 복구하면 기존 프로세스를 재사용한다. 지금은 락 안에서 panic이 날 경로가 없어 이론상의 문제다.
- 사전 `DictionaryEntry::new`: 번역 키 소문자 정규화가 로드할 때만 이루어진다. `update`로 다른 항목 이름을 기존 표기로 바꾸는 것을 막는 검사도 없다. 개인 사전 창을 만들 때 함께 처리한다.
- 사전 저장: `.bak` 복사가 실패해도 다음 저장이 원본을 덮어쓴다. 실패를 기억했다가 저장을 막거나 사용자에게 알린다.
- `toggle_target`: 빈 목록을 입력하면 `[대체 언어, lang]`이 나온다(UI에서는 생기지 않는 경우).
- `compose_replacement`: 요청하지 않은 빈 블록 제거가 들어 있다. Sheets 계획에서 없애거나 테스트를 붙인다.
- 토큰 검사: `⟦` 다음의 숫자는 ASCII만 인정한다(전각 숫자는 일반 텍스트로 처리되지만 안전한 쪽으로 실패함). 여러 조각에 걸쳐 나뉜 토큰은 v5와 같이 처리한다.
- 설명·교정·프로바이더·팩토리 테스트에 빈틈이 있다: 사전 용어만 있는 입력, 유니코드 공백(U+3000·NBSP), JSON 앞에 설명 문장이 붙은 교정 응답, `refresh_catalog_and_warm_up`, 경로 덮어쓰기 우선순위, `which_on_path`.
- dotenv 테스트가 호스트 환경 변수에 영향을 받는다. 환경 변수 테스트가 하나 더 생기면 격리한다.
- `RuleBasedDetector`에 `Debug`/`Clone`/`Copy`/`Default` derive를 추가한다.
- 스파이크 S4: LL 훅 콜백 안에서 `println`을 쓰고, key-up을 놓치면 `st.down`이 그대로 남을 수 있다(스파이크 전용).
- `ct-app`: 스파이크 전용 dev-dependency(slint, tray-icon) 때문에 cargo가 "unused dependency" 경고를 낸다. 계획 2에서 정식 의존성이 되면 사라진다.

## 운영 메모

- 실제 CLI 테스트는 PowerShell에서 실행한다: `$env:RUN_CLAUDE_CLI_LIVE_TESTS="1"; cargo test -p ct-llm --test live_cli -- --test-threads=1`. Git Bash에서는 claude.cmd 실행이 os error 5로 실패할 수 있다. 2026-11-20 PowerShell 실행에서 6개 모두 통과했다(1회 관찰, 76.67초).
- 다듬기 프롬프트에서 사전 토큰 보존 문장을 "do not change or remove them"으로 바꿨다(교정 프롬프트에 "translate"가 들어가지 않게, R13). v5와 문구가 조금 다르다.
