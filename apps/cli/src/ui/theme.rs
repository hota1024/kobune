//! Colour, symbol and frame, decided in one place.
//!
//! Views ask for intent — `theme::link()`, `theme::service_state(…)` —
//! rather than for a colour, so the palette is one file rather than a
//! search across every view.
//!
//! **The colours are the ANSI sixteen, never RGB.** Those are the ones a
//! terminal theme is allowed to redefine; a hand-picked grey that reads
//! well on black disappears on white.
//!
//! **And nothing a person reads is pushed down.** Secondary text was
//! ANSI 8 — "bright black" — which is the colour a theme is most free to
//! put wherever it likes, and what most dark themes do with it is sit it
//! close to the background. Dimming the terminal's own foreground was
//! tried next and came out too faint to read on an ordinary terminal.
//!
//! Both were the same mistake: there is no way to make text quieter that
//! is safe on every terminal, because quieter is the direction the
//! reader's own settings have already gone. So the hierarchy is built the
//! other way — the things that matter are **pulled up**, with weight and
//! with colour, and everything else is the plain foreground the reader
//! chose. The loudest thing on a panel is a URL or a state; the quietest
//! is ordinary text; nothing is a shade of the background.
//!
//! The frame is the one exception, and it is not text.

use kobune_api::CheckStatus;
use kobune_core::ServiceState;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Padding};

/// Text in a supporting role: labels, paths, the punctuation between
/// fields, the sentence under a panel.
///
/// **Plain, deliberately** — see the note at the top of this file. It is
/// named for the job it does rather than for how it looks, because how
/// it looks is "however the reader has their terminal set", and that is
/// the only setting guaranteed to be readable. What separates it from
/// the things around it is that they are bold or coloured and it is not.
///
/// Kept as a call rather than dropped for [`ratatui::text::Span::raw`]:
/// it is 70-odd places saying which text is which, and if this decision
/// is ever revisited it is revisited here.
pub fn secondary() -> Style {
    Style::new()
}

/// The frame, and the rules inside it.
///
/// The one thing that may fade almost to nothing: it is decoration, the
/// columns line up without it, and a border as legible as the text would
/// compete with what it is drawn around. This is where ANSI 8 belongs.
pub fn frame() -> Style {
    Style::new().fg(Color::DarkGray)
}

/// What a view is about — a workspace, a service, a variable's key.
pub fn subject() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
}

/// A column heading, and the caption over a block.
///
/// Weight, because a heading is structure and structure is worth
/// finding. It comes out the same as [`subject`] today and is a separate
/// name because it is a separate question — and because what tells a
/// `WORKSPACE` from an `api` is that one is a column of capitals above
/// the other, not the style either is drawn in.
pub fn heading() -> Style {
    Style::new().add_modifier(Modifier::BOLD)
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
        ServiceState::Stopped => secondary(),
        ServiceState::Failed { .. } => bad(),
        ServiceState::Unknown => secondary(),
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
    /// For a view drawn inside a frame somebody else already drew.
    ///
    /// The full-screen mode is the caller: its panes have borders of
    /// their own, and a second box inside the first says nothing. Not
    /// [`Self::PLAIN`], which is the shape a pipe gets and drops the
    /// colour with the frame — on a screen there is a terminal to show it.
    pub const BARE: Self = Self {
        borders: false,
        styled: true,
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
            // **The title does not inherit the border's colour.** ratatui
            // draws the border first and the title over it, so a span
            // that asks for weight and not for colour — which every one
            // of these does — came out in whatever the border was. The
            // panel's own name was the dimmest thing on it.
            .title_style(Style::new().fg(Color::Reset))
            .border_type(BorderType::Rounded)
            .border_style(frame())
            .padding(Padding::horizontal(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nothing_a_person_reads_is_painted_the_colour_a_theme_may_hide() {
        // ANSI 8 is the colour a terminal theme is most free to put where
        // it likes, and what most dark themes do with it is sit it close
        // to the background. The frame may fade with it; text may not.
        for style in [
            secondary(),
            heading(),
            subject(),
            link(),
            command(),
            good(),
            warn(),
            bad(),
        ] {
            assert_ne!(style.fg, Some(Color::DarkGray));
        }

        for state in [
            ServiceState::Ready,
            ServiceState::Starting,
            ServiceState::Idle,
            ServiceState::Stopped,
            ServiceState::failed("x"),
            ServiceState::Unknown,
        ] {
            assert_ne!(service_state(&state).fg, Some(Color::DarkGray));
        }
    }

    #[test]
    fn nothing_a_person_reads_is_made_quieter_than_plain() {
        // Quieter is the direction the reader's own settings have
        // already gone: ANSI 8 landed on the background, and dimming was
        // too faint to read. The hierarchy is built by pulling the
        // things that matter up instead.
        for style in [secondary(), heading(), subject()] {
            assert_eq!(style.fg, None, "the reader's own foreground");
            assert!(!style.add_modifier.contains(Modifier::DIM));
        }

        assert_eq!(secondary(), Style::new(), "plain, and nothing else");
    }

    #[test]
    fn what_matters_is_pulled_up_instead() {
        // Which is the half of the bargain that makes the other half
        // work: with nothing pushed down, a panel reads because its
        // states, URLs and names are louder than the text around them.
        assert!(subject().add_modifier.contains(Modifier::BOLD));
        assert!(heading().add_modifier.contains(Modifier::BOLD));

        for style in [link(), command(), good(), warn(), bad()] {
            assert!(style.fg.is_some(), "carries a colour");
        }
    }

    #[test]
    fn the_frame_is_the_one_thing_allowed_to_fade() {
        // It is decoration. The columns line up without it, and a border
        // as legible as the text competes with what it is drawn around.
        assert_eq!(frame().fg, Some(Color::DarkGray));
    }

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
