//! Colour, symbol and frame, decided in one place.
//!
//! Views ask for intent — `theme::link()`, `theme::service_state(…)` —
//! rather than for a colour, so the palette is one file rather than a
//! search across every view.
//!
//! **The colours are the ANSI sixteen, never RGB.** Those are the ones a
//! terminal theme is allowed to redefine; a hand-picked grey that reads
//! well on black disappears on white.

use kobune_api::CheckStatus;
use kobune_core::ServiceState;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding};

/// Secondary text: labels, paths, the punctuation between fields.
pub fn muted() -> Style {
    Style::new().fg(Color::DarkGray)
}

/// What a view is about — a workspace, a service, a variable's key.
pub fn subject() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// A column heading.
pub fn heading() -> Style {
    Style::new()
        .fg(Color::DarkGray)
        .add_modifier(Modifier::BOLD)
}

/// Somewhere to go: a URL, or an address worth copying.
pub fn link() -> Style {
    Style::new().fg(Color::Cyan)
}

/// Something the reader is meant to type.
pub fn command() -> Style {
    Style::new().fg(Color::Magenta)
}

/// It worked, or it is where it should be.
pub fn good() -> Style {
    Style::new().fg(Color::Green)
}

/// It worked, and wants looking at: a service still starting, an
/// environment now on the internet.
///
/// **Not [`bad`].** These are printed by commands that succeeded, and a
/// red line under a ✓ is read as the command having failed — which is
/// what happened to `tunnel enable --public`.
pub fn warn() -> Style {
    Style::new().fg(Color::Yellow)
}

/// Something failed: a service that will not start, work a command could
/// not finish. Nothing that merely deserves care.
pub fn bad() -> Style {
    Style::new().fg(Color::Red)
}

/// The dot in front of a service.
///
/// Shape carries the meaning as well as colour, so the state survives a
/// monochrome terminal and a reader who cannot tell red from green.
pub fn service_symbol(state: &ServiceState) -> &'static str {
    match state {
        ServiceState::Ready => "●",
        ServiceState::Starting => "◐",
        ServiceState::Idle => "◑",
        ServiceState::Stopped => "○",
        ServiceState::Failed { .. } => "✗",
        ServiceState::Unknown => "?",
    }
}

pub fn service_state(state: &ServiceState) -> Style {
    match state {
        ServiceState::Ready => good(),
        ServiceState::Starting => warn(),
        // Idle is running, just untouched — worth telling apart from ready
        // at a glance, without looking like a problem.
        ServiceState::Idle => Style::new().fg(Color::Blue),
        ServiceState::Stopped => muted(),
        ServiceState::Failed { .. } => bad(),
        ServiceState::Unknown => muted(),
    }
}

pub fn check_status(status: CheckStatus) -> Style {
    match status {
        CheckStatus::Ok => good(),
        CheckStatus::Warn => warn(),
        CheckStatus::Fail => bad(),
    }
}

/// How much decoration the destination can take.
///
/// A terminal gets the frame. A pipe does not: box-drawing characters in
/// the middle of a `kobune ls | grep` are noise, and the point of these
/// views is that they stay greppable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decor {
    pub borders: bool,
    /// Whether colour reaches a person, rather than a log file.
    ///
    /// **Not for picking colours.** A view names its intent and this file
    /// decides, and a style that is never written costs nothing. It is for
    /// the one thing a view cannot say in colour and cannot say in words
    /// either: a QR code drawn without one is at the mercy of the
    /// terminal's own, and on a dark theme that is an inverted code.
    pub styled: bool,
}

impl Decor {
    pub const FRAMED: Self = Self {
        borders: true,
        styled: true,
    };
    pub const PLAIN: Self = Self {
        borders: false,
        styled: false,
    };

    /// The same, with the colour dropped: `NO_COLOR` on a terminal that
    /// can otherwise take everything.
    pub fn unstyled(self) -> Self {
        Self {
            styled: false,
            ..self
        }
    }

    /// The rows the frame costs. One for the title either way; a bordered
    /// frame pays for its bottom edge too.
    pub fn frame_height(self) -> u16 {
        if self.borders { 2 } else { 1 }
    }

    /// The columns the frame costs: two borders and a space of padding on
    /// each side.
    pub fn frame_width(self) -> u16 {
        if self.borders { 4 } else { 0 }
    }

    /// The block every view sits in.
    ///
    /// Titled the same way in both modes, so what a person reads on screen
    /// and what lands in a log differ by decoration alone.
    pub fn block<'a>(self, title: Line<'a>) -> Block<'a> {
        if !self.borders {
            return Block::new().title(title);
        }

        // ratatui writes the title straight onto the border, so a title
        // that is not padded here comes out welded to the corner:
        // `╭services────╮`.
        let mut padded = Line::default();
        padded.push_span(Span::raw(" "));
        for span in title.spans {
            padded.push_span(span);
        }
        padded.push_span(Span::raw(" "));

        Block::bordered()
            .title(padded)
            .border_type(BorderType::Rounded)
            .border_style(muted())
            .padding(Padding::horizontal(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_service_state_has_a_shape_of_its_own() {
        // Colour alone would leave a monochrome terminal — and a
        // red-green colour-blind reader — with nothing to go on.
        let states = [
            ServiceState::Ready,
            ServiceState::Starting,
            ServiceState::Idle,
            ServiceState::Stopped,
            ServiceState::failed("x"),
            ServiceState::Unknown,
        ];

        let symbols: std::collections::BTreeSet<&str> = states.iter().map(service_symbol).collect();

        assert_eq!(symbols.len(), states.len(), "two states share a symbol");
    }

    #[test]
    fn service_symbols_are_one_column_wide() {
        // The tables align by counting columns, so a two-column glyph
        // would shear every row below it.
        for state in [ServiceState::Ready, ServiceState::Stopped] {
            assert_eq!(service_symbol(&state).chars().count(), 1);
        }
    }

    #[test]
    fn a_plain_frame_still_costs_its_title_row() {
        assert_eq!(Decor::PLAIN.frame_height(), 1);
        assert_eq!(Decor::PLAIN.frame_width(), 0);
        assert_eq!(Decor::FRAMED.frame_height(), 2);
    }
}
