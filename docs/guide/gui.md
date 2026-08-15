# The desktop app

`kobune-desktop` is a small GPUI app that lives in the menu bar. It shows which
environments are running, their URLs, and their logs.

It is not meant to be kept open. It is for glancing at the state of things and
opening one.

## Running it

```console
$ cargo build --release -p kobune-desktop
$ ./target/release/kobune-desktop
```

## Building it

The GUI needs more than the CLI does, and the build is fussier.

**Xcode Command Line Tools are enough** — a full Xcode is not, because
`runtime_shaders` is enabled so Metal shaders compile at run time.

If bindgen cannot find system headers, something else is first on your `PATH`.
A WASI SDK is the usual culprit:

```console
$ export PATH=$(echo $PATH | tr ':' '\n' | grep -v wasi-sdk | paste -sd: -)
$ unset WASI_SDK_PATH
$ export LIBCLANG_PATH=/Library/Developer/CommandLineTools/usr/lib
$ cargo build -p kobune-desktop
```

The symptom is a missing CoreMedia or similar; the cause is a clang that does
not know about macOS frameworks.

## What it shows

- **A sidebar of workspaces**, with each service's state, updated continuously
- **A detail pane** with URLs to copy or open, and start and stop buttons
- **A log viewer** for the selected workspace
- **The menu bar icon**, whose menu links straight to any running service

It follows the system light and dark setting, and you can override that from
the title bar.

## What it does not do

**It never starts the daemon.** Looking after the daemon is launchd's job, and
having the GUI manage it too would split that responsibility. If the app says
it cannot connect, start the daemon from the CLI — or install the LaunchDaemon
so it is always there.

It reads the same daemon API the CLI does, so anything visible in one is
visible in the other. There is no state that only the GUI knows.
