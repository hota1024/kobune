//! メニューバー常駐。
//!
//! Minato の GUI は常時開くものではなく、「今どの環境が動いているか」を
//! 確認して開く用途が主。egui 単体では tray を扱えないため
//! `tray-icon` を併用する。
//!
//! イベントループは eframe が持っているので、tray のイベントは
//! 描画ループからポーリングして拾う。

use std::collections::HashMap;
use std::sync::Mutex;

use tray_icon::menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem};
use tray_icon::{TrayIcon, TrayIconBuilder};

/// tray から要求された操作。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// ウィンドウを出す。
    Show,
    /// URL をブラウザで開く。
    Open(String),
    Quit,
}

pub struct Tray {
    /// 保持しないと tray が消える。
    _icon: TrayIcon,
    menu: Menu,
    /// メニュー項目 id → 操作。
    actions: Mutex<HashMap<tray_icon::menu::MenuId, Action>>,
    /// 今メニューに出している内容。変化したときだけ作り直す。
    shown: Mutex<Vec<(String, String)>>,
}

impl Tray {
    /// tray を作る。作れなくても GUI は動くので `Option` を返す。
    pub fn new() -> Option<Self> {
        let menu = Menu::new();

        let icon = TrayIconBuilder::new()
            .with_tooltip("Minato")
            .with_icon(icon())
            .with_menu(Box::new(menu.clone()))
            .build()
            .map_err(|err| tracing::warn!("メニューバーに常駐できません: {err}"))
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

    /// メニューの内容を状態に合わせる。
    ///
    /// 毎フレーム呼ばれるので、**変化していなければ何もしない**。
    /// 作り直すとメニューが開いている最中に閉じてしまう。
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
        // 既存の項目を外してから積み直す。
        // 位置指定で先頭から抜くのが、種別に依らず確実。
        while self.menu.remove_at(0).is_some() {}

        let mut actions = HashMap::new();

        let show = MenuItem::new("Minato を開く", true, None);
        actions.insert(show.id().clone(), Action::Show);
        let _ = self.menu.append(&show);

        let _ = self.menu.append(&PredefinedMenuItem::separator());

        if entries.is_empty() {
            let empty = MenuItem::new("起動中の環境はありません", false, None);
            let _ = self.menu.append(&empty);
        } else {
            for (label, url) in entries {
                let item = MenuItem::new(label, true, None);
                actions.insert(item.id().clone(), Action::Open(url.clone()));
                let _ = self.menu.append(&item);
            }
        }

        let _ = self.menu.append(&PredefinedMenuItem::separator());

        let quit = MenuItem::new("終了", true, None);
        actions.insert(quit.id().clone(), Action::Quit);
        let _ = self.menu.append(&quit);

        if let Ok(mut guard) = self.actions.lock() {
            *guard = actions;
        }
        if let Ok(mut guard) = self.shown.lock() {
            *guard = entries.to_vec();
        }
    }

    /// たまっている操作を取り出す。
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

/// tray に出すアイコン。
///
/// 画像ファイルを配布物に含めなくて済むよう、その場で描く。
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

            // 縁を少しぼかす。等倍だとギザギザが目立つ。
            let alpha = ((radius - distance).clamp(0.0, 1.0) * 255.0) as u8;

            // テンプレート画像として扱われるよう黒で描く。
            // macOS はダークモードで自動的に反転してくれる。
            rgba.extend_from_slice(&[0, 0, 0, alpha]);
        }
    }

    tray_icon::Icon::from_rgba(rgba, SIZE, SIZE).expect("生成したビットマップは常に妥当")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn icon_has_the_expected_dimensions() {
        // from_rgba は寸法とバッファ長が合わないと失敗する。
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
