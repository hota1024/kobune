#!/bin/sh
#
# Installs Kobune.
#
#   curl -fsSL https://kobune.1024.works/install.sh | sh
#
# POSIX sh on purpose. The reader's shell does not matter — this is piped
# into `sh` — but the shell they *use* does, so completions and the PATH
# advice at the end are written for whichever shells are actually installed.
#
# Environment:
#   KOBUNE_INSTALL_DIR   where the binaries go (default ~/.local/bin)
#   KOBUNE_CHANNEL       the release tag to install (default nightly)
#   KOBUNE_NO_COMPLETIONS  set to skip writing completion scripts

set -eu

REPO="${KOBUNE_REPO:-hota1024/kobune}"
CHANNEL="${KOBUNE_CHANNEL:-nightly}"
INSTALL_DIR="${KOBUNE_INSTALL_DIR:-$HOME/.local/bin}"

# Whether there is a line to rewrite in place.
#
# The spinner and the bar work by drawing over what they drew a moment
# ago, which needs a terminal. A pipe, a CI log or `TERM=dumb` gets one
# line per step, printed before the step rather than after it, so that a
# log that stops shows what it stopped on.
if [ -t 1 ] && [ "${TERM:-}" != "dumb" ]; then
    live=1
else
    live=""
fi

# The step being drawn, empty between steps. Everything that writes to the
# screen consults it, because a message printed while a line is being held
# would land in the middle of that line.
step_label=""
spin=0

say() {
    printf '%s\n' "$*"
}

# Diagnostics go to stderr so `… | sh` still shows them when stdout is
# redirected somewhere.
die() {
    step_drop
    printf 'error: %s\n' "$*" >&2
    exit 1
}

# A warning, without losing the step it interrupted.
warn() {
    step_drop_line
    printf 'warning: %s\n' "$*" >&2
    step_redraw
}

need() {
    command -v "$1" >/dev/null 2>&1
}

# ---------------------------------------------------------------------------
# Steps
#
# What the install is doing, one step at a time: the current one is held on
# the bottom line with a spinner, and finished ones are left above it with a
# tick. This is what `kobune` itself shows while the daemon works, in the one
# form a POSIX shell can manage.
# ---------------------------------------------------------------------------

# The bar is this many cells wide, whatever the window.
BAR_WIDTH=20

step() {
    step_label="$1"

    if [ -n "$live" ]; then
        step_draw ""
    else
        # Braced: bash 3.2 — which is what `/bin/sh` is on macOS — reads
        # the bytes of the ellipsis as part of the name without them.
        say "${step_label}…"
    fi
}

# Redraws the held line: the spinner, the label, and whatever the step
# wants after it.
step_draw() {
    spin=$(((spin + 1) % 10))
    printf '\r\033[2K  %s %s%s' "$(spinner)" "$step_label" "$1"
}

step_redraw() {
    if [ -n "$live" ] && [ -n "$step_label" ]; then
        step_draw ""
    fi
}

# The same frames the CLI spins, so the two look like one program.
#
# A `case` rather than an index into a string: these are multi-byte, and
# `cut -c` counts bytes in the C locale.
spinner() {
    case "$spin" in
        0) printf '⠋' ;;
        1) printf '⠙' ;;
        2) printf '⠹' ;;
        3) printf '⠸' ;;
        4) printf '⠼' ;;
        5) printf '⠴' ;;
        6) printf '⠦' ;;
        7) printf '⠧' ;;
        8) printf '⠇' ;;
        *) printf '⠏' ;;
    esac
}

# Ends the step, leaving it on screen with a tick and, optionally, what it
# came to: a size, a directory, the shells that got completions.
step_done() {
    if [ -n "$live" ]; then
        printf '\r\033[2K  ✓ %s%s\n' "$step_label" "${1:+  $1}"
    elif [ -n "${1:-}" ]; then
        say "  $1"
    fi

    step_label=""
}

