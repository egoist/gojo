# Gojo website

The home page and, at `/docs`, the docs of [`../docs`](../docs), built with
TanStack Start and SolidJS. Every page is prerendered at build time, and
Cloudflare Workers serves the result as static assets, with no Worker code.

```sh
bun install                    # at the root of the repository
bun run --cwd website dev      # http://localhost:3000
bun run --cwd website test     # the Gojo grammar
bun run --cwd website build    # dist/client
bun run --cwd website preview  # dist/client, served by wrangler as in production
bun run --cwd website deploy   # build, then wrangler deploy
```

Pages are rendered from the markdown when the site is built. The sidebar
comes from the lists of `docs/README.md`, under their headings, then the
modules of `docs/std/README.md`'s table. Pages keep their path
(`std/os.md` is `/docs/std/os`), a directory's `README.md` is the page of the
directory, and links between pages become links of the site; links to other
files of the repository go to GitHub. Lists of pages,
`- [Title](page.md): what it covers`, become cards. A ```` ```text ```` block
right after a ```` ```gojo ```` block is shown as that program's output.

The home page's programs come from the README and the tour (`src/lib/home.server.ts`
names the sections), so they are the ones the test suite compiles and runs.

## Syntax highlighting

Code is highlighted by Shiki at build time, in both color schemes. Gojo has a
TextMate grammar of its own, `src/lib/gojo-grammar.ts`, written from the lexer
(`src/lexer.rs`). `test/grammar.test.ts` checks that it has exactly the
lexer's keywords, checks the scopes of each construct, and highlights every
Gojo program of the repository (`std`, `examples`, `tests/run` and the
```` ```gojo ```` blocks of the docs) without an invalid token or a string,
comment or bracket left open. Adding a keyword to the lexer fails the test
until the grammar has it.

## For agents

The same build generates `/llms.txt`, an index of the docs as Markdown, and
`/llms-full.txt`, all pages combined. Append `.md` to a docs page's URL for
its Markdown version: `/docs.md`, `/docs/tour.md`, `/docs/std/os.md`. Docs
pages advertise both through link elements.
