//! `.dockerignore`, applied where Docker's own client applies it.
//!
//! **The daemon has never done this filtering.** `docker build` reads the
//! file, works out what it excludes and leaves those paths out of the tar it
//! uploads; the API takes the tar as given. Kobune packs the tar itself, so
//! it has to read the file itself — otherwise a repository that asks for its
//! `node_modules` to be left out has it sent anyway, and `COPY . .` picks it
//! up.
//!
//! The rules are close to `.gitignore` and not the same, which is the trap.
//! A pattern is **anchored to the root of the context**: `node_modules`
//! leaves out the one at the top and no other, where git would leave out
//! every one at any depth. `**/node_modules` is how that is said here. The
//! rest — `*` stopping at a separator, `?` for one character, `!` putting
//! back what an earlier line took out, the last line to match deciding —
//! follows Docker's `patternmatcher`, which is what the client uses.

use std::path::Path;

/// The file, at the root of the build context.
pub(crate) const FILE: &str = ".dockerignore";

/// One line of a `.dockerignore`.
struct Pattern {
    /// The `!` form, which puts back what an earlier line took out.
    exclusion: bool,
    /// The cleaned text, without the `!`. Kept for
    /// [`Ignore::may_hold_an_exception`], which reads patterns as paths
    /// rather than matching with them.
    text: String,
    tokens: Vec<Token>,
}

impl Pattern {
    /// Parses one line. `None` for a line that says nothing.
    fn parse(line: &str) -> Option<Self> {
        // **Before the line is trimmed.** A `#` in the first column is a
        // comment; one after a space is a pattern for a file whose name
        // begins with `#`, and Docker reads it that way.
        if line.starts_with('#') {
            return None;
        }

        let line = line.trim();
        let (exclusion, body) = match line.strip_prefix('!') {
            Some(rest) => (true, rest.trim()),
            None => (false, line),
        };

        if body.is_empty() {
            return None;
        }

        // A pattern is always relative to the root of the context, so a
        // leading separator is decoration rather than an anchor.
        let text = clean(body);
        let text = match text.len() > 1 {
            true => text.strip_prefix('/').unwrap_or(&text).to_string(),
            false => text,
        };

        Some(Self {
            exclusion,
            tokens: tokenise(&text),
            text,
        })
    }

    fn matches(&self, path: &str) -> bool {
        matches(&self.tokens, path)
    }
}

/// A piece of a pattern.
enum Token {
    /// Text that has to be there exactly.
    Text(String),
    /// `?` — one character, but not a separator.
    OneChar,
    /// `*` — any run of characters, but not across a separator.
    Run,
    /// `**` at the end of a pattern — the rest of the path, separators and
    /// all.
    Rest,
    /// `**` with more to come — any number of leading directories,
    /// including none, so `**/foo` finds a `foo` at the top as well.
    AnyDirs,
}

/// What a context's `.dockerignore` leaves out.
#[derive(Default)]
pub(crate) struct Ignore {
    patterns: Vec<Pattern>,
    /// Whether any line puts something back. Without one, a directory that
    /// matches can be skipped without being looked inside.
    exceptions: bool,
}

impl Ignore {
    /// Reads the `.dockerignore` at the root of `context`.
    ///
    /// An absent file leaves everything in. An unreadable one is an error
    /// rather than an empty set of patterns: carrying on would send exactly
    /// the files the repository asked to keep back.
    ///
    /// `dockerfile` is the Dockerfile's path inside the context, when it is
    /// in there at all.
    pub(crate) fn for_context(context: &Path, dockerfile: Option<&str>) -> std::io::Result<Self> {
        let mut ignore = match std::fs::read_to_string(context.join(FILE)) {
            Ok(contents) => Self::parse(&contents),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(err) => return Err(err),
        };

        // **A build's own files survive its own patterns.** `*` followed by
        // a few `!` lines is a common way to say "send almost nothing", and
        // it names the Dockerfile along with everything else. The client
        // puts these two back the same way.
        ignore.keep(FILE);
        if let Some(dockerfile) = dockerfile {
            ignore.keep(dockerfile);
        }

        Ok(ignore)
    }

