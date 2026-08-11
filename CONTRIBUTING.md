# Contributing

## How work lands

`main` is the release branch. It takes pull requests only, and CI — Rust on
macOS and Linux, plus the desktop app — has to pass. Merging replaces the
`nightly` build.

A pull request that touches `docs/` gets its own preview URL, so prose can be
reviewed by reading the rendered page.

Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/).
The subject says what changed; the body says why, when the diff does not
already make it obvious.

## Signing off

Every commit needs a `Signed-off-by` line. `git` writes it for you:

```console
$ git commit -s -m "fix(proxy): keep the Host header intact"
```

```
fix(proxy): keep the Host header intact

Signed-off-by: Your Name <you@example.com>
```

The name and email have to match the commit's author. `git config user.name`
and `user.email` are where those come from, so setting them once is all it
takes.

To sign off a branch you have already written, rebase over it:

```console
$ git rebase --signoff main
```

**Forgot, and the branch is already in review?** Do not rebase. Push one more
commit whose message is exactly this, and the check will pass:

```
I, Your Name <you@example.com>, hereby add my Signed-off-by to this commit: <sha>
```

## What signing off means

The sign-off is a line-by-line assertion of the [Developer Certificate of
Origin](https://developercertificate.org/) 1.1, reproduced in full below. In
short: you are stating that you had the right to submit this work under the
project's licence.

It is not a copyright assignment, and it is not a contributor licence
agreement. You keep the copyright in what you write. Nothing is granted beyond
[Apache-2.0](LICENSE), which is what the contribution comes in under.

```
Developer Certificate of Origin
Version 1.1

Copyright (C) 2004, 2006 The Linux Foundation and its contributors.

Everyone is permitted to copy and distribute verbatim copies of this
license document, but changing it is not allowed.


Developer's Certificate of Origin 1.1

By making a contribution to this project, I certify that:

(a) The contribution was created in whole or in part by me and I
    have the right to submit it under the open source license
    indicated in the file; or

(b) The contribution is based upon previous work that, to the best
    of my knowledge, is covered under an appropriate open source
    license and I have the right under that license to submit that
    work with modifications, whether created in whole or in part
    by me, under the same license (unless I am permitted to submit
    under a different license), as indicated in the file; or

(c) The contribution was provided directly to me by some other
    person who certified (a), (b) or (c) and I have not modified
    it.

(d) I understand and agree that this project and the contribution
    are public and that a record of the contribution (including all
    personal information I submit with it, including my sign-off) is
    maintained indefinitely and may be redistributed consistent with
    this project or the open source license(s) involved.
```