# Ends a step that turned out to have nothing to do, leaving nothing
# behind: a tick against work that did not happen reads as a lie.
step_drop() {
    step_drop_line
    step_label=""
}

# Clears the held line without forgetting the step, for the things that
# have something to say in the middle of one.
step_drop_line() {
    if [ -n "$live" ] && [ -n "$step_label" ]; then
        printf '\r\033[2K'
    fi
}

# A size to read at a glance, in whole-integer arithmetic.
#
# `awk` would do this in a line; this way the script asks for nothing a
# busybox does not have. The tenth of a megabyte is divided out rather
# than multiplied into, which keeps every value well inside the 32-bit
# signed range a shell is only promised to have.
#
# 104857 and not 104858: a tenth of a mebibyte is 104857.6, and rounding
# it *up* puts every exact megabyte one whole tenth low — `1048576` came
# out as `1.9 MB`, because 1048576/104858 is 9 rather than 10. Rounding
# down is wrong by less than a byte per tenth, which no size here can
# accumulate into a visible digit.
human() {
    if [ "$1" -ge 1048576 ]; then
        printf '%d.%d MB' "$(($1 / 1048576))" "$(($1 / 104857 % 10))"
    else
        printf '%d kB' "$(($1 / 1024))"
    fi
}

# The bar, the percentage, and how much of the file has arrived.
step_bar() {
    # $1 bytes so far, $2 bytes in total
    #
    # Kilobytes throughout, for the same 32-bit reason: bytes × 100 leaves
    # the promised range behind at about 21 MB, which a release archive
    # passes.
    total_kb=$(($2 / 1024))
    if [ "$total_kb" -lt 1 ]; then
        total_kb=1
    fi

    percent=$(($1 / 1024 * 100 / total_kb))
    if [ "$percent" -gt 100 ]; then
        # Content-Length is what a server says, not what it sends.
        percent=100
    fi

    filled=$((percent * BAR_WIDTH / 100))
    bar=""
    cell=0
    while [ "$cell" -lt "$BAR_WIDTH" ]; do
        # Braced for the same reason as the ellipsis above: bash 3.2 reads
        # the bytes of the block as part of the name otherwise.
        if [ "$cell" -lt "$filled" ]; then
            bar="${bar}█"
        else
            bar="${bar}░"
        fi
        cell=$((cell + 1))
    done

    step_draw "$(printf '  %s %3d%%  %s/%s' "$bar" "$percent" "$(human "$1")" "$(human "$2")")"
}

# The target triple, which is also how the archives are named.
detect_target() {
    os="$(uname -s)"
    arch="$(uname -m)"

    case "$os" in
        Darwin)
            case "$arch" in
                arm64 | aarch64) printf 'aarch64-apple-darwin' ;;
                x86_64) printf 'x86_64-apple-darwin' ;;
                *) die "unsupported architecture: $arch" ;;
            esac
            ;;
        Linux)
            case "$arch" in
                x86_64) printf 'x86_64-unknown-linux-gnu' ;;
                aarch64 | arm64)
                    die "there is no Linux arm64 build yet. Build from source: https://kobune.1024.works/guide/installation"
                    ;;
                *) die "unsupported architecture: $arch" ;;
            esac
            ;;
        *)
            die "unsupported operating system: $os. Kobune needs macOS or Linux"
            ;;
    esac
}

download() {
    # $1 url, $2 destination
    if need curl; then
        curl -fsSL "$1" -o "$2"
    elif need wget; then
        wget -qO "$2" "$1"
    else
        die "neither curl nor wget is installed"
    fi
}