    pub(crate) fn parse(contents: &str) -> Self {
        let patterns: Vec<Pattern> = contents.lines().filter_map(Pattern::parse).collect();

        Self {
            exceptions: patterns.iter().any(|pattern| pattern.exclusion),
            patterns,
        }
    }

    /// Whether `path` — relative to the context root, `/`-separated — is
    /// left out.
    ///
    /// **A directory being left out takes everything under it.** That is
    /// what makes `node_modules` mean the whole tree rather than an empty
    /// directory entry, so every parent is tried as well as the path.
    pub(crate) fn excludes(&self, path: &str) -> bool {
        let mut excluded = false;

        for pattern in &self.patterns {
            // An ordinary line is only asked about what is still in, and an
            // exception only about what is already out. Anything else is a
            // line that cannot change the answer.
            if pattern.exclusion != excluded {
                continue;
            }

            let hit = pattern.matches(path)
                || path
                    .match_indices('/')
                    .any(|(at, _)| pattern.matches(&path[..at]));

            if hit {
                excluded = !pattern.exclusion;
            }
        }

        excluded
    }

    /// Whether an excluded directory still has to be walked.
    ///
    /// `node_modules` with `!node_modules/.bin` below it: the directory is
    /// out, and something inside it is not. Skipping it whole would lose
    /// the exception, so a pattern that names a path *under* this directory
    /// buys the walk. The directory itself stays out of the tar either way.
    pub(crate) fn may_hold_an_exception(&self, dir: &str) -> bool {
        if !self.exceptions {
            return false;
        }

        let prefix = format!("{dir}/");

        self.patterns
            .iter()
            .filter(|pattern| pattern.exclusion)
            .any(|pattern| format!("{}/", pattern.text).starts_with(&prefix))
    }

    /// Puts `path` back, if the patterns so far take it out.
    ///
    /// **Only when it has to.** An exception makes every excluded directory
    /// worth walking into ([`Ignore::may_hold_an_exception`]), so adding one
    /// that changes nothing would turn `node_modules` from a directory
    /// skipped whole into one walked in full.
    fn keep(&mut self, path: &str) {
        if !self.excludes(path) {
            return;
        }

        let text = clean(path);

        self.patterns.push(Pattern {
            exclusion: true,
            tokens: tokenise(&text),
            text,
        });
        self.exceptions = true;
    }
}

/// Go's `filepath.Clean`, for the `/`-separated paths a `.dockerignore`
/// holds.
///
/// Its own rather than `Path::components`: that resolves `..` by dropping
/// the component before it *and* normalises away a leading `./` in ways
/// that differ from Go's, and a pattern is text to be matched rather than a
/// path to be walked. Wildcards pass through untouched — `**` is not a
/// directory named `**`.
fn clean(path: &str) -> String {
    let rooted = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();

    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => match parts.last() {
                Some(&last) if last != ".." => {
                    parts.pop();
                }
                None if rooted => {}
                _ => parts.push(".."),
            },
            other => parts.push(other),
        }
    }

    let cleaned = parts.join("/");

    if rooted {
        format!("/{cleaned}")
    } else if cleaned.is_empty() {
        ".".to_string()
    } else {
        cleaned
    }
}

fn tokenise(pattern: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut text = String::new();
    let mut chars = pattern.chars().peekable();

    while let Some(ch) = chars.next() {
        match ch {
            '*' => {
                flush(&mut tokens, &mut text);

                if chars.peek() != Some(&'*') {
                    tokens.push(Token::Run);
                    continue;
                }

                chars.next();

                // The separator after `**` is part of what it stands for,
                // which is what lets `**/foo` find a `foo` at the top.
                if chars.peek() == Some(&'/') {
                    chars.next();
                }

                tokens.push(match chars.peek() {
                    Some(_) => Token::AnyDirs,
                    None => Token::Rest,
                });
            }
            '?' => {
                flush(&mut tokens, &mut text);
                tokens.push(Token::OneChar);
            }
            // The next character stands for itself. A trailing backslash
            // stands for itself, having nothing to escape.
            '\\' => text.push(chars.next().unwrap_or('\\')),
            other => text.push(other),
        }
    }

    flush(&mut tokens, &mut text);
    tokens
}

