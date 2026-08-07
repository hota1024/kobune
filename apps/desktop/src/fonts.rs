//! 日本語を表示できるようにする。
//!
//! egui は既定で CJK のグリフを持たない。何もしないとブランチ名やパスに
//! 日本語が含まれた瞬間に豆腐になる。フォントを埋め込むとバイナリが
//! 数 MB 増えるので、**システムにあるものを探して使う**。

use std::path::{Path, PathBuf};

/// 探す順。先に見つかったものを使う。
///
/// `.ttc`（フォントコレクション）も扱える。`FontData::index` で
/// コレクション内の何番目を使うか指定できるため。
#[cfg(target_os = "macos")]
const CANDIDATES: &[(&str, u32)] = &[
    ("/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc", 0),
    ("/System/Library/Fonts/Hiragino Sans GB.ttc", 0),
    ("/System/Library/Fonts/ヒラギノ丸ゴ ProN W4.ttc", 0),
    ("/Library/Fonts/Arial Unicode.ttf", 0),
];

#[cfg(target_os = "linux")]
const CANDIDATES: &[(&str, u32)] = &[
    ("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc", 0),
    ("/usr/share/fonts/truetype/fonts-japanese-gothic.ttf", 0),
    (
        "/usr/share/fonts/truetype/noto/NotoSansCJKjp-Regular.otf",
        0,
    ),
];

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const CANDIDATES: &[(&str, u32)] = &[];

/// 見つけた日本語フォント。
pub struct SystemFont {
    pub path: PathBuf,
    pub bytes: Vec<u8>,
    /// コレクション内の何番目か。
    pub index: u32,
}

/// システムから日本語フォントを探す。
pub fn find_japanese() -> Option<SystemFont> {
    CANDIDATES
        .iter()
        .find_map(|(path, index)| load(Path::new(path), *index))
}

fn load(path: &Path, index: u32) -> Option<SystemFont> {
    let bytes = std::fs::read(path).ok()?;

    Some(SystemFont {
        path: path.to_path_buf(),
        bytes,
        index,
    })
}

/// egui にフォントを登録する。
///
/// **既定のフォントの後ろに足す。** 前に置くと英数字までこのフォントに
/// なり、UI の見た目が変わってしまう。足りないグリフだけを補いたい。
pub fn install(ctx: &egui::Context) {
    let Some(font) = find_japanese() else {
        tracing::warn!(
            "日本語フォントが見つかりません。日本語が正しく表示されない可能性があります"
        );
        return;
    };

    tracing::info!("日本語フォント: {}", font.path.display());

    let mut fonts = egui::FontDefinitions::default();

    fonts.font_data.insert(
        "japanese".to_string(),
        egui::FontData {
            font: font.bytes.into(),
            index: font.index,
            tweak: Default::default(),
        },
    );

    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        fonts
            .families
            .entry(family)
            .or_default()
            .push("japanese".to_string());
    }

    ctx.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidates_are_absolute_paths() {
        for (path, _) in CANDIDATES {
            assert!(
                Path::new(path).is_absolute(),
                "探索先は絶対パスである必要がある: {path}"
            );
        }
    }

    #[test]
    fn missing_font_is_not_a_panic() {
        // 見つからなくても落とさない。日本語が豆腐になるだけで、
        // GUI が起動しないよりはるかにまし。
        assert!(load(Path::new("/definitely/not/a/font.ttf"), 0).is_none());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn finds_a_japanese_font_on_macos() {
        // macOS には必ず日本語フォントがある。見つからないなら
        // 探索先のリストが古い。
        let font = find_japanese().expect("日本語フォントが見つかる");

        assert!(!font.bytes.is_empty());
        assert!(font.path.exists());
    }
}
