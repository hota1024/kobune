//! What Kobune has to say to a shell, rather than to a person.
//!
//! Two things, and they are the two halves of `kobune cd`: the function
//! that lets a shell be moved at all, and the completion that offers
//! workspace names while one is being typed.
//!
//! **Both are written by hand, for one shell at a time.** A wrapper that
//! has never been run in the shell it claims to support is worse than no
//! wrapper, because it fails in the file where nothing else has failed
//! before — so what is here is the three shells that were tried, and
//! `kobune completions` still writes clap's script for the others.

use std::fmt;

use clap::ValueEnum;

/// A shell `kobune shell-init` can hand a function to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
}

impl fmt::Display for Shell {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
        };

        f.write_str(name)
    }
}

impl Shell {
    /// The one line that loads [`integration`] from a startup file.
    pub fn init_line(self) -> String {
        match self {
            Self::Fish => "kobune shell-init fish | source".to_string(),
            other => format!("eval \"$(kobune shell-init {other})\""),
        }
    }

    /// The shell this session looks to be running under.
    ///
    /// `$SHELL` is the login shell rather than the one at the prompt, and
    /// it is wrong often enough that it decides nothing here — it picks
    /// which of three lines a *hint* shows, and a hint that names the
    /// wrong shell costs a reread rather than a broken configuration
    /// file.
    pub fn current() -> Option<Self> {
        let shell = std::env::var("SHELL").ok()?;
        let name = shell.rsplit('/').next()?;

        match name {
            "bash" => Some(Self::Bash),
            "zsh" => Some(Self::Zsh),
            "fish" => Some(Self::Fish),
            _ => None,
        }
    }
}

/// The function that turns `kobune cd` into a directory change.
///
/// **A program cannot move the shell that ran it.** A directory change
/// belongs to a process and dies with it, so `kobune cd` prints a path
/// and this is what a shell does with one. Everything that is not `cd`
/// is passed through untouched, and so is `cd` itself when what came
/// back was not a directory — `--json` and `--help` answer in something
/// other than a path, and swallowing either would be a worse bug than
/// the one this fixes.
pub fn integration(shell: Shell) -> String {
    match shell {
        Shell::Bash | Shell::Zsh => POSIX_INTEGRATION.to_string(),
        Shell::Fish => FISH_INTEGRATION.to_string(),
    }
}

const POSIX_INTEGRATION: &str = r#"# Moves this shell into a workspace: `kobune cd feature/user-auth`.
#
# `kobune cd` prints the path rather than moving anything, because a
# directory change belongs to a shell and dies with the process that
# makes one. Everything that is not `cd` is passed straight through.
kobune() {
    if [ "$1" != "cd" ]; then
        command kobune "$@"
        return $?
    fi

    local target
    target=$(command kobune "$@") || return $?

    # A directory is a move. Anything else is an answer — `--json`,
    # `--help` — and printing it is what was asked for.
    if [ -d "$target" ]; then
        cd "$target" || return $?
    elif [ -n "$target" ]; then
        printf '%s\n' "$target"
    fi
}
"#;

const FISH_INTEGRATION: &str = r#"# Moves this shell into a workspace: `kobune cd feature/user-auth`.
#
# `kobune cd` prints the path rather than moving anything, because a
# directory change belongs to a shell and dies with the process that
# makes one. Everything that is not `cd` is passed straight through.
function kobune
    if test "$argv[1]" != cd
        command kobune $argv
        return $status
    end

    set --local target (command kobune $argv)
    or return $status

    # A directory is a move. Anything else is an answer — `--json`,
    # `--help` — and printing it is what was asked for.
    if test -d "$target"
        cd $target
    else if test -n "$target"
        printf '%s\n' $target
    end
end
"#;

/// Points the workspace arguments of a generated script at the daemon.
///
/// clap knows every flag and every subcommand and cannot know one
/// workspace label, because they are made and destroyed while the shell
/// that loaded the script is still running. So the script it generates is
/// rewritten on the way out: the arguments that take a workspace are
/// pointed at [`WORKSPACES`], which asks the daemon.
///
/// **Rewritten rather than replaced.** What clap writes is regenerated
/// with every release and grows a command at a time; a fork of it would
/// be stale by the next one. What is matched on here is narrow enough to
/// notice when it stops matching — [`tests`] fails the build rather than
/// letting a Tab press quietly stop offering anything.
pub fn wire_workspaces(shell: clap_complete::Shell, script: &str) -> String {
    match shell {
        clap_complete::Shell::Bash => format!("{script}{BASH_COMPLETION}"),
        clap_complete::Shell::Zsh => wire_zsh(script),
        clap_complete::Shell::Fish => wire_fish(script),
        // elvish and powershell get what clap writes and nothing else:
        // see this module's own note about shells nobody has tried.
        _ => script.to_string(),
    }
}

