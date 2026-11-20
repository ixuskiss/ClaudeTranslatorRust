# 기반 스파이크 수동 검증 (S1~S5)

계획 2를 쓰기 전에 실제 Windows에서 직접 실행해 결과 칸을 채운다.

실행 명령 (저장소 루트):

```
cargo run -p ct-app --example spike
```

시작하면 작은 메인 창이 뜨고 트레이에 파란 사각형 아이콘이 생긴다. 표준출력(터미널)에 `S4: hook installed`, `S5: UIA ready`가 찍혀야 한다. 종료는 트레이 메뉴의 "Exit".

## S1. Slint 창 + 트레이가 한 이벤트 루프에서 동작

- 절차: 트레이 아이콘을 우클릭한다. 메뉴 "Show badge", "Show front window", "Exit"가 보이는지 확인한다. 메뉴를 연 채로 메인 창이 멈추지 않는지(창 이동 가능) 확인한다.
- 기대 결과: 메뉴가 뜨고 항목 클릭이 처리된다(S2, S3 동작으로 확인). "Exit"를 누르면 프로세스가 종료된다.
- 결과: [ ] 통과 [ ] 실패
- 메모:

## S2. 포커스를 뺏지 않는 배지

- 절차: 메모장을 열고 글자를 계속 입력하면서 트레이 메뉴 "Show badge"를 누른다(누른 뒤 메모장 클릭해 입력 유지). 3초 뒤 커서 근처에 "Translating..." 창이 뜬다.
- 기대 결과: 배지가 떠도 입력이 메모장에 계속 들어간다. 배지는 테두리가 없고 항상 위에 보인다. 표준출력에 `S2: badge shown`이 찍힌다.
- 결과: [ ] 통과 [ ] 실패
- 메모:

## S3. 다른 앱 위로 창 가져오기

- 절차: "Show front window"를 누르고 즉시 다른 앱(메모장 등)을 클릭해 앞으로 가져온다. 3초 뒤 결과를 본다.
- 기대 결과: 다른 앱이 앞에 있어도 "Spike front window"가 맨 앞에 뜬다(작업 표시줄 깜빡임만으로는 실패). 표준출력에 어느 방법이 통했는지 찍힌다(`SetForegroundWindow succeeded` 또는 `AttachThreadInput fallback ... foreground=true`).
- 결과: [ ] 통과 [ ] 실패
- 통한 방법:
- 메모:

## S4. 저수준 키보드 훅 Alt+Alt

- 절차: Chrome을 앞에 두고 Alt를 빠르게 두 번 눌렀다 뗀다. 또 Alt를 누른 채 유지해 본다.
- 기대 결과: 빠르게 두 번 누르면 `ALT_ALT`가 한 번만 찍힌다. 누른 채 유지(키 반복)로는 찍히지 않는다. 다른 키 입력은 정상 동작한다(훅이 입력을 막지 않음).
- 결과: [ ] 통과 [ ] 실패
- 메모:

## S5. UI Automation 포커스 요소

- 절차: Chrome에서 Gmail 새 메일 작성 본문에 포커스를 두고 Alt+Alt를 누른다.
- 기대 결과: `S5: focused name_len=<n> control_type=<id>`가 찍힌다. 이름 내용은 출력되지 않는다(개인정보 보호). Edit/Document 계열 control type id가 나오는지 기록한다.
- 결과: [ ] 통과 [ ] 실패
- 찍힌 control_type:
- 메모:
