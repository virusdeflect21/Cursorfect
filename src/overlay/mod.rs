//! Transparent hardware-accelerated overlay: window + Direct3D 11 renderer presented
//! through DirectComposition (see `renderer.rs`).

pub mod renderer;
pub mod window;

use glam::Vec2;

use crate::effects::{Batch, World};
use renderer::{Crop, FrameResult, Grade, Renderer};
use window::{OverlayWindow, VirtualScreen};

pub struct Overlay {
    // Field order matters: the renderer (COM objects bound to the HWND) drops before the window.
    renderer: Renderer,
    window: OverlayWindow,
}

impl Overlay {
    pub fn create() -> anyhow::Result<Self> {
        let window = OverlayWindow::create()?;
        let s = window.screen;
        let origin = Vec2::new(s.x as f32, s.y as f32);
        let renderer = Renderer::new(
            window.hwnd(),
            origin,
            s.w as u32,
            s.h as u32,
            window.capture_excluded,
        )?;
        Ok(Self { renderer, window })
    }

    pub fn screen(&self) -> VirtualScreen {
        self.window.screen
    }

    pub fn origin(&self) -> Vec2 {
        Vec2::new(self.window.screen.x as f32, self.window.screen.y as f32)
    }

    pub fn size(&self) -> Vec2 {
        Vec2::new(self.window.screen.w as f32, self.window.screen.h as f32)
    }

    pub fn set_visible(&mut self, v: bool) {
        self.window.set_visible(v);
    }

    pub fn is_visible(&self) -> bool {
        self.window.is_visible()
    }

    pub fn raise(&self) {
        self.window.raise();
    }

    pub fn pump(&self) {
        self.window.pump();
    }

    /// True if something non-transparent is currently on the surface.
    pub fn has_content(&self) -> bool {
        self.renderer.dirty_surface
    }

    pub fn render(
        &mut self,
        batch: &Batch,
        world: Option<&World>,
        crops: &[Crop],
        grade: &Grade,
    ) -> FrameResult {
        self.renderer.render(batch, world, crops, grade)
    }

    /// The World: capture the screen now and keep showing that image.
    pub fn freeze(&mut self) -> bool {
        self.renderer.freeze()
    }

    pub fn unfreeze(&mut self) {
        self.renderer.unfreeze();
    }

    pub fn set_demo_desktop(&mut self, bgra: &[u8], w: u32, h: u32) -> anyhow::Result<()> {
        self.renderer.set_demo_desktop(bgra, w, h)
    }

    pub fn prewarm(&mut self) {
        self.renderer.prewarm();
    }

    pub fn is_frozen(&self) -> bool {
        self.renderer.is_frozen()
    }

    /// Raw window handle (for the watchdog, which may only hide it asynchronously).
    pub fn hwnd_raw(&self) -> isize {
        self.window.hwnd().0 as isize
    }

    /// Float render target available (colours above 1.0 bloom instead of clipping).
    pub fn hdr_available(&self) -> bool {
        self.renderer.hdr_available()
    }

    pub fn world_supported(&self) -> bool {
        self.renderer.world_supported()
    }

    pub fn release_world(&mut self) {
        self.renderer.release_world();
    }

    pub fn cursor_handle(&self) -> isize {
        self.renderer.cursor_handle()
    }

    pub fn has_cursor_texture(&self) -> bool {
        self.renderer.has_cursor_texture()
    }

    pub fn set_cursor(&mut self, img: &crate::platform::native::CursorImage) {
        self.renderer.set_cursor(img);
    }

    pub fn dump_bmp(&self, path: &std::path::Path) -> anyhow::Result<()> {
        self.renderer.dump_bmp(path)
    }

    /// Does the current window geometry still match the desktop?
    pub fn matches_desktop(&self) -> bool {
        VirtualScreen::query() == self.window.screen
    }
}
