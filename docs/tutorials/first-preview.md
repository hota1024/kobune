# A preview per branch

We will take a small Node app, give it a preview environment, and end with two
branches running side by side on their own URLs.

About fifteen minutes. You need Minato installed and `minato doctor` happy.

## The app

Any app that listens on a port will do. If you want one to hand:

```console
$ mkdir myapp && cd myapp && git init
$ npm init -y && npm pkg set scripts.dev="node server.js"
```

```js
// server.js
import { createServer } from 'node:http'

const banner = process.env.BANNER ?? 'hello'

createServer((_, res) => {
  res.writeHead(200, { 'content-type': 'text/plain' })
  res.end(`${banner} from ${process.env.MINATO_WORKSPACE ?? 'somewhere'}\n`)
}).listen(3000, '0.0.0.0')
```

```console
$ npm pkg set type=module
$ git add -A && git commit -m "a server"
```

::: warning Bind 0.0.0.0
`listen(3000)` alone binds `0.0.0.0`, which is what you want. A server bound to
`127.0.0.1` inside a container cannot be reached from outside it — a common
first mistake.
:::

## Describe it

```console
$ minato init
```

Edit `minato.toml`:

```toml
[project]
name = "myapp"

[runtime]
default = "docker"

[services.web]
image = "node:22"
port = 3000
command = "npm run dev"
health = "http://localhost:3000/"
```

`health` is optional but worth setting from the start: without it, readiness
means only that a TCP connection succeeded, which can be true before your app
can answer.

```console
$ git add minato.toml && git commit -m "minato"
```

## Start it

```console
$ minato up
  ✓ pulling image node:22
  ✓ starting web
  ✓ waiting for web
╭ myapp / (main) ───────────────────────────╮
│ main  /path/to/myapp                      │
│                                           │
│ ● web  ready  https://web.myapp.localhost │
╰───────────────────────────────────────────╯
```

```console
$ curl -sS --fail-with-body https://web.myapp.localhost
hello from main
```

`MINATO_WORKSPACE` was injected — the app knows which branch it is.

## Branch

```console
$ minato new feature/loud-banner
  ✓ creating worktree feature/loud-banner
  ✓ starting web
╭ myapp / feature-loud-banner ──────────────────────────────────╮
│ feature/loud-banner  /path/to/myapp.wt/feature-loud-banner    │
│                                                               │
│ ● web  ready  https://web.feature-loud-banner.myapp.localhost │
╰───────────────────────────────────────────────────────────────╯
```

Two environments now. Nothing was stopped and no port was chosen.

## Change something, on the branch only

```console
$ cd ../myapp.wt/feature-loud-banner
$ minato env set BANNER=HELLO
$ minato down && minato up
```

```console
$ curl -sS https://web.feature-loud-banner.myapp.localhost
HELLO from feature-loud-banner

$ curl -sS https://web.myapp.localhost
hello from main
```

The variable went to the *workspace* layer, so it applies to this worktree and
nothing else:

```console
$ minato env ls
╭ environment ─────────────╮
│ KEY     SCOPE      VALUE │
│ BANNER  workspace  HELLO │
╰──────────────────────────╯
```

`down && up` was needed because a running container does not pick up a new
value.

## Watch it stop and start again

```console
$ minato down
$ minato status
╭ myapp / feature-loud-banner ────────────────────────────────────╮
│ feature/loud-banner  /path/to/myapp.wt/feature-loud-banner      │
│                                                                 │
│ ○ web  stopped  https://web.feature-loud-banner.myapp.localhost │
╰─────────────────────────────────────────────────────────────────╯
```

The URL is still there. Stopped is not gone:

```console
$ time curl -sS https://web.feature-loud-banner.myapp.localhost
HELLO from feature-loud-banner
curl …  0.01s user … 2.104s total
```

Two seconds, and it is up. You never run `minato up` for a branch you are still
using — and an idle worktree costs nothing, which is what makes making them
cheap.

## Look inside

```console
$ minato logs web -n 20
$ minato exec web -- node --version
v22.14.0
$ minato exec web -- npm test; echo $?
```

The exit code is the command's, so `npm test` can drive a script.

## Clean up

```console
$ cd ../../myapp
$ minato rm -w feature-loud-banner
$ minato ls
╭ workspaces ─────────────────╮
│ WORKSPACE  SERVICES  BRANCH │
│ (main)     1/1       main   │
╰─────────────────────────────╯
```

The branch is still there; only the worktree and its containers are gone.

## Next

- [A web app and a database](./multi-service) — several services, and one
  shared between branches
- [Everyday workflow](../guide/workflow)
