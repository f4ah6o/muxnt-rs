mod input;
mod layout;
mod pane;
mod types;

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, MouseButton, MouseEvent,
    MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::Terminal;
use shadow_terminal::active_terminal::ActiveTerminal;
use shadow_terminal::output::native::{Output, ScreenMode};
use shadow_terminal::pty::BytesFromSTDIN;
use shadow_terminal::shadow_terminal::Config;
use shadow_terminal::Protocol;

use crate::input::key_event_to_bytes;
use crate::layout::{
    build_snapshot, pane_rect, remove_pane, replace_leaf_with_split, split_ratio_for_position,
    LayoutSnapshot, Node, SplitDirection, Splitter, MIN_COLS, MIN_ROWS,
};
use crate::pane::Pane;
use crate::types::PaneId;

const STATUS_BAR_HEIGHT: u16 = 1;
const SCROLLBACK_SIZE: usize = 10_000;
const SCROLL_STEP: usize = 5;

#[derive(Parser, Debug)]
#[command(name = "muxnt-rs", version, about = "Mouse-first terminal multiplexer")]
struct Cli {
    #[arg(long)]
    shell: Option<PathBuf>,
    #[arg(long)]
    cwd: Option<PathBuf>,
    #[arg(long)]
    log: Option<PathBuf>,
}

#[derive(Debug)]
enum PaneEvent {
    Output { id: PaneId, output: Output },
    Exited { id: PaneId },
}

#[derive(Clone, Copy, Debug)]
enum MenuAction {
    NewPane,
    SplitRight,
    SplitDown,
    Close,
}

const MENU_ITEMS: [(&str, MenuAction); 4] = [
    ("New Pane", MenuAction::NewPane),
    ("Split Right", MenuAction::SplitRight),
    ("Split Down", MenuAction::SplitDown),
    ("Close Pane", MenuAction::Close),
];

#[derive(Debug, Clone)]
struct MenuState {
    pane_id: PaneId,
    position: (u16, u16),
    selected: Option<usize>,
}

#[derive(Debug, Clone)]
struct ConfirmClose {
    pane_id: PaneId,
}

struct App {
    panes: BTreeMap<PaneId, Pane>,
    layout: Node,
    active_pane: PaneId,
    next_pane_id: PaneId,
    viewport: Rect,
    layout_snapshot: LayoutSnapshot,
    menu: Option<MenuState>,
    confirm: Option<ConfirmClose>,
    dragging: Option<Splitter>,
    pane_events_tx: tokio::sync::mpsc::Sender<PaneEvent>,
    shell_command: Vec<OsString>,
    needs_draw: bool,
    should_quit: bool,
    status_bottom: Option<(u16, u16)>,
}

impl App {
    fn new(
        viewport: Rect,
        shell_command: Vec<OsString>,
        pane_events_tx: tokio::sync::mpsc::Sender<PaneEvent>,
    ) -> Self {
        Self {
            panes: BTreeMap::new(),
            layout: Node::Leaf(0),
            active_pane: 0,
            next_pane_id: 1,
            viewport,
            layout_snapshot: LayoutSnapshot::default(),
            menu: None,
            confirm: None,
            dragging: None,
            pane_events_tx,
            shell_command,
            needs_draw: true,
            should_quit: false,
            status_bottom: None,
        }
    }

    fn init_first_pane(&mut self) {
        let content = self.content_area();
        let width = content.width.max(1);
        let height = content.height.max(1);
        let id = self.spawn_pane(width, height);
        self.layout = Node::Leaf(id);
        self.active_pane = id;
        self.sync_layout();
    }

    fn pane_config(&self, width: u16, height: u16) -> Config {
        Config {
            width: width.max(1),
            height: height.max(1),
            command: self.shell_command.clone(),
            scrollback_size: SCROLLBACK_SIZE,
            scrollback_step: SCROLL_STEP,
        }
    }