# Downloads the archive with a bar in front of it.
#
# The transfer runs in the background and the file it is writing is
# measured as it grows. curl and wget both have progress meters of their
# own, but they look nothing like each other and nothing like the rest of
# this — and which of the two is installed is not something to show a
# person.
#
# A server that will not say how big the file is gets the amount so far
# and no bar: a bar has to know where the end is, and one that guesses is
# worse than none. With no terminal at all there is nothing to watch, and
# the download is simply run.
download_watched() {
    # $1 url, $2 destination
    if [ -z "$live" ]; then
        download "$1" "$2"
        return
    fi

    total="$(content_length "$1")"
    if [ -n "$total" ] && [ "$total" -lt 1 ]; then
        total=""
    fi

    download "$1" "$2" &
    download_pid=$!

    while kill -0 "$download_pid" 2>/dev/null; do
        got=0
        if [ -f "$2" ]; then
            got="$(wc -c <"$2" | tr -d ' ')"
        fi

        if [ -n "$total" ]; then
            step_bar "$got" "$total"
        else
            step_draw "  $(human "$got")"
        fi

        sleep "$tick"
    done

    if wait "$download_pid"; then
        download_pid=""

        # Whatever the last poll caught, the file is now whole. Leaving the
        # bar at 98% would say the opposite.
        if [ -n "$total" ]; then
            step_bar "$total" "$total"
        fi
    else
        download_pid=""
        return 1
    fi
}

# How big the download is, according to the server. Empty when it will not
# say, or says something that is not a number.
#
# `-L` because a release download is a redirect to a CDN, and it is the
# CDN that knows the length.
#
# The status is checked before the headers are read, and that is the whole
# point of the function: a server that refuses HEAD answers with an error
# page, which has a Content-Length of its own. Believing it draws a bar
# that is full from the second frame.
content_length() {
    if need curl; then
        headers="$(curl -fsSLI "$1" 2>/dev/null)" || return 0
    elif need wget; then
        headers="$(wget -qS --spider "$1" 2>&1)" || return 0
    else
        return 0
    fi

    length="$(
        printf '%s\n' "$headers" |
            tr -d '\r' |
            grep -i '^ *content-length:' |
            tail -n 1 |
            sed 's/.*: *//'
    )"

    case "$length" in
        '' | *[!0-9]*) return 0 ;;
        *) printf '%s' "$length" ;;
    esac
}

# The checksum is published alongside the archive, so a truncated download
# fails here instead of turning into a confusing "cannot execute".
#
# **Nothing here is allowed to pass by not answering.** A check that gives
# up and carries on is not a check, and the only trace it leaves is a
# warning in the scrollback of a `curl … | sh` nobody is reading. So every
# way out of this function either compares two digests or stops the
# install.
verify() {
    # $1 archive, $2 checksum file
    expected="$(cut -d' ' -f1 <"$2")"

    # A download that produced an error page rather than a checksum would
    # otherwise report a mismatch, which sends someone looking at the
    # archive when the problem is the other file.
    case "$expected" in
        '' | *[!0-9a-fA-F]*)
            die "$(basename "$2") is not a sha256 checksum. The download may have been intercepted, or the release may be broken"
            ;;
    esac

    if [ "${#expected}" -ne 64 ]; then
        die "$(basename "$2") holds a ${#expected}-character digest, not a sha256. The download may have been intercepted, or the release may be broken"
    fi

    # Three, because failing closed is only reasonable if it does not lock
    # anyone out: coreutils has the first, macOS ships the second, and
    # openssl covers what is left.
    if need sha256sum; then
        actual="$(sha256sum "$1" | cut -d' ' -f1)"
    elif need shasum; then
        actual="$(shasum -a 256 "$1" | cut -d' ' -f1)"
    elif need openssl; then
        actual="$(openssl dgst -sha256 "$1" | sed 's/.*= *//')"
    else
        die "no sha256 tool found (sha256sum, shasum or openssl), so the download cannot be verified. Install one, or download and check the release by hand: https://github.com/$REPO/releases"
    fi

    # Lowercased on both sides: sha256sum and shasum agree today, but a
    # mismatch here would look like a corrupt download.
    expected="$(printf '%s' "$expected" | tr 'A-F' 'a-f')"
    actual="$(printf '%s' "$actual" | tr 'A-F' 'a-f')"

    if [ "$expected" != "$actual" ]; then
        die "checksum mismatch: expected $expected, got $actual"
    fi
}

