#!/bin/sh
#
# Installs Minato.
#
#   curl -fsSL https://minato.1024.works/install.sh | sh
#
# POSIX sh on purpose. The reader's shell does not matter — this is piped
# into `sh` — but the shell they *use* does, so completions and the PATH
# advice at the end are written for whichever shells are actually installed.
#
# Environment:
#   MINATO_INSTALL_DIR   where the binaries go (default ~/.local/bin)
#   MINATO_CHANNEL       the release tag to install (default nightly)
#   MINATO_NO_COMPLETIONS  set to skip writing completion scripts

set -eu

REPO="${MINATO_REPO:-hota1024/minato}"
CHANNEL="${MINATO_CHANNEL:-nightly}"
INSTALL_DIR="${MINATO_INSTALL_DIR:-$HOME/.local/bin}"

say() {
    printf '%s\n' "$*"
}

# Diagnostics go to stderr so `… | sh` still shows them when stdout is
# redirected somewhere.
die() {
    printf 'error: %s\n' "$*" >&2
    exit 1
}

need() {
    command -v "$1" >/dev/null 2>&1
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
                    die "there is no Linux arm64 build yet. Build from source: https://minato.1024.works/guide/installation"
                    ;;
                *) die "unsupported architecture: $arch" ;;
            esac
            ;;
        *)
            die "unsupported operating system: $os. Minato needs macOS or Linux"
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

# The checksum is published alongside the archive, so a truncated download
# fails here instead of turning into a confusing "cannot execute".
verify() {
    # $1 archive, $2 checksum file
    expected="$(cut -d' ' -f1 <"$2")"

    if need sha256sum; then
        actual="$(sha256sum "$1" | cut -d' ' -f1)"
    elif need shasum; then
        actual="$(shasum -a 256 "$1" | cut -d' ' -f1)"
    else
        say "warning: no sha256 tool found, skipping verification" >&2
        return 0
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
    # $1 the installed minato binary
    [ -z "${MINATO_NO_COMPLETIONS:-}" ] || return 0

    data="${XDG_DATA_HOME:-$HOME/.local/share}"
    config="${XDG_CONFIG_HOME:-$HOME/.config}"

    if need bash; then
        write_completion "$1" bash "$data/bash-completion/completions" minato
    fi

    # Only advertise the fpath line when a file was actually written.
    if need zsh && write_completion "$1" zsh "$data/zsh/site-functions" _minato; then
        zsh_fpath="$data/zsh/site-functions"
    fi

    if need fish; then
        write_completion "$1" fish "$config/fish/completions" minato.fish
    fi
}

# Writes one script, and says whether it managed to.
#
# A build without `minato completions` — anything older than this script —
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
archive="minato-$target.tar.gz"
base="https://github.com/$REPO/releases/download/$CHANNEL"

need tar || die "tar is not installed"

say "Minato"
say "  release  $CHANNEL"
say "  target   $target"
say "  into     $INSTALL_DIR"
say ""

tmp="$(mktemp -d)"
# Runs on failure too, so a mismatched checksum does not leave an archive
# behind for someone to find and trust later.
trap 'rm -rf "$tmp"' EXIT INT TERM

say "downloading ${archive}…"
download "$base/$archive" "$tmp/$archive" ||
    die "cannot download $base/$archive"
download "$base/$archive.sha256" "$tmp/$archive.sha256" ||
    die "cannot download the checksum for $archive"

verify "$tmp/$archive" "$tmp/$archive.sha256"

tar xzf "$tmp/$archive" -C "$tmp" || die "cannot unpack $archive"

# The archive nests the binaries under a directory named after the target,
# so that unpacking it by hand does not scatter files into the current one.
if [ -f "$tmp/minato-$target/minato" ]; then
    payload="$tmp/minato-$target"
elif [ -f "$tmp/minato" ]; then
    payload="$tmp"
else
    die "$archive does not contain minato"
fi

for binary in minato minatod; do
    [ -f "$payload/$binary" ] || die "$archive does not contain $binary"
done

mkdir -p "$INSTALL_DIR" || die "cannot create $INSTALL_DIR"

# `minato` finds the daemon next to itself, so the two move together or
# the CLI starts a version of minatod it was not built against.
for binary in minato minatod; do
    chmod +x "$payload/$binary"
    # Replaced rather than written through: the running daemon's own
    # executable cannot be opened for writing, but it can be renamed over.
    mv -f "$payload/$binary" "$INSTALL_DIR/$binary" ||
        die "cannot write $INSTALL_DIR/$binary"
done

# Unsigned, so macOS refuses to run them until the download flag is gone.
if [ "$(uname -s)" = "Darwin" ] && need xattr; then
    xattr -d com.apple.quarantine "$INSTALL_DIR/minato" 2>/dev/null || true
    xattr -d com.apple.quarantine "$INSTALL_DIR/minatod" 2>/dev/null || true
fi

version="$("$INSTALL_DIR/minato" --version 2>/dev/null || true)"
[ -n "$version" ] || die "the installed binary does not run. Report this at https://github.com/$REPO/issues"

completions_written=""
install_completions "$INSTALL_DIR/minato"

say ""
say "installed $version"
say "  $INSTALL_DIR/minato"
say "  $INSTALL_DIR/minatod"

if [ -n "$completions_written" ]; then
    say "  completions: $completions_written"
fi

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
say "  minato doctor         # what is missing"
say "  minato setup          # the privileged one-off steps, printed not run"
say "  minato init           # in a repository"
say ""
say "https://minato.1024.works"