    fn spawn_pane(&mut self, width: u16, height: u16) -> PaneId {
        let id = self.next_pane_id;
        self.next_pane_id = self.next_pane_id.saturating_add(1);

        let config = self.pane_config(width, height);
        let mut terminal = ActiveTerminal::start(config);
        let (dummy_tx, dummy_rx) = tokio::sync::mpsc::channel(1);
        drop(dummy_tx);
        let mut output_rx = std::mem::replace(&mut terminal.surface_output_rx, dummy_rx);
        let output_tx = self.pane_events_tx.clone();
        tokio::spawn(async move {
            while let Some(output) = output_rx.recv().await {
                if output_tx
                    .send(PaneEvent::Output { id, output })
                    .await
                    .is_err()
                {
                    break;
                }
            }
        });

        let mut control_rx = terminal.control_tx.subscribe();
        let output_tx = self.pane_events_tx.clone();
        tokio::spawn(async move {
            while let Ok(message) = control_rx.recv().await {
                if matches!(message, Protocol::End) {
                    let _ = output_tx.send(PaneEvent::Exited { id }).await;
                    break;
                }
            }
        });

        let pane = Pane::new(id, terminal, width, height);
        self.panes.insert(id, pane);
        id
    }

    fn content_area(&self) -> Rect {
        if self.viewport.height <= STATUS_BAR_HEIGHT {
            return Rect {
                x: 0,
                y: 0,
                width: self.viewport.width,
                height: 0,
            };
        }

        Rect {
            x: 0,
            y: 0,
            width: self.viewport.width,
            height: self.viewport.height - STATUS_BAR_HEIGHT,
        }
    }

    fn status_area(&self) -> Rect {
        if self.viewport.height == 0 {
            return Rect {
                x: 0,
                y: 0,
                width: 0,
                height: 0,
            };
        }
        Rect {
            x: 0,
            y: self.viewport.height - STATUS_BAR_HEIGHT,
            width: self.viewport.width,
            height: STATUS_BAR_HEIGHT,
        }
    }

    fn sync_layout(&mut self) {
        let content = self.content_area();
        let snapshot = build_snapshot(&mut self.layout, content);
        let rects = snapshot.panes.clone();

        for pane in self.panes.values_mut() {
            if let Some(rect) = rects.iter().find(|p| p.id == pane.id).map(|p| p.rect) {
                pane.resize(rect.width, rect.height);
            }
        }

        if !self.panes.contains_key(&self.active_pane) {
            if let Some(first) = rects.first() {
                self.active_pane = first.id;
            }
        }

        self.layout_snapshot = snapshot;
    }

    fn handle_pane_event(&mut self, event: PaneEvent) {
        match event {
            PaneEvent::Output { id, output } => {
                if let Some(pane) = self.panes.get_mut(&id) {
                    pane.apply_output(output);
                }
            }
            PaneEvent::Exited { id } => {
                if let Some(pane) = self.panes.get_mut(&id) {
                    pane.exited = true;
                }
            }
        }
        self.needs_draw = true;
    }

    fn active_input_sender(&self) -> Option<tokio::sync::mpsc::Sender<BytesFromSTDIN>> {
        self.panes
            .get(&self.active_pane)
            .filter(|pane| !pane.exited)
            .map(|pane| pane.terminal.pty_input_tx.clone())
    }

    fn focus_pane(&mut self, id: PaneId) {
        if self.panes.contains_key(&id) {
            self.active_pane = id;
            self.needs_draw = true;
        }
    }

    fn pane_at(&self, x: u16, y: u16) -> Option<PaneId> {
        self.layout_snapshot
            .panes
            .iter()
            .find(|pane| contains(pane.rect, x, y))
            .map(|pane| pane.id)
    }

    fn splitter_at(&self, x: u16, y: u16) -> Option<Splitter> {
        self.layout_snapshot
            .splitters
            .iter()
            .find(|splitter| contains(splitter.hit_rect, x, y))
            .cloned()
    }

    fn handle_resize(&mut self, width: u16, height: u16) {
        self.viewport = Rect {
            x: 0,
            y: 0,
            width,
            height,
        };
        self.sync_layout();
        self.needs_draw = true;
    }

    fn split_pane(&mut self, target: PaneId, dir: SplitDirection) {
        let Some(rect) = pane_rect(&self.layout_snapshot, target) else {
            return;
        };

        let can_split = match dir {
            SplitDirection::Vertical => rect.width >= MIN_COLS.saturating_mul(2),
            SplitDirection::Horizontal => rect.height >= MIN_ROWS.saturating_mul(2),
        };
        if !can_split {
            return;
        }

        let new_id = self.spawn_pane(rect.width, rect.height);
        if !replace_leaf_with_split(&mut self.layout, target, dir, new_id) {
            self.panes.remove(&new_id);
            return;
        }

        self.sync_layout();
        self.needs_draw = true;
    }

