//! Living in the menu bar.
//!
//! Kobune's GUI is not something to keep open; it is mostly for glancing
//! at which environments are running and opening one. GPUI cannot do a
//! tray on its own, so `tray-icon` handles that part.
//!
//! GPUI owns the event loop, so tray events are polled from GPUI's
//! executor instead.

use std::collections::HashMap;
use std::sync::Mutex;

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

/// Something the tray was asked to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Show the window.
    Show,
    /// Open a URL in the browser.
    Open(String),
    Quit,
}

pub struct Tray {
    /// Dropped, the tray disappears.
    _icon: TrayIcon,
    menu: Menu,
    /// Menu item id to action.
    actions: Mutex<HashMap<tray_icon::menu::MenuId, Action>>,
    /// What the menu currently shows. Rebuilt only when this changes.
    shown: Mutex<Vec<(String, String)>>,
}

impl Tray {
    /// Builds the tray. The GUI works without one, hence `Option`.
    pub fn new() -> Option<Self> {
        let menu = Menu::new();

        let icon = TrayIconBuilder::new()
            .with_tooltip("Kobune")
            .with_icon(icon())
            .with_menu(Box::new(menu.clone()))
            .build()
            .map_err(|err| tracing::warn!("cannot create the tray icon: {err}"))
            .ok()?;

        let tray = Self {
            _icon: icon,
            menu,
            actions: Mutex::new(HashMap::new()),
            shown: Mutex::new(Vec::new()),
        };

        tray.rebuild(&[]);
        Some(tray)
    }

    /// Brings the menu in line with the state.
    ///
    /// Called every frame, so it **does nothing when nothing changed** —
    /// a rebuild closes the menu out from under whoever has it open.
    pub fn sync(&self, entries: &[(String, String)]) {
        let changed = self
            .shown
            .lock()
            .map(|shown| shown.as_slice() != entries)
            .unwrap_or(true);

        if !changed {
            return;
        }

        self.rebuild(entries);
    }

    fn rebuild(&self, entries: &[(String, String)]) {
        // Take the existing items out before putting new ones in.
        // Removing by position from the front works for every item kind.
        while self.menu.remove_at(0).is_some() {}

        let mut actions = HashMap::new();

        let show = MenuItem::new("Open Kobune", true, None);
        actions.insert(show.id().clone(), Action::Show);
        let _ = self.menu.append(&show);

        let _ = self.menu.append(&PredefinedMenuItem::separator());

        if entries.is_empty() {
            let empty = MenuItem::new("No running environments", false, None);
            let _ = self.menu.append(&empty);
        } else {
            for (label, url) in entries {
                let item = MenuItem::new(label, true, None);
                actions.insert(item.id().clone(), Action::Open(url.clone()));
                let _ = self.menu.append(&item);
            }
        }

        let _ = self.menu.append(&PredefinedMenuItem::separator());

        let quit = MenuItem::new("Quit", true, None);
        actions.insert(quit.id().clone(), Action::Quit);
        let _ = self.menu.append(&quit);

        if let Ok(mut guard) = self.actions.lock() {
            *guard = actions;
        }
        if let Ok(mut guard) = self.shown.lock() {
            *guard = entries.to_vec();
        }
    }

    /// Takes whatever actions have piled up.
    pub fn poll(&self) -> Vec<Action> {
        let mut actions = Vec::new();

        while let Ok(event) = MenuEvent::receiver().try_recv() {
            let action = self
                .actions
                .lock()
                .ok()
                .and_then(|guard| guard.get(&event.id).cloned());

            if let Some(action) = action {
                actions.push(action);
            }
        }

        actions
    }
}

/// The tray icon.
///
/// Drawn on the spot, so no image file has to ship with the binary.
fn icon() -> tray_icon::Icon {
    const SIZE: u32 = 32;

    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let center = (SIZE as f32 - 1.0) / 2.0;
    let radius = center - 3.0;

    for y in 0..SIZE {
        for x in 0..SIZE {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            let distance = (dx * dx + dy * dy).sqrt();

            // Soften the edge a little; at 1× the jaggies show.
            let alpha = ((radius - distance).clamp(0.0, 1.0) * 255.0) as u8;

            // Black, so it is treated as a template image. macOS inverts
            // it for dark mode by itself.
            rgba.extend_from_slice(&[0, 0, 0, alpha]);
        }
    }

    tray_icon::Icon::from_rgba(rgba, SIZE, SIZE).expect("a bitmap we generated is always valid")
}

/// Polls tray events and keeps the menu in step with the state.
///
/// There is no way into GPUI's event loop, so this looks in at a short
/// interval. Menus are operated at human speed; nothing gets missed.
pub fn spawn_poller(tray: Tray, state: crate::state::SharedState, cx: &mut gpui::App) {
    const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(200);

    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(POLL_INTERVAL).await;

            let entries = state.read(|state| state.menu_entries()).unwrap_or_default();
            tray.sync(&entries);

            for action in tray.poll() {
                match action {
                    Action::Show => {
                        cx.update(|cx| cx.activate(true));
                    }
                    Action::Open(url) => crate::app::open_url(&url),
                    Action::Quit => {
                        cx.update(|cx| cx.quit());
                        return;
                    }
                }
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_the_expected_dimensions() {
        // from_rgba fails when the dimensions and the buffer disagree.
        let _ = icon();
    }

    #[test]
    fn actions_compare_by_value() {
        assert_eq!(
            Action::Open("https://x".into()),
            Action::Open("https://x".into())
        );
        assert_ne!(Action::Show, Action::Quit);
    }
}
