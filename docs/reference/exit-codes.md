# Exit codes

Every command exits with a code that says what kind of failure it was, so a
script or an agent can branch without reading any output.

| Code | Meaning | Retryable? |
| --- | --- | --- |
| `0` | Success | |
| `1` | An error with no more specific code | No |
| `2` | The command line is wrong | No |
| `4` | Not found — workspace, service or project | No |
| `5` | Already exists | No |
| `6` | No `kobune.toml` was found | No |
| `7` | `kobune.toml` is invalid | No |
| `8` | Not inside a git repository | No |
| `9` | The container runtime cannot be reached | **Yes** |
| `10` | A runtime operation failed | **Yes** |
| `11` | Unsupported | No |
| `70` | Kobune itself went wrong | No |
| `130` | Ctrl-C | No |

"Retryable" means the same command may succeed later without anything being
changed, and two codes qualify. `9` is nothing to talk to: start Docker or
Apple Container and try again. `10` is the operation itself failing, which a
pull over a network that has come back can pass on the second attempt — though
a Dockerfile that does not build fails the same way every time, so this one
deserves a limit rather than a loop.

Three sit outside the block the rest occupy. `2` is the usage code, and comes
from the argument parser rather than from anything Kobune tried to do — a
misspelled flag, or a command group named with nothing after it. `70` is
`EX_SOFTWARE` from `sysexits.h`, and says Kobune itself went wrong rather than
the machine or the configuration. `130` is Ctrl-C, which is 128 plus the signal
number and what a shell expects from an interrupted program; what a cancelled
command leaves behind is in [Interrupting](./cli#interrupting).

## `exec` is different

`kobune exec` returns **the command's own exit code**, not one of the above:

```console
$ kobune exec web -- npm test; echo $?
1
```

So test success is readable from exit status alone. A code above cannot be told
apart from the same code coming out of your command — check `--json` output if
you need to be sure which happened.

## In JSON

```console
$ kobune url nope --json; echo $?
{
  "error": {
    "code": "not_found",
    "message": "no service named `nope`. Available: web, api",
    "hint": "…"
  }
}
4
```

`error.code` is the machine-readable name; the exit code is its numeric
counterpart. `hint`, when present, says what to do next.

Errors print to **stdout** under `--json`, so a caller watches one stream and
one exit code.