/// What the completion scripts run to list workspaces.
///
/// **stderr goes nowhere, in every shell.** The command answers a Tab
/// press with silence when it cannot answer at all, and this is the
/// second half of that: fish does not capture a command substitution's
/// stderr, so a line written there would land across the prompt somebody
/// is typing at.
const WORKSPACES: &str = "kobune complete workspaces 2>/dev/null";

/// zsh names a completion function per argument, so the wiring is a
/// substitution: every argument that takes a workspace is `_default` —
/// complete a file, in other words — until this points it somewhere
/// better.
fn wire_zsh(script: &str) -> String {
    let body = script
        .lines()
        .filter(|line| *line != "#compdef kobune")
        .map(wire_zsh_line)
        .collect::<Vec<_>>()
        .join("\n");

    // The function goes above what clap wrote rather than below it. The
    // file *is* the body of `_kobune` when zsh autoloads it, and the last
    // thing in that body calls it — so anything appended afterwards is
    // defined one Tab press too late.
    format!("#compdef kobune\n\n{ZSH_COMPLETION}\n{}\n", body.trim())
}

/// One line of clap's zsh script, pointed at [`WORKSPACES`] if it is an
/// argument that takes a workspace.
///
/// Two shapes carry one: an option, written `:WORKSPACE:_default` after
/// the value name, and `cd`'s positional, written `::workspace_name --
/// …:_default` after the id clap gave it and however many colons its
/// cardinality came to.
fn wire_zsh_line(line: &str) -> String {
    let (body, tail) = match line.strip_suffix(" \\") {
        Some(body) => (body, " \\"),
        None => (line, ""),
    };

    let Some(head) = body.strip_suffix(":_default'") else {
        return line.to_string();
    };

    let takes_a_workspace = head.ends_with(":WORKSPACE")
        || head
            .trim_start_matches(['\'', '*', ':'])
            .starts_with("workspace");

    if !takes_a_workspace {
        return line.to_string();
    }

    format!("{head}:_kobune_workspaces'{tail}")
}

/// fish names the candidates on the `complete` line itself, so the
/// wiring is that line gaining an `-a`, and one new line for `cd` — clap
/// writes nothing for a positional, which leaves it completing files.
fn wire_fish(script: &str) -> String {
    let body: Vec<String> = script
        .lines()
        .map(|line| {
            if line.starts_with("complete -c kobune ")
                && line.contains(" -l workspace ")
                && line.ends_with(" -r")
            {
                format!("{line} -f -a '({WORKSPACES})'")
            } else {
                line.to_string()
            }
        })
        .collect();

    format!("{}\n{}", body.join("\n"), FISH_COMPLETION)
}

const BASH_COMPLETION: &str = r#"
# Workspace names for `cd` and `--workspace`, which only the daemon knows.
#
# Wrapped around what clap generated rather than woven into it, and
# registered after it: the last `complete` for a command is the one bash
# uses.
_kobune_workspaces() {
    # The label is the first field; the branch after the tab is for the
    # shells that can show a description beside a candidate.
    kobune complete workspaces 2>/dev/null | cut -f1
}

_kobune_or_workspace() {
    local cur prev
    cur="${COMP_WORDS[COMP_CWORD]}"
    prev="${COMP_WORDS[COMP_CWORD-1]}"

    # Read off the word before rather than off a parse of the line: bash
    # has none to offer, and both places a workspace is named — `cd`'s
    # one argument and the value `-w` takes — are one word deep.
    case "$prev" in
        cd|-w|--workspace)
            if [[ "$cur" != -* ]]; then
                COMPREPLY=($(compgen -W "$(_kobune_workspaces)" -- "$cur"))
                return 0
            fi
            ;;
    esac

    _kobune "$@"
}

if [[ "${BASH_VERSINFO[0]}" -eq 4 && "${BASH_VERSINFO[1]}" -ge 4 || "${BASH_VERSINFO[0]}" -gt 4 ]]; then
    complete -F _kobune_or_workspace -o nosort -o bashdefault -o default kobune
else
    complete -F _kobune_or_workspace -o bashdefault -o default kobune
fi
"#;