    fn request_close(&mut self, id: PaneId) {
        let Some(pane) = self.panes.get(&id) else {
            return;
        };
        if pane.exited {
            self.close_pane(id);
        } else {
            self.confirm = Some(ConfirmClose { pane_id: id });
            self.menu = None;
            self.needs_draw = true;
        }
    }

    fn close_pane(&mut self, id: PaneId) {
        let pane = self.panes.remove(&id);
        if let Some(pane) = pane {
            let _ = pane.terminal.kill();
        }

        self.menu = None;
        self.confirm = None;

        let next_layout = remove_pane(std::mem::replace(&mut self.layout, Node::Leaf(0)), id);
        if let Some(layout) = next_layout {
            self.layout = layout;
        } else {
            self.should_quit = true;
            return;
        }

        if self.panes.is_empty() {
            self.should_quit = true;
            return;
        }

        if !self.panes.contains_key(&self.active_pane) {
            if let Some(next) = self.panes.keys().next().copied() {
                self.active_pane = next;
            }
        }

        self.sync_layout();
        self.needs_draw = true;
    }

    fn open_menu(&mut self, pane_id: PaneId, position: (u16, u16)) {
        self.menu = Some(MenuState {
            pane_id,
            position,
            selected: None,
        });
        self.confirm = None;
        self.needs_draw = true;
    }

    fn menu_rect(&self, menu: &MenuState) -> Rect {
        let content = self.content_area();
        let max_len = MENU_ITEMS
            .iter()
            .map(|(label, _)| label.len())
            .max()
            .unwrap_or(0);
        let width = (max_len + 4).min(content.width as usize) as u16;
        let height = (MENU_ITEMS.len() + 2).min(content.height as usize) as u16;
        let mut x = menu.position.0.min(content.x + content.width.saturating_sub(1));
        let mut y = menu.position.1.min(content.y + content.height.saturating_sub(1));
        if x + width > content.x + content.width {
            x = content.x + content.width.saturating_sub(width);
        }
        if y + height > content.y + content.height {
            y = content.y + content.height.saturating_sub(height);
        }
        Rect { x, y, width, height }
    }

    fn menu_item_at(&self, menu: &MenuState, x: u16, y: u16) -> Option<usize> {
        let rect = self.menu_rect(menu);
        if rect.width < 2 || rect.height < 2 {
            return None;
        }
        let item_x = rect.x + 1;
        let item_y = rect.y + 1;
        if x < item_x || x >= rect.x + rect.width - 1 {
            return None;
        }
        if y < item_y || y >= item_y + MENU_ITEMS.len() as u16 {
            return None;
        }
        Some((y - item_y) as usize)
    }

    fn handle_menu_action(&mut self, action: MenuAction, pane_id: PaneId) {
        match action {
            MenuAction::NewPane | MenuAction::SplitRight => {
                self.split_pane(pane_id, SplitDirection::Vertical);
            }
            MenuAction::SplitDown => {
                self.split_pane(pane_id, SplitDirection::Horizontal);
            }
            MenuAction::Close => {
                self.request_close(pane_id);
            }
        }
    }

