// The programs of the home page, taken from the README and the tour when the
// site is built: the tour's are compiled and run by the test suite, with the
// output that follows them.

import fs from "node:fs/promises"
import path from "node:path"
import GithubSlugger from "github-slugger"

import { highlightCode } from "@/lib/markdown.server"

const repoDir = path.resolve(process.cwd(), "..")

export interface Sample {
  id: string
  label: string
  blurb: string
  html: string
  output?: string
  /** The section of the tour it comes from. */
  href: string
}

const samples = [
  {
    id: "glance",
    label: "At a glance",
    blurb: "Structs with defaults, errors as values, optional fields and string interpolation, from the README.",
    from: { file: "README.md", section: undefined, index: 0 },
  },
  {
    id: "nil",
    label: "No nil",
    blurb: "A value that may be missing has type ?T. if let, ?? and match take it apart; there is no null pointer to forget.",
    from: { file: "docs/tour.md", section: "No nil", index: 0 },
  },
  {
    id: "ownership",
    label: "Ownership",
    blurb: "Every value has one owner and is freed when the owner goes out of scope. Parameters borrow; inout changes, sink takes.",
    from: { file: "docs/tour.md", section: "Ownership", index: 0 },
  },
  {
    id: "concurrency",
    label: "Concurrency",
    blurb: "A scope waits for every task it starts, so tasks may borrow what outlives it. go gives a handle whose join is the result.",
    from: { file: "docs/tour.md", section: "Concurrency", index: 1 },
  },
] as const

/** The nth ```gojo block under a heading (or of the whole file), and the ```text block right after it. */
function extract(source: string, section: string | undefined, index: number) {
  let body = source
  if (section) {
    const start = source.indexOf(`\n## ${section}\n`)
    if (start < 0) throw new Error(`no section "${section}"`)
    const end = source.indexOf("\n## ", start + 1)
    body = source.slice(start, end < 0 ? undefined : end)
  }
  const blocks = [...body.matchAll(/^```gojo\n([\s\S]*?)^```\n(?:\n```text\n([\s\S]*?)^```$)?/gm)]
  const block = blocks[index]
  if (!block) throw new Error(`no Gojo block ${index} in "${section ?? "the file"}"`)
  return { code: block[1]!.replace(/\n$/, ""), output: block[2]?.replace(/\n$/, "") }
}

export async function loadHome(): Promise<{ samples: Sample[] }> {
  const files = new Map<string, string>()
  const read = async (file: string) => files.get(file) ?? files.set(file, await fs.readFile(path.join(repoDir, file), "utf8")).get(file)!
  return {
    samples: await Promise.all(
      samples.map(async (s) => {
        const { code, output } = extract(await read(s.from.file), s.from.section, s.from.index)
        const href = s.from.section ? `/docs/tour#${new GithubSlugger().slug(s.from.section)}` : "/docs/tour"
        return { id: s.id, label: s.label, blurb: s.blurb, html: await highlightCode(code, "gojo"), output, href }
      })
    ),
  }
}