const ZSH_COMPLETION: &str = r#"# Workspace names for `cd` and `--workspace`, which only the daemon
# knows. Every argument that takes one is pointed here.
#
# **Nothing here starts a daemon.** A Tab press is not a reason to, and a
# shell with none running completes to nothing rather than waiting for
# one to come up.
_kobune_workspaces() {
    local line
    local -a workspaces
    for line in ${(f)"$(kobune complete workspaces 2>/dev/null)"}; do
        [[ -n "$line" ]] && workspaces+=("${line//$'\t'/:}")
    done

    (( ${#workspaces} )) || return 1
    _describe -t workspaces workspace workspaces
}
"#;

const FISH_COMPLETION: &str = r#"
# The workspace `cd` moves to. clap writes nothing for a positional
# argument, which leaves it offering the files in the current directory —
# none of which is what `cd` takes.
complete -c kobune -n '__fish_kobune_using_subcommand cd' -f -a '(kobune complete workspaces 2>/dev/null)'
"#;

#[cfg(test)]
mod tests {
    use super::*;

    /// What `kobune completions <shell>` writes, wiring and all.
    fn script(shell: clap_complete::Shell) -> String {
        let mut command = <crate::Cli as clap::CommandFactory>::command();
        let name = command.get_name().to_string();

        let mut out = Vec::new();
        clap_complete::generate(shell, &mut command, name, &mut out);

        wire_workspaces(shell, &String::from_utf8(out).expect("utf-8"))
    }

    /// The one that would fail quietly: clap changing how it writes an
    /// action would leave the substitution matching nothing, and Tab
    /// would go back to offering the files in the current directory
    /// without anything else breaking.
    #[test]
    fn zsh_points_every_workspace_argument_at_the_daemon() {
        let script = script(clap_complete::Shell::Zsh);

        assert!(
            !script.contains(":WORKSPACE:_default"),
            "an option taking a workspace was left completing files"
        );
        let positional = script
            .lines()
            .find(|line| line.starts_with("'::workspace_name"))
            .expect("cd takes a workspace positionally");

        assert!(
            positional.contains(":_kobune_workspaces'"),
            "cd's argument was left completing files: {positional}"
        );
        assert!(script.contains(":WORKSPACE:_kobune_workspaces"));
        assert!(script.contains("_kobune_workspaces() {"));
    }

    /// The function has to be defined before the line that runs
    /// `_kobune`, which is the last thing in the file: zsh evaluates the
    /// whole of it as that function's body.
    #[test]
    fn zsh_defines_the_completer_before_it_is_reached() {
        let script = script(clap_complete::Shell::Zsh);

        let defined = script.find("_kobune_workspaces() {").expect("defined");
        let used = script.find(":_kobune_workspaces'").expect("used");

        assert!(defined < used, "the completer is defined too late");
        assert!(script.starts_with("#compdef kobune\n"), "{script:.40}");
        assert_eq!(script.matches("#compdef kobune").count(), 1);
    }

    #[test]
    fn fish_offers_workspaces_where_one_is_taken() {
        let script = script(clap_complete::Shell::Fish);

        assert!(
            script.contains("__fish_kobune_using_subcommand cd' -f -a '(kobune complete"),
            "cd's argument was left completing files"
        );
        assert!(
            script
                .lines()
                .filter(|line| line.contains(" -l workspace "))
                .all(|line| line.ends_with(&format!("-a '({WORKSPACES})'"))),
            "an option taking a workspace was left completing files"
        );
    }

    #[test]
    fn bash_completes_the_word_after_cd() {
        let script = script(clap_complete::Shell::Bash);

        assert!(script.contains("cd|-w|--workspace)"));
        assert!(script.contains("complete -F _kobune_or_workspace"));

        // clap's own registration has to stay above ours, which is the
        // whole of why appending works.
        let theirs = script.find("complete -F _kobune ").expect("clap registers");
        let ours = script
            .find("complete -F _kobune_or_workspace")
            .expect("ours");
        assert!(theirs < ours);
    }

    /// A Tab press answers with candidates or with nothing, and one
    /// script forgetting the redirect is how "nothing" becomes a
    /// sentence written across somebody's command line.
    ///
    /// Read off the snippets rather than the finished script: clap writes
    /// the words `kobune complete workspaces` itself, in the description
    /// of a subcommand nobody runs by hand, and that one is text rather
    /// than a command.
    #[test]
    fn no_script_lets_the_completer_speak_to_the_terminal() {
        for snippet in [BASH_COMPLETION, ZSH_COMPLETION, FISH_COMPLETION] {
            let asked = snippet.matches("kobune complete workspaces").count();
            let quiet = snippet.matches(WORKSPACES).count();

            assert!(asked > 0, "a snippet asks for nothing");
            assert_eq!(asked, quiet, "asked somewhere without 2>/dev/null");
        }
    }

    /// elvish and powershell are written for by clap and nobody else.
    #[test]
    fn an_untried_shell_is_left_alone() {
        let mut command = <crate::Cli as clap::CommandFactory>::command();
        let mut out = Vec::new();
        clap_complete::generate(
            clap_complete::Shell::Elvish,
            &mut command,
            "kobune",
            &mut out,
        );

        let generated = String::from_utf8(out).expect("utf-8");
        assert_eq!(
            wire_workspaces(clap_complete::Shell::Elvish, &generated),
            generated
        );
    }

    #[test]
    fn every_shell_is_told_how_to_load_its_function() {
        assert_eq!(Shell::Zsh.init_line(), r#"eval "$(kobune shell-init zsh)""#);
        assert_eq!(Shell::Fish.init_line(), "kobune shell-init fish | source");
    }

    #[test]
    fn the_function_passes_everything_that_is_not_cd_through() {
        for shell in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let function = integration(shell);
            assert!(function.contains("command kobune"), "{shell}");
            assert!(function.contains("cd"), "{shell}");
        }
    }
}