# Writes a completion script where the shell loads it without being told.
#
# zsh is the exception: it has no user directory that is in `fpath` by
# default, so the file lands in the conventional place and the caller is
# told the one line to add.
install_completions() {
    # $1 the installed kobune binary
    [ -z "${KOBUNE_NO_COMPLETIONS:-}" ] || return 0

    data="${XDG_DATA_HOME:-$HOME/.local/share}"
    config="${XDG_CONFIG_HOME:-$HOME/.config}"

    # `|| true` on every call, and it is not decoration. `set -e` aborts
    # on a failing command in an `if` *body*, and `write_completion`
    # failing is an expected path — it is how a build too old to have
    # `kobune completions` is handled. Without this the script dies here,
    # before the summary, the paths and the PATH advice, and says nothing
    # about why.
    if need bash; then
        write_completion "$1" bash "$data/bash-completion/completions" kobune || true
    fi

    # Only advertise the fpath line when a file was actually written.
    if need zsh && write_completion "$1" zsh "$data/zsh/site-functions" _kobune; then
        zsh_fpath="$data/zsh/site-functions"
    fi

    if need fish; then
        write_completion "$1" fish "$config/fish/completions" kobune.fish || true
    fi

    # And not the status of the last `if` either. Completions are a
    # nicety; they cannot be what decides whether an install finished.
    return 0
}

# Writes one script, and says whether it managed to.
#
# A build without `kobune completions` — anything older than this script —
# leaves nothing behind rather than an empty file the shell would source.
write_completion() {
    # $1 binary, $2 shell, $3 directory, $4 filename
    mkdir -p "$3" 2>/dev/null || return 1

    if "$1" completions "$2" >"$3/$4" 2>/dev/null && [ -s "$3/$4" ]; then
        completions_written="${completions_written:+$completions_written }$2"
        return 0
    fi

    rm -f "$3/$4"
    return 1
}

# The shell the person is actually using.
#
# `$SHELL` is only the login shell, and it is wrong often enough to matter:
# someone whose login shell is zsh but who works in fish would be handed
# `export PATH`, which fish does not understand. So the process tree is
# asked first — under `curl … | sh` the parent of this script *is* the
# interactive shell — and `$SHELL` is the fallback.
#
# Prints nothing and fails when it cannot tell.
detect_shell() {
    pid=""
    need ps && pid="${PPID:-}"
    depth=0

    # Up a few levels, because the immediate parent can be another `sh`
    # (`sh -c "curl … | sh"`) or the pipeline's `curl`.
    while [ -n "$pid" ] && [ "$pid" -gt 1 ] && [ "$depth" -lt 5 ]; do
        # `-fish` for a login shell, and an absolute path on some systems.
        name="$(ps -o comm= -p "$pid" 2>/dev/null | sed 's|^-||; s|.*/||')"

        if known_shell "$name"; then
            printf '%s' "$name"
            return 0
        fi

        pid="$(ps -o ppid= -p "$pid" 2>/dev/null | tr -d ' ')"
        depth=$((depth + 1))
    done

    name="$(basename "${SHELL:-}" 2>/dev/null || true)"
    if known_shell "$name"; then
        printf '%s' "$name"
        return 0
    fi

    return 1
}

known_shell() {
    case "$1" in
        fish | zsh | bash | ksh | ksh93 | mksh | dash | ash | tcsh | csh | nu | elvish | pwsh | powershell)
            return 0
            ;;
        *) return 1 ;;
    esac
}

# Where a shell reads its startup configuration.
#
# bash differs by platform: macOS opens login shells in Terminal, which read
# `.bash_profile` and never `.bashrc`, and on Linux it is the other way
# round. Writing to the wrong one is a line that appears to do nothing.
shell_config() {
    case "$1" in
        zsh) printf '~/.zshrc' ;;
        bash)
            if [ "$(uname -s)" = "Darwin" ]; then
                printf '~/.bash_profile'
            else
                printf '~/.bashrc'
            fi
            ;;
        ksh | ksh93 | mksh | dash | ash) printf '~/.profile' ;;
        tcsh) printf '~/.tcshrc' ;;
        csh) printf '~/.cshrc' ;;
        elvish) printf '~/.config/elvish/rc.elv' ;;
        *) printf '' ;;
    esac
}

