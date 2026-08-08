//! The shape every view has.
//!
//! A title, then sections separated by a blank row. A section is either
//! free lines or a grid whose columns are as wide as what is in them.
//! Views describe themselves in these terms and never touch a [`Rect`], so
//! they read as content rather than as layout arithmetic.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Row, Table, Widget};
use unicode_width::UnicodeWidthChar as _;

use super::View;
use super::theme::{self, Decor};

/// The gap between a grid's columns.
const COLUMN_SPACING: u16 = 2;

/// A table sized by its contents.
///
/// ratatui hands out width from constraints, which is right for a screen
/// that has to be filled and wrong here: these tables are printed once,
/// and a URL cut in half is a URL nobody can use. So the content decides,
/// and the frame is drawn around the result.
#[derive(Debug, Default)]
pub struct Grid {
    caption: Option<Line<'static>>,
    header: Option<Vec<Line<'static>>>,
    rows: Vec<Vec<Line<'static>>>,
}

impl Grid {
    pub fn new() -> Self {
        Self::default()
    }

    /// A line above the table, kept in the same section so that no blank
    /// row comes between a heading and what it heads.
    pub fn caption(mut self, caption: impl Into<Line<'static>>) -> Self {
        self.caption = Some(caption.into());
        self
    }

    pub fn header(mut self, cells: Vec<Line<'static>>) -> Self {
        self.header = Some(cells);
        self
    }

    pub fn push(&mut self, cells: Vec<Line<'static>>) {
        self.rows.push(cells);
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The width each column needs, over the header and every row.
    fn column_widths(&self) -> Vec<u16> {
        let mut widths: Vec<u16> = Vec::new();

        for cells in self.header.iter().chain(self.rows.iter()) {
            for (column, cell) in cells.iter().enumerate() {
                let width = u16::try_from(cell.width()).unwrap_or(u16::MAX);
                match widths.get_mut(column) {
                    Some(current) => *current = (*current).max(width),
                    None => widths.push(width),
                }
            }
        }

        widths
    }

    fn width(&self) -> u16 {
        let widths = self.column_widths();
        let columns = u16::try_from(widths.len()).unwrap_or(0);

        let table = widths
            .iter()
            .copied()
            .sum::<u16>()
            .saturating_add(COLUMN_SPACING.saturating_mul(columns.saturating_sub(1)));

        let caption = self
            .caption
            .as_ref()
            .map(|line| u16::try_from(line.width()).unwrap_or(u16::MAX))
            .unwrap_or(0);

        table.max(caption)
    }

    fn height(&self) -> u16 {
        let rows = u16::try_from(self.rows.len()).unwrap_or(u16::MAX);
        rows.saturating_add(u16::from(self.header.is_some()))
            .saturating_add(u16::from(self.caption.is_some()))
    }

    fn render(&self, mut area: Rect, buf: &mut Buffer) {
        if let Some(caption) = &self.caption {
            caption.clone().render(Rect { height: 1, ..area }, buf);
            area.y = area.y.saturating_add(1);
            area.height = area.height.saturating_sub(1);
        }

        let widths = self.column_widths();

        // Every column is given exactly what it asked for except the last,
        // which takes the slack. `Fill` rather than `Min` deliberately:
        // ratatui's solver ranks `Min` above `Length`, so in a window too
        // narrow for the table a `Min` last column would satisfy itself by
        // squeezing the name column down to nothing. `Fill` yields first,
        // which puts the truncation in the column chosen to bear it.
        let constraints: Vec<Constraint> = widths
            .iter()
            .enumerate()
            .map(|(index, width)| {
                if index + 1 == widths.len() {
                    Constraint::Fill(1)
                } else {
                    Constraint::Length(*width)
                }
            })
            .collect();

        let rows = self
            .rows
            .iter()
            .map(|cells| Row::new(cells.iter().cloned()));

        let mut table = Table::new(rows, constraints).column_spacing(COLUMN_SPACING);

        if let Some(header) = &self.header {
            table = table.header(Row::new(header.iter().cloned()).style(theme::heading()));
        }

        Widget::render(table, area, buf);
    }
}

/// One block within a panel.
#[derive(Debug)]
pub enum Section {
    Lines(Vec<Line<'static>>),
    Grid(Grid),
}

impl Section {
    fn width(&self) -> u16 {
        match self {
            Self::Lines(lines) => lines
                .iter()
                .map(|line| u16::try_from(line.width()).unwrap_or(u16::MAX))
                .max()
                .unwrap_or(0),
            Self::Grid(grid) => grid.width(),
        }
    }