    fn handle_mouse(&mut self, mouse: MouseEvent) {
        let x = mouse.column;
        let y = mouse.row;

        if let Some(confirm) = self.confirm.clone() {
            if matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left)) {
                if let Some(result) = confirm_hit_test(self.viewport, x, y) {
                    if result {
                        self.confirm = None;
                        self.close_pane(confirm.pane_id);
                    } else {
                        self.confirm = None;
                        self.needs_draw = true;
                    }
                }
            }
            return;
        }

        if let Some(menu) = self.menu.clone() {
            match mouse.kind {
                MouseEventKind::Moved => {
                    let selected = self.menu_item_at(&menu, x, y);
                    self.menu = Some(MenuState { selected, ..menu });
                    self.needs_draw = true;
                }
                MouseEventKind::Down(MouseButton::Left) => {
                    if let Some(index) = self.menu_item_at(&menu, x, y) {
                        let (_, action) = MENU_ITEMS[index];
                        self.menu = None;
                        self.handle_menu_action(action, menu.pane_id);
                    } else {
                        self.menu = None;
                        self.needs_draw = true;
                    }
                }
                MouseEventKind::Down(MouseButton::Right) => {
                    self.menu = None;
                    self.needs_draw = true;
                }
                _ => {}
            }
            return;
        }

        match mouse.kind {
            MouseEventKind::Down(MouseButton::Right) => {
                if let Some(pane_id) = self.pane_at(x, y) {
                    self.open_menu(pane_id, (x, y));
                }
            }
            MouseEventKind::Down(MouseButton::Left) => {
                if let Some((start, end)) = self.status_bottom {
                    if y == self.status_area().y && x >= start && x < end {
                        if let Some(pane) = self.panes.get(&self.active_pane) {
                            if pane.scroll_position > 0 {
                                let _ = pane.terminal.scroll_cancel();
                            }
                        }
                        return;
                    }
                }

                if let Some(splitter) = self.splitter_at(x, y) {
                    self.dragging = Some(splitter);
                    return;
                }

                if let Some(pane_id) = self.pane_at(x, y) {
                    self.focus_pane(pane_id);
                    if let Some(pane) = self.panes.get(&pane_id) {
                        if pane.scroll_position > 0 {
                            let _ = pane.terminal.scroll_cancel();
                        }
                    }
                }
            }
            MouseEventKind::Drag(MouseButton::Left) => {
                if let Some(splitter) = self.dragging.clone() {
                    let ratio = split_ratio_for_position(&splitter, x, y);
                    let _ = crate::layout::update_ratio(&mut self.layout, &splitter.path, ratio);
                    self.sync_layout();
                    self.needs_draw = true;
                }
            }
            MouseEventKind::Up(MouseButton::Left) => {
                self.dragging = None;
            }
            MouseEventKind::ScrollUp => {
                if let Some(pane) = self.panes.get(&self.active_pane) {
                    let _ = pane.terminal.scroll_up();
                }
            }
            MouseEventKind::ScrollDown => {
                if let Some(pane) = self.panes.get(&self.active_pane) {
                    let _ = pane.terminal.scroll_down();
                }
            }
            _ => {}
        }
    }

    fn draw(&mut self, frame: &mut ratatui::Frame) {
        let content = self.content_area();
        let status = self.status_area();
        let buf = frame.buffer_mut();

        for pane_layout in &self.layout_snapshot.panes {
            fill_rect(buf, pane_layout.rect, Style::default());
            if let Some(pane) = self.panes.get(&pane_layout.id) {
                let (surface, offset) = pane_surface_view(pane, pane_layout.rect);
                render_surface(buf, pane_layout.rect, surface, offset);
                if pane.exited {
                    let label = "(exited)";
                    buf.set_stringn(
                        pane_layout.rect.x.saturating_add(1),
                        pane_layout.rect.y,
                        label,
                        pane_layout.rect.width.saturating_sub(2) as usize,
                        Style::default().fg(Color::Yellow),
                    );
                }
            }
        }

        draw_splitters(buf, &self.layout_snapshot.splitters);

        if let Some(active_rect) = pane_rect(&self.layout_snapshot, self.active_pane) {
            draw_border(buf, active_rect, Style::default().fg(Color::Cyan));
        }

        if status.height > 0 {
            self.draw_status_bar(buf, status);
        }

        if let Some(menu) = self.menu.clone() {
            let rect = self.menu_rect(&menu);
            draw_menu(buf, rect, menu.selected);
        }

        if self.confirm.is_some() {
            if let Some(rect) = confirm_rect(self.viewport) {
                draw_confirm(buf, rect);
            }
        }

        if content.height == 0 || content.width == 0 {
            buf.set_stringn(
                0,
                0,
                "Resize the window to continue.",
                self.viewport.width as usize,
                Style::default().fg(Color::Yellow),
            );
        }
    }

    fn draw_status_bar(&mut self, buf: &mut Buffer, area: Rect) {
        let style = Style::default().bg(Color::DarkGray).fg(Color::White);
        fill_rect(buf, area, style);
        let width = area.width as usize;
        if width == 0 {
            self.status_bottom = None;
            return;
        }

        let label = "[Bottom]";
        let label_len = label.len();
        let label_start = width.saturating_sub(label_len);
        let summary = layout_summary(&self.layout);
        let left_text = format!("Pane {} | {}", self.active_pane, summary);
        let left_max = label_start.saturating_sub(1);

        buf.set_stringn(area.x, area.y, left_text, left_max, style);
        if label_len <= width {
            let x = area.x + label_start as u16;
            buf.set_stringn(x, area.y, label, label_len, style);
            self.status_bottom = Some((x, x + label_len as u16));
        } else {
            self.status_bottom = None;
        }
    }
}