# How to put a directory on PATH, in one shell's own syntax.
#
# Two lines where it can be: the one that makes it stick, and the one that
# makes it take effect in the shell that is already open. `.` rather than
# `source` for the POSIX family, since dash does not have `source`.
#
# Indented two spaces, ready to print.
path_advice() {
    # $1 shell, $2 directory
    config="$(shell_config "$1")"

    case "$1" in
        fish)
            # Persists by itself, so there is no file to edit and no
            # duplicate entry on the next login.
            say "  fish_add_path $2"
            ;;
        zsh | bash | ksh | ksh93 | mksh | dash | ash)
            say "  echo 'export PATH=\"$2:\$PATH\"' >> $config"
            say "  . $config"
            ;;
        tcsh | csh)
            say "  echo 'setenv PATH $2:\$PATH' >> $config"
            say "  source $config"
            ;;
        nu)
            # Nushell has no append-and-forget line: its configuration is a
            # script, and `$nu.config-path` is where it lives.
            say "  Add this to your config (\`config nu\` opens it):"
            say ""
            say "    \$env.PATH = (\$env.PATH | prepend '$2')"
            ;;
        elvish)
            say "  Add this to $config:"
            say ""
            say "    set paths = ['$2' \$@paths]"
            ;;
        pwsh | powershell)
            say "  Add this to your profile (\`\$PROFILE\`):"
            say ""
            say "    \$env:PATH = '$2' + [IO.Path]::PathSeparator + \$env:PATH"
            ;;
        *)
            say "  export PATH=\"$2:\$PATH\""
            ;;
    esac
}

target="$(detect_target)"
archive="kobune-$target.tar.gz"
base="https://github.com/$REPO/releases/download/$CHANNEL"

need tar || die "tar is not installed"

say "Kobune"
say "  release  $CHANNEL"
say "  target   $target"
say "  into     $INSTALL_DIR"
say ""

tmp="$(mktemp -d)"
download_pid=""

# Runs on failure too, so a mismatched checksum does not leave an archive
# behind for someone to find and trust later — and so that a Ctrl-C during
# the download takes the transfer with it rather than leaving it writing
# into a directory nobody will look at again.
cleanup() {
    if [ -n "$download_pid" ]; then
        kill "$download_pid" 2>/dev/null || true
    fi

    if [ -n "$live" ]; then
        printf '\033[?25h'
    fi

    rm -rf "$tmp"
}

trap cleanup EXIT
# Interrupted means stopped. Without this the handler would run and the
# script would carry on to unpack an archive that is half a file.
trap 'cleanup; exit 130' INT TERM

# How long to wait between two paints of the bar.
#
# POSIX only promises `sleep` whole seconds, and a bar that moves once a
# second is barely a bar — so a fraction is tried first and taken if it
# works.
if sleep 0.1 2>/dev/null; then
    tick=0.1
else
    tick=1
fi

# The cursor would otherwise sit blinking at the end of the bar, and jump
# about as the line is redrawn. Put back by `cleanup`, on every exit.
if [ -n "$live" ]; then
    printf '\033[?25l'
fi

step "downloading $archive"
download_watched "$base/$archive" "$tmp/$archive" ||
    die "cannot download $base/$archive"
download "$base/$archive.sha256" "$tmp/$archive.sha256" ||
    die "cannot download the checksum for $archive"
step_done "$(human "$(wc -c <"$tmp/$archive" | tr -d ' ')")"

step "verifying the checksum"
verify "$tmp/$archive" "$tmp/$archive.sha256"
step_done

step "unpacking the archive"
tar xzf "$tmp/$archive" -C "$tmp" || die "cannot unpack $archive"
step_done

