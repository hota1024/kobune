---
name: prose
description: Write or revise prose in this repository — the docs site in English and Japanese, README.md, SECURITY.md, docs/DESIGN.md, docs/AGENT-RUN.md, skills/kobune/SKILL.md. Carries the house style for both languages, the English–Japanese glossary and a checker. Read it before editing any Markdown written for a reader rather than for a machine.
---

# Prose

The documentation has a voice, and it was arrived at rather than chosen. English
is second person, unabbreviated, and says what does not work as readily as what
does. Japanese is ですます, spaces half-width Latin away from the characters
either side of it, and is written rather than translated. Both wrap at 80
columns.

None of that was written down until this skill. It was inferred from the corpus,
and the numbers behind each rule are in the reference files, so a rule that no
longer matches the text can be recognised as out of date rather than obeyed out
of habit.

## What this covers

Everything written for a person to read:

- `docs/index.md`, `docs/guide/`, `docs/reference/`, `docs/tutorials/` — and
  `docs/ja/` beneath them, page for page
- `README.md`, `SECURITY.md`, `assets/README.md`
- `docs/README.md`, `docs/DESIGN.md`, `docs/AGENT-RUN.md`
- `skills/kobune/SKILL.md`, which ships to users and is read by agents
- `docs/.vitepress/theme/copy.ts` — everything the theme says, in both
  languages: the bar above the nav, on every page, and the home page demo's
  captions. The checker cannot read TypeScript, and the frontmatter it skips
  carries the rest of the home page, so both are held to this style by hand

Leave alone:

- `CHANGELOG.md` — its entries come from commit messages, and rewriting them
  puts the file at odds with the history it summarises
- `docs/v*/` — released snapshots, frozen at release. `versions.json` is empty
  today, so there are none yet
- `.agents/skills/` — vendored from elsewhere, tracked by `skills-lock.json`
- Anything inside a code block, and every line of console output

## The three that come before the others

They were already in `docs/README.md`, and they outrank everything in the
reference files.

**Say what a thing is for before saying how to use it.** A reader has usually
landed on the page from a search, not from the page before it.

**Show real output.** Every console block was produced by running the command.
If you cannot run it, leave the old block alone rather than composing a
plausible one.

**Say what does not work.** Firecracker is planned and not usable yet, and
nothing has been released, and a reader is better served knowing that than
discovering it. `build` and `cmd:` health checks were on this list until they
shipped — check before repeating it.

## Writing a page

Both languages move together. A page is not done when its English is done.

1. **Read both versions in full**, and the code behind any claim you are unsure
   of. `git log -- <path>` says whether a passage is load-bearing or leftover.
2. **Decide what the page should say.** This is the step that produces the
   change; everything after it is expression.
3. **Write the English.** `references/english.md`.
4. **Write the Japanese from the same understanding, not from the English.**
   `references/japanese.md`. A sentence that only makes sense as a translation
   of an English sentence is the thing this skill exists to prevent.
5. **Run the checker** (below) until it is silent.
6. **Follow the links and anchors you touched.** A renamed heading breaks every
   `#anchor` pointing at it, in both languages.
7. **Build**: `cd docs && pnpm build`. A page named in `PAGES` with no file
   behind it fails the build on purpose.

## What the two languages share

The checker enforces the first four, because a mismatch is always a mistake:

- **Headings** — same level, same order, one for one. Only the text differs.
- **Code blocks** — same number, same order, same info string, and byte for
  byte the same contents, apart from comments.
- **`:::` containers** — same type in the same place. The title after
  `::: tip` is prose and is translated.
- **Tables** — same number of columns and rows.
- **Claims** — if one language says a flag exists, so does the other. Drift
  here is how `docs/ja/guide/runtimes.md` came to be missing a section of
  `docs/guide/runtimes.md` entirely.

**Console output is never translated.** It is what the program printed.

**Comments inside code blocks are translated**, because they are the writer
speaking, not the program:

```console
$ kobune logs web -f          # follow this branch's logs
$ kobune logs web -f          # このブランチのログを追跡する
```

## What they do not share

Sentence structure. English here leans on ` — ` to append a qualification;
Japanese does not inherit that dash, and a page whose Japanese has one dash per
English dash has been translated rather than written. `references/japanese.md`
says what to do instead.

Paragraph boundaries and heading text may also differ where the language needs
it. The heading *count* may not.

## Checking

```console
$ node .claude/skills/prose/scripts/check.mjs             # everything
$ node .claude/skills/prose/scripts/check.mjs docs/guide  # a subtree
$ node .claude/skills/prose/scripts/check.mjs --json      # for an agent
```

It exits non-zero when it finds anything, and it only reports what needs no
judgement: column width, the Japanese typographic rules, the English ones, the
structural parity above, `PAGES` titles against each page's `H1`, and links —
both the ones resolving to no file and the `#anchors` naming no heading.

**Renaming a heading is what the anchor check is for.** It knows that a page on
the site and a page read on GitHub slug their headings differently, so it can be
trusted on `docs/DESIGN.md` as well as on the guide.

CI runs it on every pull request, as the `Prose` job in `ci.yml`. Run it
yourself first — finding out from a red tick is slower than finding out from
the terminal you are already in.

**A silent checker does not mean the page is good.** Nothing here detects
translationese, a wrong claim, or a paragraph that explains the mechanism before
saying what it is for. Those are read for.

## Stop and ask

- A claim you cannot verify in the code or by running the command. Leave the
  sentence as it is and say so; do not soften it into something vaguer that
  happens to be safe.
- A rule in the reference files that the surrounding page contradicts
  throughout. One of the two is wrong and it is worth knowing which.
- Anything that changes what the software does, rather than what the
  documentation says about it.
