# English

Rules inferred from the corpus as it stood at `aa141bf`, with the count that
justified each one. Where a rule says "0 exceptions", a new exception is a bug
in the sentence, not evidence that the rule has softened.

## Person and voice

**Second person.** `you` 124, `your` 47. The reader is the one doing the thing.

**`we` and `our` do not appear.** One instance today, in `guide/agents.md`.
There is no project voice speaking about itself in the first person; where you
want one, name the thing instead — "Minato waits for the health check", not "we
wait for the health check".

**Present tense, indicative.** "`minato url` prints one line", not "will print".

**Address the software as a subject where it has agency**, and the reader where
they do. "`depends_on` waits for the dependency to be ready" and "Commit it".

## Spelling

**British.** `colour` 8, `behaviour` 1, `virtualisation` 1, `recognises`. So
`-ise` and `-isation` rather than `-ize`, `colour`/`behaviour`/`favourite`
rather than the American forms.

Exempt, because they are names or identifiers rather than words:

- `LICENSE`, the file; `Apache License 2.0`, the licence's own name
- anything inside a code span or a code block — flags, keys, output
- product names as their owners spell them

## Punctuation

**`—` always has a space either side.** 180 occurrences, 0 without. Never `--`,
never `–`, never `—` closed up against a word.

The dash appends a qualification the sentence has earned; it does not join two
independent thoughts that wanted a full stop:

> Your worktree is mounted at `/workspace` — anything a build writes under it
> lands in the repository on the host.

**No contractions.** Three exceptions survive today (`isn't` in
`guide/getting-started.md` and `guide/agents.md`, `don't` in
`guide/configuration.md`) and they are the outliers, not the licence. Write
`is not`, `does not`, `cannot`.

**Straight quotes**, `'` and `"`. Typographic quotes are not used.

**Serial comma** is not used: "a framed panel, columns that line up, and colour
on the parts that carry meaning" takes the comma because the list needs it, not
by rule.

## Headings

**Sentence case.** `Your first environment`, `What the output looks like`,
`CLI commands`, `Keeping it up to date`. Not Title Case, and no trailing colon.

**A heading is the answer to why the reader is here**, so `Service URLs go
through /etc/hosts` rather than `/etc/hosts`.

A heading's text is an anchor. Renaming one breaks every `#link` to it in both
languages — grep before you rename.

## Paragraph shapes

**The bold lead-in.** A rule the reader must not miss opens its paragraph in
bold and is stated flat, then explained:

> **`curl -s` on its own is not enough.** It swallows errors, leaving nothing
> that looks like anything but an empty response.

Use it for the thing that will otherwise be skimmed past. A page where every
paragraph starts bold has no emphasis left.

**Say the failure mode.** The corpus repeatedly names what a mistake looks like
from the outside, because that is how a reader recognises they have made it:
"A stale process answers exactly like a change that did not work".

**Prefer the concrete noun to the abstraction.** "the proxy is not listening on
that address", not "connectivity is not established".

## Layout

**Wrap prose at 80 columns.** Not applicable to, and not counted for:

- YAML frontmatter — `tagline:` and `details:` in `docs/index.md` are data
- table rows
- code blocks
- a line whose overflow is one URL that cannot be broken

`README.md`'s feature list is over the limit on eight lines today and is not a
precedent.

**One sentence may span lines.** Wrap where the line runs out, not where the
sentence ends; the corpus does not one-sentence-per-line.

**A file ends with exactly one newline** and carries no trailing whitespace.

## Words

| Not | But |
| --- | --- |
| `simply`, `just`, `easy`, `obviously` | nothing. If it were obvious the sentence would not be there |
| `please` | the imperative on its own |
| `utilise` | `use` |
| `in order to` | `to` |
| `allows you to` | the verb: "`--fresh` runs it in a throwaway container" |
| `should` for behaviour | `does`, or say what it does instead |
| `we recommend` | say it: "Use `-sS --fail-with-body`" |

**`worktree`** is one word, lowercase, and is the git term. **`workspace`** is
Minato's name for the environment a worktree has. They are not interchangeable;
`references/glossary.md` has the rest.