# The archive nests the binaries under a directory named after the target,
# so that unpacking it by hand does not scatter files into the current one.
if [ -f "$tmp/kobune-$target/kobune" ]; then
    payload="$tmp/kobune-$target"
elif [ -f "$tmp/kobune" ]; then
    payload="$tmp"
else
    die "$archive does not contain kobune"
fi

for binary in kobune kobuned; do
    [ -f "$payload/$binary" ] || die "$archive does not contain $binary"
done

step "installing kobune and kobuned"

mkdir -p "$INSTALL_DIR" || die "cannot create $INSTALL_DIR"

# `kobune` finds the daemon next to itself, so the two move together or
# the CLI starts a version of kobuned it was not built against.
for binary in kobune kobuned; do
    chmod +x "$payload/$binary"
    # Replaced rather than written through: the running daemon's own
    # executable cannot be opened for writing, but it can be renamed over.
    mv -f "$payload/$binary" "$INSTALL_DIR/$binary" ||
        die "cannot write $INSTALL_DIR/$binary"
done

# Unsigned, so macOS refuses to run them until the download flag is gone.
if [ "$(uname -s)" = "Darwin" ] && need xattr; then
    xattr -d com.apple.quarantine "$INSTALL_DIR/kobune" 2>/dev/null || true
    xattr -d com.apple.quarantine "$INSTALL_DIR/kobuned" 2>/dev/null || true
fi

# `--version` checks for a newer build, which this has just installed and
# would only wait on the network to be told about. The check is turned off
# for the one call rather than left to time out mid-install.
version="$(KOBUNE_NO_UPDATE_CHECK=1 "$INSTALL_DIR/kobune" --version 2>/dev/null || true)"
[ -n "$version" ] || die "the installed binary does not run. Report this at https://github.com/$REPO/issues"

# No directory after it: the header said where this was going, and the
# summary below says it again with the two names.
step_done

# Quick enough that there is nothing to watch, and it can come to nothing
# at all — no shell to write for, or a build too old to have `kobune
# completions`. So it is announced afterwards, and only if it happened: a
# step named before the fact would sit there having claimed something.
completions_written=""
install_completions "$INSTALL_DIR/kobune"

if [ -n "$completions_written" ]; then
    step "writing completions"
    step_done "$completions_written"
fi

say ""
say "installed $version"
say "  $INSTALL_DIR/kobune"
say "  $INSTALL_DIR/kobuned"

# Only when it is not already reachable, and in the syntax of the shell the
# person is actually in. A wrong line here gets pasted into a config file
# and stays there for months.
shell="$(detect_shell || true)"

case ":$PATH:" in
    *":$INSTALL_DIR:"*) ;;
    *)
        say ""
        if [ -n "$shell" ]; then
            say "$INSTALL_DIR is not on your PATH. For $shell:"
            say ""
            path_advice "$shell" "$INSTALL_DIR"
        else
            # Rather than guessing: an `export` line shown to a fish user
            # fails, and a fish line shown to a bash user fails too.
            say "$INSTALL_DIR is not on your PATH. Add it, in your shell:"
            for candidate in fish zsh bash tcsh nu elvish pwsh; do
                say ""
                say "$candidate"
                path_advice "$candidate" "$INSTALL_DIR"
            done
        fi
        ;;
esac

# Written because zsh is installed, which is not the same as zsh being what
# anyone uses. Someone in fish does not need to hear about `fpath`.
if [ -n "${zsh_fpath:-}" ] && { [ "$shell" = "zsh" ] || [ -z "$shell" ]; }; then
    say ""
    say "For zsh completions, if they do not work yet:"
    say ""
    say "  echo 'fpath=($zsh_fpath \$fpath)' >> ~/.zshrc"
fi

say ""
say "Next:"
say ""
say "  kobune doctor         # what is missing"
say "  kobune setup          # the privileged one-off steps, asked one at a time"
say "  kobune init           # in a repository"
say ""
say "https://kobune.1024.works"