    /// How tall this is once wrapped to `width`.
    ///
    /// At the width a view asks for, nothing wraps and this is simply the
    /// number of lines. It matters in a window narrower than that, where
    /// the alternative is cutting off half of a `sudo …` line the reader
    /// is being told to run.
    fn height(&self, width: u16) -> u16 {
        match self {
            Self::Lines(lines) => u16::try_from(wrap(lines, width).len()).unwrap_or(u16::MAX),
            Self::Grid(grid) => grid.height(),
        }
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        match self {
            Self::Lines(lines) => {
                for (index, line) in wrap(lines, area.width).into_iter().enumerate() {
                    let Some(y) = area.y.checked_add(u16::try_from(index).unwrap_or(u16::MAX))
                    else {
                        return;
                    };
                    if y >= area.bottom() {
                        return;
                    }
                    line.render(Rect::new(area.x, y, area.width, 1), buf);
                }
            }
            Self::Grid(grid) => grid.render(area, buf),
        }
    }
}

/// Breaks lines too wide for the space they are given.
///
/// At the column and not at a space: what overflows a panel here is a
/// shell command or a path, and breaking one on whitespace would put a
/// line ending exactly where a reader would believe there is not one.
///
/// ratatui's own `Paragraph` wraps, but the method that says how tall the
/// result will be is behind an unstable feature — and the height has to be
/// known before the area is allocated. Doing it here keeps the count and
/// the drawing from ever disagreeing.
fn wrap(lines: &[Line<'static>], width: u16) -> Vec<Line<'static>> {
    let limit = usize::from(width);
    if limit == 0 {
        return Vec::new();
    }

    let mut wrapped = Vec::new();

    for line in lines {
        if line.width() <= limit {
            wrapped.push(line.clone());
            continue;
        }

        let mut row: Vec<Span<'static>> = Vec::new();
        let mut used = 0usize;

        for span in &line.spans {
            let mut text = String::new();

            for character in span.content.chars() {
                let advance = character.width().unwrap_or(0);

                if used + advance > limit {
                    if !text.is_empty() {
                        row.push(Span::styled(std::mem::take(&mut text), span.style));
                    }
                    wrapped.push(Line::from(std::mem::take(&mut row)).style(line.style));
                    used = 0;
                }

                text.push(character);
                used += advance;
            }

            if !text.is_empty() {
                row.push(Span::styled(text, span.style));
            }
        }

        if !row.is_empty() {
            wrapped.push(Line::from(row).style(line.style));
        }
    }

    wrapped
}

/// A titled block of output.
#[derive(Debug)]
pub struct Panel {
    decor: Decor,
    title: Line<'static>,
    sections: Vec<Section>,
}

impl Panel {
    pub fn new(decor: Decor, title: impl Into<Line<'static>>) -> Self {
        Self {
            decor,
            title: title.into(),
            sections: Vec::new(),
        }
    }

    /// Adds free lines. An empty set adds nothing, so a view can offer a
    /// section that only sometimes has anything in it without checking.
    pub fn lines(mut self, lines: Vec<Line<'static>>) -> Self {
        if !lines.is_empty() {
            self.sections.push(Section::Lines(lines));
        }
        self
    }

    pub fn line(self, line: impl Into<Line<'static>>) -> Self {
        self.lines(vec![line.into()])
    }

    pub fn grid(mut self, grid: Grid) -> Self {
        if !grid.is_empty() {
            self.sections.push(Section::Grid(grid));
        }
        self
    }
}

impl View for Panel {
    fn preferred_width(&self) -> u16 {
        // A title is drawn into the top border with a space either side of
        // it, so it asks for two columns more than it measures.
        let title = u16::try_from(self.title.width())
            .unwrap_or(u16::MAX)
            .saturating_add(2);

        self.sections
            .iter()
            .map(Section::width)
            .max()
            .unwrap_or(0)
            .max(title)
            .saturating_add(self.decor.frame_width())
    }

    fn height(&self, width: u16) -> u16 {
        let inner = width.saturating_sub(self.decor.frame_width());

        let content: u16 = self
            .sections
            .iter()
            .map(|section| section.height(inner))
            .fold(0u16, u16::saturating_add);

        // One blank row between neighbouring sections.
        let gaps = u16::try_from(self.sections.len().saturating_sub(1)).unwrap_or(0);

        content
            .saturating_add(gaps)
            .saturating_add(self.decor.frame_height())
    }

    fn render(&self, area: Rect, buf: &mut Buffer) {
        let block = self.decor.block(self.title.clone());
        let inner = block.inner(area);
        block.render(area, buf);

        let constraints: Vec<Constraint> = self
            .sections
            .iter()
            .enumerate()
            .flat_map(|(index, section)| {
                let gap = (index > 0).then_some(Constraint::Length(1));
                gap.into_iter().chain(std::iter::once(Constraint::Length(
                    section.height(inner.width),
                )))
            })
            .collect();

        let areas = Layout::vertical(constraints).split(inner);

        // The gaps take a slot each, so the sections sit on the odd ones.
        for (section, area) in self.sections.iter().zip(areas.iter().step_by(2)) {
            section.render(*area, buf);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::test_support::render;

    fn grid() -> Grid {
        let mut grid = Grid::new();
        grid.push(vec!["web".into(), "ready".into()]);
        grid.push(vec!["database".into(), "stopped".into()]);
        grid
    }

    #[test]
    fn a_column_is_as_wide_as_its_widest_cell() {
        // "database" is eight, "stopped" is seven, two of spacing between.
        assert_eq!(grid().width(), 8 + COLUMN_SPACING + 7);
    }

    #[test]
    fn columns_line_up() {
        // The second column starts at the same offset on every row, which
        // is the whole reason a table is drawn rather than printed.
        let panel = Panel::new(Decor::PLAIN, "services").grid(grid());

        assert_eq!(
            render(&panel),
            "services\nweb       ready\ndatabase  stopped\n"
        );
    }

    #[test]
    fn a_caption_sits_directly_above_its_table() {
        // As a section of its own it would be pushed a blank row away
        // from the rows it heads, and read as heading the whole panel.
        let panel = Panel::new(Decor::PLAIN, "status").grid(grid().caption("shared:"));

        assert_eq!(
            render(&panel),
            "status\nshared:\nweb       ready\ndatabase  stopped\n"
        );
    }

    #[test]
    fn sections_are_separated_by_one_blank_row() {
        let panel = Panel::new(Decor::PLAIN, "title")
            .line(Line::raw("first"))
            .line(Line::raw("second"));

        assert_eq!(render(&panel), "title\nfirst\n\nsecond\n");
    }

    #[test]
    fn an_empty_section_is_not_a_section() {
        // Otherwise every view would have to guard each optional block
        // itself, and one of them would eventually forget and print a
        // stray blank line.
        let panel = Panel::new(Decor::PLAIN, "title")
            .lines(vec![])
            .grid(Grid::new())
            .line(Line::raw("body"));

        assert_eq!(render(&panel), "title\nbody\n");
    }

    #[test]
    fn a_frame_is_drawn_round_the_content() {
        let panel = Panel::new(Decor::FRAMED, "services").grid(grid());
        let text = render(&panel);

        // Padded away from the corner: `╭services─` is unreadable.
        assert!(text.starts_with("╭ services "), "got:\n{text}");
        assert!(text.trim_end().ends_with('╯'), "got:\n{text}");
        assert!(text.contains("│ web"), "got:\n{text}");
    }

    #[test]
    fn the_frame_never_cuts_the_content_it_surrounds() {
        let panel = Panel::new(Decor::FRAMED, "services").grid(grid());
        let text = render(&panel);

        assert!(text.contains("database"), "got:\n{text}");
        assert!(text.contains("stopped"), "got:\n{text}");
    }

    #[test]
    fn a_line_too_wide_for_the_space_is_wrapped_rather_than_cut() {
        // `doctor` prints commands to run. Half a `sudo` line is worse
        // than useless.
        let command =
            "sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain";
        let wrapped = wrap(&[Line::raw(command)], 40);

        assert!(wrapped.len() > 1);
        let rejoined: String = wrapped.iter().map(Line::to_string).collect();
        assert_eq!(rejoined, command, "wrapping lost or added characters");
        assert!(wrapped.iter().all(|line| line.width() <= 40));
    }

    #[test]
    fn wrapping_keeps_the_style_of_what_it_splits() {
        use ratatui::style::Stylize;

        let wrapped = wrap(&[Line::from("aaaaaa".green())], 2);

        assert_eq!(wrapped.len(), 3);
        for line in &wrapped {
            assert!(line.spans.iter().all(|span| span.style.fg.is_some()));
        }
    }

    #[test]
    fn the_height_a_section_reports_is_the_height_it_draws() {
        // They are computed in two places and a disagreement clips the
        // last line or leaves a blank one.
        let lines = vec![Line::raw("x".repeat(100)), Line::raw("short")];
        let section = Section::Lines(lines.clone());

        for width in [10u16, 33, 100, 200] {
            assert_eq!(
                u16::try_from(wrap(&lines, width).len()).unwrap(),
                section.height(width),
                "at width {width}"
            );
        }
    }

    #[test]
    fn a_narrow_window_keeps_the_first_column() {
        // ratatui's solver ranks `Min` above `Length`, so a `Min` last
        // column would take its width out of the service names.
        let panel = Panel::new(Decor::PLAIN, "services").grid(grid());

        let area = Rect::new(0, 0, 14, panel.height(14));
        let mut buffer = Buffer::empty(area);
        panel.render(area, &mut buffer);

        let text = crate::ui::surface::render_to_string(&buffer, false);
        assert!(text.contains("database"), "got:\n{text}");
    }

    #[test]
    fn a_long_title_widens_the_panel() {
        // A title wider than the body used to be truncated into the
        // border, which is exactly where a workspace's name lives.
        let title = "myapp / a-rather-long-workspace-name";
        let panel = Panel::new(Decor::FRAMED, title).line(Line::raw("x"));

        assert!(render(&panel).contains(title));
    }
}
