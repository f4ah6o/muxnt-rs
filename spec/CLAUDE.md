Overall Assessment

  This is a well-structured MVP spec
  for a mouse-first terminal
  multiplexer. The scope is
  appropriately constrained, and the
  non-goals are clearly stated
  upfront.

  Strengths

  1. Clear scope boundaries - The
  non-goals section prevents feature
  creep (no detach/attach, no
  scripting, no network sessions)
  2. Binary tree layout model -
  Simple and sufficient for the use
  case. The ratio clamping
  (0.05–0.95) and min size
  constraints (20 cols, 5 rows) are
  sensible defaults
  3. Crate selection - The suggested
  stack (crossterm + ratatui +
  portable-pty + termwiz + tokio) is
  pragmatic and well-supported
  4. Phased plan - The 6-phase
  approach (Phase 0-5) builds
  incrementally from skeleton to
  acceptance

  Concerns & Suggestions

  Technical

  1. Thread model - DECIDED: tokio async
  - 各ペインのPTY読み取りは spawn_blocking で実行
  - portable-pty は同期APIのため blocking task が必要
  - PTY出力は tokio::sync::mpsc でメインタスクへ送信
  - crossterm の EventStream で入力を async 処理
  - メインループは tokio::select! で統合
  2. Scrollback buffer size - DECIDED:
  10,000 lines per pane
  3. Resize handling - TODO: PTY
  SIGWINCH propagation on window
  resize (各ペインのPTYに新サイズを通知)
  4. Mouse capture mode - DECIDED:
  muxntが常にハンドル（パススルーなし）

  UX

  5. Dead pane UX - DECIDED: 手動クローズ
  "(exited)"表示のまま残し、ユーザーがClose
  6. Splitter hit area - DECIDED:
  3セル幅のヒット判定

  Resolved Details

  7. TERM環境変数 - DECIDED: xterm-256color
  8. Focus indication - DECIDED: Cyan色の枠線
  9. Last pane closes - DECIDED: アプリ終了

  Minor Issues (Acknowledged)

  - Section 3.1: "Split Right" =
  右に新ペイン作成（垂直分割線）
  tmux命名規則とは逆だが、直感的な名前を採用

  Plan (MVP)

  1. Phase 0: Project setup - Confirm
  crates and pin versions (crossterm,
  ratatui, portable-pty, termwiz,
  tokio), define module layout, wire
  CLI/logging flags.
  2. Phase 1: Layout + rendering
  skeleton - Implement layout tree
  with ratio clamp and min size
  rules, render panes/borders, add
  status bar stub.
  3. Phase 2: PTY + terminal
  emulation - Spawn shell per pane
  with tokio spawn_blocking, connect
  PTY I/O via mpsc channels, feed
  termwiz buffer, forward keyboard
  input via EventStream.
  4. Phase 3: Mouse interactions -
  Hit-test panes/splitters for focus,
  drag to resize, wheel scrollback
  with sticky behavior.
  5. Phase 4: Pane lifecycle + menu -
  Context menu with Split/Close/New,
  confirm on close if process alive,
  handle process exit state.
  6. Phase 5: Acceptance pass -
  Manual verification on macOS and
  Ubuntu, validate MVP criteria.
