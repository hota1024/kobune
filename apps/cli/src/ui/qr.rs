//! A URL, drawn to be photographed.
//!
//! Two module rows to a text row, through the half-block glyphs, because a
//! QR module is square and a terminal cell is not: one module per cell
//! would come out twice as tall as it is wide, and a scanner reads that as
//! a different code or as none at all.
//!
//! **Polarity is carried twice, by the glyph and by the colour.** A
//! terminal that takes the styling gets black modules on a white ground
//! however the window is themed — a QR drawn in the terminal's own
//! foreground is inverted on a dark theme, which iOS' camera will not
//! read. Where styling has been turned off, the glyphs are still there and
//! still the right way round, which is what every other terminal QR code
//! is made of.

use qrcode::QrCode;
use qrcode::types::Color as Module;
use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// The blank border the format requires, in modules.
///
/// A code drawn hard against the surrounding text is one a scanner has to
/// find the edges of, and mostly does not.
const QUIET_ZONE: usize = 4;

/// The QR code for `data`, as text rows.
///
/// `None` when there is more data than the format can hold — 2,953 bytes,
/// which no URL reaches, but the error is the encoder's to raise rather
/// than this module's to assume away.
pub fn lines(data: &str) -> Option<Vec<Line<'static>>> {
    let code = QrCode::new(data).ok()?;
    Some(draw(&Grid::new(&code)))
}

/// The modules, with the quiet zone included in the coordinates.
struct Grid {
    modules: Vec<Module>,
    /// The side of the code itself, without the quiet zone.
    side: usize,
}

impl Grid {
    fn new(code: &QrCode) -> Self {
        Self {
            modules: code.to_colors(),
            side: code.width(),
        }
    }

    /// The whole drawing, quiet zone and all.
    fn span(&self) -> usize {
        self.side + QUIET_ZONE * 2
    }

    /// The module at a point in the drawing, where the quiet zone reads as
    /// light. Out of range is light too, which is what makes an odd number
    /// of rows pair with a blank one.
    fn at(&self, x: usize, y: usize) -> Module {
        let (Some(x), Some(y)) = (x.checked_sub(QUIET_ZONE), y.checked_sub(QUIET_ZONE)) else {
            return Module::Light;
        };

        if x >= self.side || y >= self.side {
            return Module::Light;
        }

        self.modules
            .get(y * self.side + x)
            .copied()
            .unwrap_or(Module::Light)
    }
}

fn draw(grid: &Grid) -> Vec<Line<'static>> {
    // One style for the lot of it, so a row leaves the process as a single
    // escape sequence rather than one per module.
    let style = Style::new().fg(Color::Black).bg(Color::White);
    let span = grid.span();

    (0..span)
        .step_by(2)
        .map(|y| {
            let row: String = (0..span)
                .map(|x| glyph(grid.at(x, y), grid.at(x, y + 1)))
                .collect();

            Line::from(Span::styled(row, style))
        })
        .collect()
}

/// The glyph for two stacked modules.
///
/// The upper and lower halves are drawn by the character rather than by
/// the colour, so the code survives `NO_COLOR` and a pipe.
fn glyph(upper: Module, lower: Module) -> char {
    match (upper, lower) {
        (Module::Dark, Module::Dark) => '█',
        (Module::Dark, Module::Light) => '▀',
        (Module::Light, Module::Dark) => '▄',
        (Module::Light, Module::Light) => ' ',
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What a row's glyphs say, as a string.
    fn text(line: &Line<'static>) -> String {
        line.spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect()
    }

    #[test]
    fn a_url_becomes_a_square() {
        let rows = lines("https://web.feat-1.myapp.localhost").expect("encodes");

        let width = text(&rows[0]).chars().count();
        // Two module rows to a text row, so the drawing is half as tall as
        // it is wide, give or take the odd row at the bottom.
        assert!(
            rows.len() * 2 >= width && rows.len() * 2 <= width + 1,
            "{width} wide, {} rows",
            rows.len()
        );
    }

    #[test]
    fn every_row_is_the_same_width() {
        // A ragged row would shear the code, and the panel around it sizes
        // itself on the widest line.
        let rows = lines("https://example.com").expect("encodes");
        let widths: std::collections::BTreeSet<usize> =
            rows.iter().map(|row| text(row).chars().count()).collect();

        assert_eq!(widths.len(), 1, "rows differ in width: {widths:?}");
    }

    #[test]
    fn the_quiet_zone_is_there() {
        // Four modules on every side. Vertically that is two text rows,
        // and they have to be blank or a scanner has no edge to find.
        let rows = lines("https://example.com").expect("encodes");

        for row in rows.iter().take(QUIET_ZONE / 2) {
            assert!(text(row).trim().is_empty(), "top: {:?}", text(row));
        }

        for row in &rows {
            let drawn = text(row);
            assert!(drawn.starts_with("    "), "left: {drawn:?}");
            assert!(drawn.ends_with("    "), "right: {drawn:?}");
        }
    }

    #[test]
    fn the_finder_pattern_is_where_it_should_be() {
        // The first row past the quiet zone carries module rows 0 and 1,
        // which is the top of the finder pattern: seven dark, then a dark
        // edge with light between. That pairs to `█▀▀▀▀▀█` — the shape a
        // scanner looks for, and proof the halves are the right way up.
        let rows = lines("https://example.com").expect("encodes");
        let first = text(&rows[QUIET_ZONE / 2]);

        let found: String = first.chars().skip(QUIET_ZONE).take(7).collect();
        assert_eq!(found, "█▀▀▀▀▀█", "got: {first:?}");
    }

    #[test]
    fn it_is_the_drawing_the_encoder_would_have_made() {
        // **The one check that catches a transposed grid.** A QR has a
        // finder pattern in three of its four corners, and transposing it
        // leaves one in each of those three corners — so a code that has
        // been drawn sideways looks entirely correct until a camera is
        // pointed at it. The encoder's own half-block renderer is the
        // reference, and this is a character-for-character comparison
        // against it.
        let url = "https://web-feat-1.decius.hota.codes";

        let mine = lines(url)
            .expect("encodes")
            .iter()
            .map(text)
            .collect::<Vec<String>>()
            .join("\n");

        let theirs = qrcode::QrCode::new(url)
            .expect("encodes")
            .render::<qrcode::render::unicode::Dense1x2>()
            .quiet_zone(true)
            .module_dimensions(1, 1)
            .build();

        assert_eq!(mine.trim_end(), theirs.trim_end(), "\n{mine}");
    }

    #[test]
    fn it_is_drawn_dark_on_light() {
        // Not in the terminal's own colours: a dark theme would invert the
        // code, and an inverted code is one a phone camera refuses.
        let rows = lines("https://example.com").expect("encodes");

        for span in &rows[0].spans {
            assert_eq!(span.style.fg, Some(Color::Black));
            assert_eq!(span.style.bg, Some(Color::White));
        }
    }

    #[test]
    fn a_row_is_one_span() {
        // Per-module spans would mean an escape sequence per cell, and a
        // code several times the size of the one being drawn.
        let rows = lines("https://example.com").expect("encodes");
        assert!(rows.iter().all(|row| row.spans.len() == 1));
    }
}