fn flush(tokens: &mut Vec<Token>, text: &mut String) {
    if !text.is_empty() {
        tokens.push(Token::Text(std::mem::take(text)));
    }
}

/// Whether `path` is the whole of what `tokens` describe.
fn matches(tokens: &[Token], path: &str) -> bool {
    let Some((first, rest)) = tokens.split_first() else {
        return path.is_empty();
    };

    match first {
        Token::Text(text) => path
            .strip_prefix(text.as_str())
            .is_some_and(|tail| matches(rest, tail)),

        Token::OneChar => {
            let mut chars = path.chars();
            match chars.next() {
                Some(ch) if ch != '/' => matches(rest, chars.as_str()),
                _ => false,
            }
        }

        Token::Run => {
            let mut tail = path;
            loop {
                if matches(rest, tail) {
                    return true;
                }

                let mut chars = tail.chars();
                match chars.next() {
                    Some(ch) if ch != '/' => tail = chars.as_str(),
                    _ => return false,
                }
            }
        }

        Token::Rest => {
            let mut tail = path;
            loop {
                if matches(rest, tail) {
                    return true;
                }

                let mut chars = tail.chars();
                if chars.next().is_none() {
                    return false;
                }
                tail = chars.as_str();
            }
        }

        // Nothing at all, or anything ending in a separator.
        Token::AnyDirs => {
            matches(rest, path)
                || path
                    .match_indices('/')
                    .any(|(at, _)| matches(rest, &path[at + 1..]))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ignore(contents: &str) -> Ignore {
        Ignore::parse(contents)
    }

    /// Whether one pattern on its own leaves `path` out.
    fn leaves_out(pattern: &str, path: &str) -> bool {
        ignore(pattern).excludes(path)
    }

    #[test]
    fn no_patterns_leave_nothing_out() {
        assert!(!ignore("").excludes("anything"));
    }

    #[test]
    fn a_name_matches_that_name() {
        assert!(leaves_out("node_modules", "node_modules"));
        assert!(!leaves_out("node_modules", "node_modules_other"));
        assert!(!leaves_out("node_modules", "src"));
    }

    #[test]
    fn a_pattern_is_anchored_to_the_root_of_the_context() {
        // **The difference from `.gitignore`, and the one that catches
        // people out.** git would leave out every `node_modules` at any
        // depth; Docker leaves out the one at the top and no other.
        assert!(leaves_out("node_modules", "node_modules"));
        assert!(!leaves_out("node_modules", "packages/web/node_modules"));

        assert!(leaves_out("**/node_modules", "packages/web/node_modules"));
        assert!(leaves_out("**/node_modules", "node_modules"));
    }

    #[test]
    fn what_is_under_a_left_out_directory_is_left_out_too() {
        // Otherwise `node_modules` would mean an empty directory entry
        // rather than the tree everybody means by it.
        assert!(leaves_out("node_modules", "node_modules/react/index.js"));
        assert!(leaves_out("build", "build/static/js/main.js"));
    }

    #[test]
    fn a_star_stops_at_a_separator() {
        assert!(leaves_out("*.log", "debug.log"));
        assert!(!leaves_out("*.log", "logs/debug.log"));
        assert!(leaves_out("logs/*.log", "logs/debug.log"));
        assert!(!leaves_out("logs/*.log", "logs/nested/debug.log"));
    }

    #[test]
    fn a_question_mark_is_one_character_and_not_a_separator() {
        assert!(leaves_out("?.log", "a.log"));
        assert!(!leaves_out("?.log", "ab.log"));
        assert!(!leaves_out("a?b", "a/b"));
    }

    #[test]
    fn a_double_star_crosses_separators() {
        assert!(leaves_out("**/*.log", "a/b/c/debug.log"));
        assert!(leaves_out("**/*.log", "debug.log"));
        assert!(leaves_out("docs/**", "docs/a/b/c.md"));
        assert!(!leaves_out("docs/**", "docs"));
        assert!(leaves_out("a/**/b", "a/b"));
        assert!(leaves_out("a/**/b", "a/x/y/b"));
    }

    #[test]
    fn a_bang_puts_back_what_an_earlier_line_took_out() {
        let patterns = ignore("*.md\n!README.md\n");

        assert!(patterns.excludes("CHANGES.md"));
        assert!(!patterns.excludes("README.md"));
    }

    #[test]
    fn the_last_line_to_match_decides() {
        // Order is the whole mechanism: a `!` before the line that takes
        // the file out does nothing.
        assert!(ignore("!README.md\n*.md\n").excludes("README.md"));
        assert!(!ignore("*.md\n!README.md\n").excludes("README.md"));
    }

    #[test]
    fn everything_out_and_a_few_things_back_works() {
        // The shape a lot of repositories use, and the one that would
        // break a build if the Dockerfile were not put back for it.
        let patterns = ignore("*\n!src\n!package.json\n");

        assert!(patterns.excludes("node_modules"));
        assert!(patterns.excludes("README.md"));
        assert!(!patterns.excludes("src"));
        assert!(!patterns.excludes("package.json"));
    }

    #[test]
    fn a_comment_needs_the_first_column() {
        assert!(!leaves_out("# node_modules", "node_modules"));

        // Trimmed to `# notes`, which is a pattern for a file of that name
        // rather than a comment. Docker reads it the same way.
        assert!(leaves_out("  # notes", "# notes"));
    }

    #[test]
    fn blank_lines_say_nothing() {
        let patterns = ignore("\n   \n*.log\n\n");

        assert!(patterns.excludes("a.log"));
        assert!(!patterns.excludes("a.txt"));
    }

    #[test]
    fn a_leading_separator_is_decoration() {
        // A pattern is relative to the context whether or not it is
        // written as though it were absolute.
        assert!(leaves_out("/node_modules", "node_modules"));
        assert!(leaves_out("./node_modules", "node_modules"));
        assert!(leaves_out("node_modules/", "node_modules"));
    }

    #[test]
    fn a_backslash_takes_the_next_character_literally() {
        assert!(leaves_out(r"a\*b", "a*b"));
        assert!(!leaves_out(r"a\*b", "axb"));
    }

    #[test]
    fn a_left_out_directory_is_walked_only_for_an_exception_inside_it() {
        let plain = ignore("node_modules\n");
        assert!(!plain.may_hold_an_exception("node_modules"));

        // An exception somewhere else is no reason to walk this one.
        let elsewhere = ignore("node_modules\n*.md\n!README.md\n");
        assert!(!elsewhere.may_hold_an_exception("node_modules"));

        let inside = ignore("node_modules\n!node_modules/.bin\n");
        assert!(inside.may_hold_an_exception("node_modules"));
        assert!(!inside.excludes("node_modules/.bin"));
    }

    #[test]
    fn the_dockerfile_and_the_ignore_file_come_back() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(FILE), "*\n").expect("writes");

        let patterns =
            Ignore::for_context(dir.path(), Some("docker/web.Dockerfile")).expect("reads");

        assert!(!patterns.excludes(FILE));
        assert!(!patterns.excludes("docker/web.Dockerfile"));
        assert!(patterns.excludes("src/main.rs"));
    }

    #[test]
    fn putting_a_file_back_that_was_never_out_adds_no_exception() {
        // An exception is not free: it makes every left-out directory
        // worth walking into, so one that changes nothing turns a
        // `node_modules` skipped whole into one walked in full.
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(FILE), "node_modules\n").expect("writes");

        let patterns = Ignore::for_context(dir.path(), Some("Dockerfile")).expect("reads");

        assert!(!patterns.may_hold_an_exception("node_modules"));
    }

    #[test]
    fn an_absent_ignore_file_leaves_everything_in() {
        let dir = tempfile::tempdir().expect("tempdir");

        let patterns = Ignore::for_context(dir.path(), Some("Dockerfile")).expect("reads");

        assert!(!patterns.excludes("node_modules"));
    }

    #[test]
    fn cleaning_a_pattern_follows_go() {
        assert_eq!(clean("a/./b"), "a/b");
        assert_eq!(clean("a//b"), "a/b");
        assert_eq!(clean("a/b/"), "a/b");
        assert_eq!(clean("a/b/../c"), "a/c");
        assert_eq!(clean("./a"), "a");
        assert_eq!(clean(""), ".");
        assert_eq!(clean("/a/b"), "/a/b");
        assert_eq!(clean("**/a"), "**/a");
    }
}
