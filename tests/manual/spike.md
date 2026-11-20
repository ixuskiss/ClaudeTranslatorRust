# 기반 스파이크 수동 검증 (S1~S5)

계획 2를 쓰기 전에 실제 Windows에서 직접 실행해 결과 칸을 채운다.

실행 명령 (저장소 루트):

```
cargo run -p ct-app --example spike
```

시작하면 작은 메인 창이 뜨고 트레이에 파란 사각형 아이콘이 생긴다. 표준출력(터미널)에 `S4: hook installed`, `S5: UIA ready`, 그리고 1~2초 안에 `S2: badge created and styled while hidden`이 찍혀야 한다. `S2: no HWND after retries: ...`가 찍히면 그 줄 전체를 기록한다. 종료는 트레이 메뉴의 "Exit".

## 실행 기록

| 날짜 | S1 | S2 | S3 | S4 | S5 | 메모 |
|---|---|---|---|---|---|---|
| 2026-11-20 (1차) | 통과 | 판정 불가 (`badge unavailable`) | 판정 불가 (`no HWND`, 창은 앞에 뜸) | 통과 | `name_len=6 control_type=50000` (Button) | HWND를 `show()` 직후에 요청한 스파이크 버그. Alt 단독 탭으로 Chrome 포커스가 툴바 버튼으로 이동(F15 처리 없음). 2차: HWND 재시도 + v5 F15 처리 추가 |

## S1. Slint 창 + 트레이가 한 이벤트 루프에서 동작

- 절차: 트레이 아이콘을 우클릭한다. 메뉴 "Show badge", "Show front window", "Exit"가 보이는지 확인한다. 메뉴를 연 채로 메인 창이 멈추지 않는지(창 이동 가능) 확인한다.
- 기대 결과: 메뉴가 뜨고 항목 클릭이 처리된다(S2, S3 동작으로 확인). "Exit"를 누르면 프로세스가 종료된다.
- 결과: [ ] 통과 [ ] 실패
- 메모:

## S2. 포커스를 뺏지 않는 배지

- 준비: 배지 창은 시작할 때 한 번만 만들어진다(화면 밖에 한 번 띄워 창 스타일을 적용한 뒤 숨김). 표준출력에 `S2: badge created and styled while hidden`이 찍혀야 한다.
- 절차: 메모장을 열고 글자를 계속 입력하면서 트레이 메뉴 "Show badge"를 누른다(누른 뒤 메모장 클릭해 입력 유지). 3초 뒤 커서 근처에 "Translating..." 창이 뜨고, 다시 3초 뒤 사라진다. 이 과정을 최소 3번 반복한다.
- 기대 결과: 배지가 떠도 입력이 메모장에 계속 들어간다. 배지는 테두리가 없고 항상 위에 보인다. 표준출력에 `S2: badge shown`이 찍힌다.
- 판정: 시작 시 첫 표시(화면 밖)가 포커스를 가져가더라도 그것은 판정에서 제외하고, 2번째 표시부터 포커스를 뺏지 않는지로 판정한다.
- 결과: [ ] 통과 [ ] 실패
- 메모:

## S3. 다른 앱 위로 창 가져오기

- 절차: "Show front window"를 누르고 즉시 다른 앱(메모장 등)을 클릭해 앞으로 가져온다. 3초 뒤 결과를 본다.
- 기대 결과: 다른 앱이 앞에 있어도 "Spike front window"가 맨 앞에 뜬다(작업 표시줄 깜빡임만으로는 실패). 표준출력에 어느 경로였는지 찍힌다(`window was already in front when the HWND appeared`, `SetForegroundWindow succeeded`, `AttachThreadInput fallback ... foreground=true/false` 중 하나).
- 결과: [ ] 통과 [ ] 실패
- 통한 방법:
- 메모:

## S4. 저수준 키보드 훅 Alt+Alt

- 절차: Chrome을 앞에 두고 Alt를 빠르게 두 번 눌렀다 뗀다. 또 Alt를 누른 채 유지해 본다.
- 기대 결과: 빠르게 두 번 누르면 `ALT_ALT`가 한 번만 찍힌다. 누른 채 유지(키 반복)로는 찍히지 않는다. 다른 키 입력은 정상 동작한다(훅이 입력을 막지 않음).
- F15 처리(v5 `windows_keyboard_hook.py` 방식): Alt를 혼자 눌렀다 뗄 때마다 `S4: solo Alt tap -> F15 down ok=true up ok=true`가 찍혀야 한다. `ok=false`면 그 줄을 기록한다. Alt+Tab처럼 다른 키와 함께 쓴 Alt에서는 찍히지 않아야 한다.
- 추가 확인: Chrome에서 Alt를 한 번 눌렀다 떼도 Chrome 오른쪽 위 메뉴 버튼(⋮)에 포커스 테두리가 생기지 않아야 한다. 입력 중이던 칸에서 바로 계속 타이핑이 되어야 한다.
- 참고: 두 번의 Alt 떼기(release) 사이가 200 ms 이내여야 `ALT_ALT`로 인식된다. Alt를 누른 채 유지하는 동안에는 아무것도 찍히면 안 된다.
- 결과: [ ] 통과 [ ] 실패
- 메모:

## S5. UI Automation 포커스 요소

- 절차: Chrome에서 Gmail 새 메일 작성 본문에 포커스를 두고 Alt+Alt를 누른다.
- 기대 결과: `S5: focused name_len=<n> control_type=<id>`가 찍힌다. 이름 내용은 출력되지 않는다(개인정보 보호). Edit/Document 계열 control type id가 나오는지 기록한다.
- 참고: Edit = 50004, Document = 50030, Button = 50000(Alt 때문에 포커스가 툴바로 옮겨 간 경우). Chrome은 첫 UIA 조회 뒤에야 접근성 트리를 켤 수 있으므로 Alt+Alt를 두 번 시도해 두 번째 결과도 기록한다.
- 결과: [ ] 통과 [ ] 실패
- 찍힌 control_type:
- 메모:
