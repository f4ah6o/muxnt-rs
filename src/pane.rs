use shadow_terminal::active_terminal::ActiveTerminal;
use shadow_terminal::output::native::{CompleteSurface, Output, ScreenMode, SurfaceDiff};
use shadow_terminal::termwiz::surface::Surface;

use crate::types::PaneId;

pub struct Pane {
    pub id: PaneId,
    pub terminal: ActiveTerminal,
    pub screen_surface: Surface,
    pub scrollback_surface: Surface,
    pub screen_mode: ScreenMode,
    pub scroll_position: usize,
    pub scrollback_height: usize,
    pub size: (u16, u16),
    pub exited: bool,
}

impl Pane {
    pub fn new(id: PaneId, terminal: ActiveTerminal, width: u16, height: u16) -> Self {
        let width = width.max(1);
        let height = height.max(1);
        let screen_surface = Surface::new(width as usize, height as usize);
        let scrollback_surface = Surface::new(width as usize, height as usize);
        Self {
            id,
            terminal,
            screen_surface,
            scrollback_surface,
            screen_mode: ScreenMode::Primary,
            scroll_position: 0,
            scrollback_height: height as usize,
            size: (width, height),
            exited: false,
        }
    }

    pub fn apply_output(&mut self, output: Output) {
        match output {
            Output::Diff(diff) => match diff {
                SurfaceDiff::Scrollback(scrollback) => {
                    let (cols, _) = scrollback.size;
                    if self.scrollback_surface.dimensions() != (cols, scrollback.height) {
                        self.scrollback_surface = Surface::new(cols, scrollback.height);
                    }
                    self.scrollback_surface.add_changes(scrollback.changes);
                    self.scroll_position = scrollback.position;
                    self.scrollback_height = scrollback.height;
                }
                SurfaceDiff::Screen(screen) => {
                    let (cols, rows) = screen.size;
                    if self.screen_surface.dimensions() != (cols, rows) {
                        self.screen_surface = Surface::new(cols, rows);
                    }
                    self.screen_surface.add_changes(screen.changes);
                    self.screen_mode = screen.mode;
                }
                _ => {}
            },
            Output::Complete(complete) => match complete {
                CompleteSurface::Scrollback(scrollback) => {
                    self.scrollback_height = scrollback.surface.dimensions().1;
                    self.scroll_position = scrollback.position;
                    self.scrollback_surface = scrollback.surface;
                }
                CompleteSurface::Screen(screen) => {
                    self.screen_surface = screen.surface;
                    self.screen_mode = screen.mode;
                }
                _ => {}
            },
            _ => {}
        }
    }

    pub fn resize(&mut self, width: u16, height: u16) {
        let width = width.max(1);
        let height = height.max(1);
        if self.size != (width, height) {
            self.size = (width, height);
            let _ = self.terminal.resize(width, height);
        }
    }
}
