# Your first environment

From an empty repository to a working URL. Ten minutes, most of it waiting for
an image to pull.

This assumes you have followed [Installation](./installation) and that
`minato doctor` is happy.

## 1. Describe the project

In the root of a git repository:

```console
$ minato init
╭ init ─────────────────────────────────────╮
│ created  /path/to/myapp/minato.toml       │
│ project  myapp                            │
│                                           │
│ › bring the environment up with minato up │
╰───────────────────────────────────────────╯
```

`minato init` writes a starter file and guesses the project name from the
directory. Open it and point it at something real:

```toml
[project]
name = "myapp"

[runtime]
default = "docker"

[services.web]
image = "node:22"
port = 3000
command = "npm run dev"
```

Three things matter here:

- **`port`** is the port your app listens on *inside* the container. Minato
  never asks you for a host port; there isn't one you need to know.
- **`command`** replaces the image's own command. Leave it out to use the
  image's default.
- Your worktree is mounted at **`/workspace`**, which is also the working
  directory. So `npm run dev` runs against the branch's code.

Commit it. `minato.toml` belongs in the repository — every worktree reads the
same one.

## 2. Start it

```console
$ minato up
  ✓ preparing the network
  ✓ pulling image node:22
  ✓ starting web
  ✓ waiting for web
╭ myapp / (main) ───────────────────────────╮
│ main  /path/to/myapp                      │
│                                           │
│ ● web  ready  https://web.myapp.localhost │
╰───────────────────────────────────────────╯
```

The main worktree leaves the workspace label out of its URL, so it is
`web.myapp.localhost` rather than `web.main.myapp.localhost`.

That last step — `waiting for web` — is Minato waiting for your app to actually
answer, not just for the container to exist. The two are not the same, and a
`curl` immediately after a container starts usually fails.

## 3. Reach it

```console
$ curl -sS --fail-with-body https://web.myapp.localhost
```

Or ask for the URL and use it:

```console
$ minato url web
https://web.myapp.localhost
```

With a service named, `minato url` prints one line and nothing else, so it
pipes and substitutes cleanly. This is the command to reach for instead of
writing a URL by hand. Leave the name out and it lists them all.

::: tip Certificate errors
`curl` exiting with code 60 means the local CA is not trusted yet. Run
`minato doctor` — it prints the command to fix it. This is the single most
common thing to hit first.
:::

## 4. Branch, and get a second environment

Here is the part that makes worktrees worth it:

```console
$ minato new feature/user-auth
  ✓ creating worktree feature/user-auth
  ✓ starting web
  ✓ waiting for web
╭ myapp / feature-user-auth ──────────────────────────────────╮
│ feature/user-auth  /path/to/myapp.wt/feature-user-auth      │
│                                                             │
│ ● web  ready  https://web.feature-user-auth.myapp.localhost │
╰─────────────────────────────────────────────────────────────╯
```

Both environments are now running, on separate URLs, from separate checkouts.
Nothing was stopped, and no port was chosen by anyone.

The worktree lands in `../myapp.wt/feature-user-auth` — beside the repository
rather than inside it, so editors and searches do not pick it up twice.

```console
$ minato ls
╭ workspaces ────────────────────────────────────╮
│ WORKSPACE          SERVICES  BRANCH            │
│ (main)             1/1       main              │
│ feature-user-auth  1/1       feature/user-auth │
╰────────────────────────────────────────────────╯
```

## 5. Work in it

```console
$ cd ../myapp.wt/feature-user-auth
```

From inside a worktree, commands act on that workspace by default:

```console
$ minato logs web -f          # follow this branch's logs
$ minato exec web -- npm test # run the tests inside its container
$ minato status               # what is running, and where
```

From anywhere else, name it with `-w`:

```console
$ minato logs -w feature-user-auth web
```

## 6. Leave it alone

Do nothing for a while and the environment stops itself. Come back and the
first request starts it again:

```console
$ curl -sS https://web.feature-user-auth.myapp.localhost
# a second or two, then the response
```

You never have to run `minato up` again for a branch you are still using. This
is what makes creating worktrees cheap: an idle one costs nothing.

## 7. Clean up

```console
$ minato rm -w feature-user-auth
```

Removes the worktree and its containers. The branch stays — this is not
`git branch -d`.

## Next

- [Configuration](./configuration) — several services, health checks, volumes
- [Everyday workflow](./workflow) — the commands you will actually use
- [A preview per branch](../tutorials/first-preview) — the same ground, worked
  through on a real app