struct TuiGuard;

impl TuiGuard {
    fn enter() -> Result<Self> {
        enable_raw_mode().context("enable raw mode")?;
        execute!(io::stdout(), EnterAlternateScreen, EnableMouseCapture, Hide)?;
        Ok(Self)
    }
}

impl Drop for TuiGuard {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), Show, DisableMouseCapture, LeaveAlternateScreen);
        let _ = disable_raw_mode();
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Some(log_path) = cli.log {
        let file = File::create(log_path).context("open log file")?;
        tracing_subscriber::fmt().with_writer(file).with_ansi(false).init();
    }

    if let Some(cwd) = cli.cwd {
        std::env::set_current_dir(cwd).context("set cwd")?;
    }

    unsafe {
        std::env::set_var("TERM", "xterm-256color");
    }

    let shell = cli
        .shell
        .map(|path| path.into_os_string())
        .or_else(|| std::env::var_os("SHELL"))
        .unwrap_or_else(|| OsString::from("/bin/sh"));
    let shell_command = vec![shell];

    let _guard = TuiGuard::enter()?;
    let backend = CrosstermBackend::new(io::stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let (pane_events_tx, mut pane_events_rx) = tokio::sync::mpsc::channel(256);
    let size = terminal.size()?;
    let mut app = App::new(size.into(), shell_command, pane_events_tx);
    app.init_first_pane();

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_millis(33));

    loop {
        tokio::select! {
            event = events.next() => {
                match event {
                    Some(Ok(Event::Key(key))) => {
                        if let Some(bytes) = key_event_to_bytes(key) {
                            if let Some(sender) = app.active_input_sender() {
                                send_bytes(sender, &bytes).await;
                            }
                        }
                    }
                    Some(Ok(Event::Mouse(mouse))) => {
                        app.handle_mouse(mouse);
                    }
                    Some(Ok(Event::Resize(width, height))) => {
                        app.handle_resize(width, height);
                    }
                    Some(Err(_)) | None => {
                        app.should_quit = true;
                    }
                    _ => {}
                }
            }
            Some(pane_event) = pane_events_rx.recv() => {
                app.handle_pane_event(pane_event);
            }
            _ = tick.tick() => {
                app.needs_draw = true;
            }
        }

        if app.needs_draw {
            terminal.draw(|frame| app.draw(frame))?;
            app.needs_draw = false;
        }

        if app.should_quit {
            break;
        }
    }

    Ok(())
}

async fn send_bytes(sender: tokio::sync::mpsc::Sender<BytesFromSTDIN>, bytes: &[u8]) {
    for chunk in bytes.chunks(128) {
        let mut payload: BytesFromSTDIN = [0; 128];
        payload[..chunk.len()].copy_from_slice(chunk);
        if sender.send(payload).await.is_err() {
            break;
        }
    }
}

fn layout_summary(layout: &Node) -> String {
    let count = layout.leaf_count();
    let (all_vertical, all_horizontal) = layout.split_orientation();
    let grid = if all_vertical {
        format!("1x{}", count)
    } else if all_horizontal {
        format!("{}x1", count)
    } else {
        "mixed".to_string()
    };
    let label = if count == 1 { "pane" } else { "panes" };
    format!("{}, {} {}", grid, count, label)
}

fn pane_surface_view(pane: &Pane, rect: Rect) -> (&shadow_terminal::termwiz::surface::Surface, usize) {
    if pane.screen_mode == ScreenMode::Alternate {
        return (&pane.screen_surface, 0);
    }

    let visible = rect.height as usize;
    let total = pane.scrollback_surface.dimensions().1;
    let max_start = total.saturating_sub(visible);
    let start = max_start.saturating_sub(pane.scroll_position);
    (&pane.scrollback_surface, start)
}

