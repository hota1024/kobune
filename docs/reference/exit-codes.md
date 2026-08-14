# Exit codes

Every command exits with a code that says what kind of failure it was, so a
script or an agent can branch without reading any output.

| Code | Meaning | Retryable? |
| --- | --- | --- |
| `0` | Success | |
| `1` | An error with no more specific code | No |
| `4` | Not found — workspace, service or project | No |
| `5` | Already exists | No |
| `6` | No `kobune.toml` was found | No |
| `7` | `kobune.toml` is invalid | No |
| `8` | Not inside a git repository | No |
| `9` | The container runtime cannot be reached | **Yes** |
| `10` | A runtime operation failed | No |
| `11` | Unsupported | No |

"Retryable" means the same command may succeed later without anything being
changed. Only code 9 qualifies: start Docker or Apple Container and try again.

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
