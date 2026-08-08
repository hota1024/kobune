# What is Minato?

Minato gives every git worktree its own running environment, reachable at its
own URL.

```console
$ minato new feature/user-auth
  ✓ creating worktree feature/user-auth
  ✓ starting web
  ✓ waiting for web

myapp / feature-user-auth  (feature/user-auth)
  web   ready     https://web.feature-user-auth.myapp.localhost
```

That is the whole idea. One worktree, one environment. An environment appears
with its worktree and goes with it, and there is nothing else to keep track of.

## The problem it solves

Checking out a branch to review it means stopping what you were running,
switching, waiting for it to come back, and losing your place. Running several
branches at once means finding free ports, remembering which is which, and
tripping over shared state.

Worktrees already solve the source-code half of this. Minato solves the
running-it half:

- **Every worktree gets a URL** built from its branch name, so
  `feature/user-auth` becomes `web.feature-user-auth.myapp.localhost`. It does
  not change when things restart.
- **Environments that nobody is using stop themselves**, and start again on the
  next request in a second or two. Ten worktrees do not mean ten running stacks.
- **Nothing collides.** Separate containers, separate URLs, and where a
  database should be shared you say so once in configuration.

## Built for agents to drive

This is the part that shapes most of the design. An AI agent working on your
code needs to check that its change works, and the usual answer — reach for
`docker`, guess a port, curl it — goes wrong in ways that are hard to see.

So every command speaks `--json`, every failure exits with a code that says
what kind of failure it was, and `minato skill install` writes a Skill file
that tells an agent which commands to reach for and which to avoid. An agent
that follows it never touches `docker` directly, never guesses a port, and gets
a real error instead of an empty response when something is wrong.

None of that makes it worse to use by hand. The same commands, read by a
person, print the same information without the JSON.

## What it is not

- **Not a production deployment tool.** Everything here assumes a development
  machine and a person who owns it.
- **Not a container runtime.** Docker or Apple Container does that work;
  Minato arranges it.
- **Not a replacement for compose.** If one stack on one branch is all you
  need, compose is simpler and you should keep using it. Minato earns its keep
  when branches multiply.

## Where to go next

- [Installation](./installation) — build it, and pick a container runtime
- [Your first environment](./getting-started) — from nothing to a URL
- [How it works](./how-it-works) — the DNS, the proxy, and why there is a daemon