fn render_surface(
    buf: &mut Buffer,
    area: Rect,
    surface: &shadow_terminal::termwiz::surface::Surface,
    line_offset: usize,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let lines = surface.screen_lines();
    let width = area.width as usize;
    let height = area.height as usize;

    for row in 0..height {
        let Some(line) = lines.get(line_offset + row) else {
            break;
        };
        for cell in line.visible_cells() {
            let col = cell.cell_index();
            if col >= width {
                continue;
            }
            let attrs = cell.attrs();
            let style = style_from_attrs(attrs);
            let symbol = if attrs.invisible() { " " } else { cell.str() };
            buf[(area.x + col as u16, area.y + row as u16)]
                .set_symbol(symbol)
                .set_style(style);
        }
    }
}

fn style_from_attrs(attrs: &shadow_terminal::termwiz::cell::CellAttributes) -> Style {
    use shadow_terminal::termwiz::cell::{Blink, Intensity, Underline};

    let mut fg = color_from_attr(attrs.foreground());
    let mut bg = color_from_attr(attrs.background());
    if attrs.reverse() {
        std::mem::swap(&mut fg, &mut bg);
    }

    let mut style = Style::default();
    if let Some(color) = fg {
        style = style.fg(color);
    }
    if let Some(color) = bg {
        style = style.bg(color);
    }

    let mut modifiers = Modifier::empty();
    match attrs.intensity() {
        Intensity::Bold => modifiers.insert(Modifier::BOLD),
        Intensity::Half => modifiers.insert(Modifier::DIM),
        _ => {}
    }
    if attrs.italic() {
        modifiers.insert(Modifier::ITALIC);
    }
    if attrs.underline() != Underline::None {
        modifiers.insert(Modifier::UNDERLINED);
    }
    if attrs.blink() != Blink::None {
        modifiers.insert(Modifier::SLOW_BLINK);
    }
    if attrs.strikethrough() {
        modifiers.insert(Modifier::CROSSED_OUT);
    }
    if attrs.reverse() {
        modifiers.insert(Modifier::REVERSED);
    }

    style.add_modifier(modifiers)
}

fn color_from_attr(
    attr: shadow_terminal::termwiz::color::ColorAttribute,
) -> Option<Color> {
    use shadow_terminal::termwiz::color::ColorAttribute;

    match attr {
        ColorAttribute::Default => None,
        ColorAttribute::PaletteIndex(idx) => Some(Color::Indexed(idx)),
        ColorAttribute::TrueColorWithPaletteFallback(srgba, _) => Some(srgba_color(srgba)),
        ColorAttribute::TrueColorWithDefaultFallback(srgba) => Some(srgba_color(srgba)),
    }
}

fn srgba_color(srgba: shadow_terminal::termwiz::color::SrgbaTuple) -> Color {
    let to_u8 = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    Color::Rgb(to_u8(srgba.0), to_u8(srgba.1), to_u8(srgba.2))
}

fn draw_splitters(buf: &mut Buffer, splitters: &[Splitter]) {
    let style = Style::default().fg(Color::DarkGray);
    for splitter in splitters {
        match splitter.dir {
            SplitDirection::Vertical => {
                let x = splitter.pos;
                if x >= splitter.area.x.saturating_add(splitter.area.width) {
                    continue;
                }
                for y in splitter.area.y..splitter.area.y.saturating_add(splitter.area.height) {
                    buf[(x, y)].set_symbol("|").set_style(style);
                }
            }
            SplitDirection::Horizontal => {
                let y = splitter.pos;
                if y >= splitter.area.y.saturating_add(splitter.area.height) {
                    continue;
                }
                for x in splitter.area.x..splitter.area.x.saturating_add(splitter.area.width) {
                    buf[(x, y)].set_symbol("-").set_style(style);
                }
            }
        }
    }
}

