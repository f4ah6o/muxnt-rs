MVP Spec

## 0. Goal / Non-goals
### Goal
- macOS + Ubuntuで動く、マウス中心の軽量ターミナルマルチプレクサ
- tmux代替として「分割・フォーカス・リサイズ・スクロール」がストレスなくできる

### Non-goals (MVPではやらない)
- detach/attach（セッション永続化）
- ネットワーク越しの共有セッション
- 高度なスクリプト/プラグイン/設定言語
- コピーモードの高機能化（矩形選択など）
- 端末エミュレータをゼロから自作（既存部品を利用）

## 1. Target Platforms
- macOS (Apple Silicon / Intel)
- Ubuntu (x86_64)

## 2. UI / Interaction (Mouse-first)
### Global
- 画面は1つの「ワークスペース」
- ワークスペース内に複数「ペイン（Pane）」を配置
- ペインは水平/垂直に分割可能
- 操作の大半はマウスで完結

### Mouse interactions
- マウスイベントはmuxntが常にハンドル（ペイン内アプリへのパススルーなし）
- Click:
  - クリックしたペインをアクティブ化（入力先）
- Wheel:
  - アクティブペインのスクロールバックを上下
- Drag on splitter (境界線):
  - ペイン境界をドラッグしてリサイズ
- Right click (context menu):
  - ペイン上で右クリック → コンテキストメニュー表示（Split/Close/New）
- Drag tab (MVPでは不要):
  - omit（タブ自体MVPでは不要）

### Minimal keyboard (MVP)
- 入力は基本すべてアクティブペインへ素通し
- 例外（最小限）:
  - Ctrl+C / Ctrl+Z 等は通常通りシェルへ送る
  - EscはUIに奪わない（端末へ送る）
- “UI操作専用キー”はMVPでは0〜2個に抑える（任意）
  - 例: Ctrl+Shift+P（任意）= コマンドメニュー（MVPでは省略可）

## 3. Core Features (MVP)
### 3.1 Pane management
- New pane:
  - 現在のペインを split して新規ペインを作る
  - 新規ペインはデフォルトでログインシェルを起動
- Split:
  - Split Right（Vertical split）
  - Split Down（Horizontal split）
- Close pane:
  - ペインを閉じる
  - ペイン内でプロセスが生きている場合は確認ダイアログ（Yes/No）
- Focus:
  - クリックでフォーカス移動
  - アクティブ枠を視覚的に強調（Cyan色の枠線）
- Resize:
  - splitterドラッグで比率変更（ヒット判定: 3セル幅）
  - 最小サイズ（min cols/rows）を下回らないよう制限

### 3.2 Terminal I/O (PTY)
- 各ペインは1つのPTYを持つ
- ペインごとに子プロセス（shell）をspawn
- 入力:
  - アクティブペインに対してキー入力をPTYへwrite
- 出力:
  - PTY read → ターミナルエミュレータに流す → 画面バッファ更新

### 3.3 Scrollback
- ペインごとにスクロールバックを保持（上限: 10,000行）
- Wheelでスクロールバック位置を変更
- 新しい出力が来たら原則「最下部追従」だが、
  - ユーザーがスクロールバック中は追従しない（sticky）
  - クリックで最下部へ戻す（例: status barのボタン/メニュー項目）

### 3.4 Status bar (MVP)
- 画面下部に固定
- 表示:
  - active pane id
  - layout summary（例: 1x2, 2 panes）
  - optional: cwd/title（取れたら）
- クリック操作（任意）:
  - “Back to bottom” ボタン

## 4. Layout Model
### Data model
- レイアウトは「二分木」
  - Leaf(pane_id)
  - Split { dir: Horizontal|Vertical, ratio: f32, a: Node, b: Node }

### Layout rules
- ratio: 0.05〜0.95 でクランプ
- min size:
  - pane_min_cols >= 20
  - pane_min_rows >= 5
- resizing:
  - splitterヒットテストで対象Splitを特定し、ratio更新

## 5. Rendering
- フレームごとに:
  1) 端末出力イベントを処理して各ペインのscreen buffer更新
  2) レイアウト計算で各ペイン矩形を決定
  3) 各ペインのbufferを矩形へ描画
  4) splitter線、アクティブ枠、status bar描画

## 6. Context Menu (MVP)
### On pane
- Split Right
- Split Down
- Close Pane
- New Pane (same as Split Right by default)

### On splitter
- (optional) Equalize（MVPでは省略可）

## 7. Process lifecycle
- pane spawn:
  - default command = user login shell
  - TERM環境変数: xterm-256color
- close:
  - 子プロセスが終了したらペインを自動的に「dead」状態にし、表示上 "(exited)" を出す
  - ユーザーがCloseで手動削除（自動クローズしない）
  - 最後のペインを閉じたらアプリ終了

## 8. Configuration (MVP)
- 設定ファイルなし
- コマンドライン引数は最小:
  - --shell <path>（任意）
  - --cwd <path>（任意）

## 9. Logging / Debug (MVP)
- --log <path> でファイルログ（任意）
- panic時に画面を壊さず復帰はMVPでは不要（落ちても良い）

## 10. Crates & Architecture (Implementation guidance)
### Crates
- UI + input: crossterm (mouse events) + ratatui
- PTY: portable-pty
- Terminal emulation: termwiz
- Async runtime: tokio (chosen)

### PTY I/O Model (tokio async)
- 各ペインのPTY読み取りは `tokio::task::spawn_blocking` で実行
  - `portable-pty` は同期APIのため blocking task が必要
- PTY出力は `tokio::sync::mpsc` でメインタスクへ送信
- crossterm の `EventStream` で入力イベントを async 処理
- メインループは `tokio::select!` で PTY出力・入力イベント・タイマーを統合

## 11. Acceptance Criteria (MVP)
- macOS/Ubuntuで起動して、1ペインでシェルが動く
- 右クリックメニューからSplit Right / Split Downができ、複数ペインで同時に入力できる
- クリックでアクティブペインが切り替わる
- splitterをドラッグしてリサイズできる
- マウスホイールでスクロールバックが動く
- ペインを閉じられる（プロセス生存時は確認）
- 設定不要で使える

## 12. Plan (MVP)
### Phase 0: Project setup
- Confirm crates and pin versions (crossterm, ratatui, portable-pty, termwiz, tokio)
- Define module layout (app, layout, pane, pty, input, render)
- Wire CLI flags and logging (--shell, --cwd, --log)

### Phase 1: Layout + rendering skeleton
- Implement layout tree with ratio clamp + min size rules
- Render panes with borders and active highlight
- Add status bar placeholder with layout summary

### Phase 2: PTY + terminal emulation
- Spawn shell per pane with `tokio::task::spawn_blocking`
- Connect PTY read via `tokio::sync::mpsc` channel
- Feed PTY output into termwiz buffer
- Forward keyboard input to active pane via `crossterm::event::EventStream`

### Phase 3: Mouse interactions
- Hit-test panes/splitters and update focus on click
- Drag splitter to resize (update ratio)
- Wheel scrollback with sticky behavior

### Phase 4: Pane lifecycle + menu
- Right-click context menu with Split/Close/New
- Confirm on close if process is alive
- Handle process exit (mark dead and allow remove)

### Phase 5: Acceptance pass
- Manual verification on macOS and Ubuntu
- Validate MVP acceptance criteria

## 13. Out of Scope (Future)
- detach/attach, session restore
- tabs / workspaces
- copy mode / search
- per-pane environment, pane rename
- git integration (VS Code like) — Option B領域
- multiplex over SSH / remote control API