fn draw_border(buf: &mut Buffer, rect: Rect, style: Style) {
    if rect.width < 2 || rect.height < 2 {
        return;
    }
    let x0 = rect.x;
    let y0 = rect.y;
    let x1 = rect.x + rect.width - 1;
    let y1 = rect.y + rect.height - 1;

    buf[(x0, y0)].set_symbol("+").set_style(style);
    buf[(x1, y0)].set_symbol("+").set_style(style);
    buf[(x0, y1)].set_symbol("+").set_style(style);
    buf[(x1, y1)].set_symbol("+").set_style(style);

    for x in (x0 + 1)..x1 {
        buf[(x, y0)].set_symbol("-").set_style(style);
        buf[(x, y1)].set_symbol("-").set_style(style);
    }
    for y in (y0 + 1)..y1 {
        buf[(x0, y)].set_symbol("|").set_style(style);
        buf[(x1, y)].set_symbol("|").set_style(style);
    }
}

fn draw_menu(buf: &mut Buffer, rect: Rect, selected: Option<usize>) {
    if rect.width < 2 || rect.height < 2 {
        return;
    }
    let style = Style::default().bg(Color::Black).fg(Color::White);
    fill_rect(buf, rect, style);
    draw_border(buf, rect, Style::default().fg(Color::Gray));
    let content_x = rect.x + 1;
    let content_y = rect.y + 1;
    let content_width = rect.width.saturating_sub(2) as usize;

    for (idx, (label, _)) in MENU_ITEMS.iter().enumerate() {
        if (idx as u16) >= rect.height.saturating_sub(2) {
            break;
        }
        let mut item_style = style;
        if Some(idx) == selected {
            item_style = Style::default().bg(Color::DarkGray).fg(Color::White);
        }
        buf.set_stringn(
            content_x,
            content_y + idx as u16,
            format!(" {}", label),
            content_width,
            item_style,
        );
    }
}

fn confirm_rect(viewport: Rect) -> Option<Rect> {
    let width = 38.min(viewport.width);
    let height = 5.min(viewport.height);
    if width < 10 || height < 3 {
        return None;
    }
    let x = viewport.x + viewport.width.saturating_sub(width) / 2;
    let y = viewport.y + viewport.height.saturating_sub(height) / 2;
    Some(Rect { x, y, width, height })
}

fn confirm_hit_test(viewport: Rect, x: u16, y: u16) -> Option<bool> {
    let rect = confirm_rect(viewport)?;
    let button_y = rect.y + rect.height.saturating_sub(2);
    if y != button_y {
        return None;
    }
    let yes = "[Yes]";
    let no = "[No]";
    let total = yes.len() + 1 + no.len();
    let start = rect.x + (rect.width.saturating_sub(total as u16) / 2);
    let yes_start = start;
    let yes_end = yes_start + yes.len() as u16;
    let no_start = yes_end + 1;
    let no_end = no_start + no.len() as u16;

    if x >= yes_start && x < yes_end {
        Some(true)
    } else if x >= no_start && x < no_end {
        Some(false)
    } else {
        None
    }
}

fn draw_confirm(buf: &mut Buffer, rect: Rect) {
    let style = Style::default().bg(Color::Black).fg(Color::White);
    fill_rect(buf, rect, style);
    draw_border(buf, rect, Style::default().fg(Color::Gray));
    let message = "Process still running. Close pane?";
    buf.set_stringn(
        rect.x + 1,
        rect.y + 1,
        message,
        rect.width.saturating_sub(2) as usize,
        style,
    );
    let button_y = rect.y + rect.height.saturating_sub(2);
    let yes = "[Yes]";
    let no = "[No]";
    let total = yes.len() + 1 + no.len();
    let start = rect.x + (rect.width.saturating_sub(total as u16) / 2);
    buf.set_stringn(start, button_y, yes, yes.len(), style);
    buf.set_stringn(
        start + yes.len() as u16 + 1,
        button_y,
        no,
        no.len(),
        style,
    );
}

fn contains(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && y >= rect.y
        && x < rect.x.saturating_add(rect.width)
        && y < rect.y.saturating_add(rect.height)
}

fn fill_rect(buf: &mut Buffer, rect: Rect, style: Style) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    for y in rect.y..rect.y.saturating_add(rect.height) {
        for x in rect.x..rect.x.saturating_add(rect.width) {
            buf[(x, y)].set_symbol(" ").set_style(style);
        }
    }
}
