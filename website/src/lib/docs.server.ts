// The docs of the repository (../docs), read and rendered on the server. The
// site is prerendered, so this runs at build time only.

import fs from "node:fs/promises"
import path from "node:path"
import type { List, Root, Table } from "mdast"
import { toString } from "mdast-util-to-string"
import remarkGfm from "remark-gfm"
import remarkParse from "remark-parse"
import { unified } from "unified"

import type { Doc, NavItem, NavSection, SearchEntry } from "@/lib/docs"
import { renderMarkdown, slugOf, type RenderedMarkdown } from "@/lib/markdown.server"
import { site } from "@/lib/site"

// Vite runs in website/, next to the repository's docs.
const repoDir = path.resolve(process.cwd(), "..")
const docsDir = path.join(repoDir, "docs")

/** The files a page may be: x.md, else x/README.md, as std/README.md is /docs/std. */
function filesOf(slug: string) {
  if (slug === "") return ["README.md"]
  if (!/^[a-z0-9-]+(\/[a-z0-9-]+)*$/.test(slug)) return []
  return [`${slug}.md`, `${slug}/README.md`]
}

/** The file of a page, relative to the docs, or undefined for none. */
async function fileOf(slug: string) {
  for (const file of filesOf(slug)) {
    if (await fs.stat(path.join(docsDir, file)).then((s) => s.isFile(), () => false)) return file
  }
  return undefined
}

// Pages are rendered once per build; in development, on every request.
const cache = new Map<string, Promise<RenderedMarkdown | undefined>>()

function render(slug: string) {
  let page = cache.get(slug)
  if (!page) {
    page = fileOf(slug).then((file) => {
      if (!file) return undefined
      const abs = path.join(docsDir, file)
      return fs.readFile(abs, "utf8").then(
        (source) => renderMarkdown(source, abs, { docsDir, repoDir, repoUrl: site.repo }),
        () => undefined
      )
    })
    if (!import.meta.env.DEV) cache.set(slug, page)
  }
  return page
}

const parse = (source: string) => unified().use(remarkParse).use(remarkGfm).parse(source) as Root

/**
 * The sidebar: the lists of docs/README.md, in their order, under their
 * headings; then the modules of std/README.md's table; then any page that
 * neither lists.
 */
export async function loadNav(): Promise<NavSection[]> {
  const readme = parse(await fs.readFile(path.join(docsDir, "README.md"), "utf8"))
  const sections: NavSection[] = [{ title: "Overview", items: [{ slug: "", title: "Introduction" }] }]
  let heading = ""
  for (const node of readme.children) {
    if (node.type === "heading" && node.depth === 2) heading = toString(node)
    if (node.type === "list" && heading) sections.push({ title: heading, items: listItems(node, "") })
  }

  const std = parse(await fs.readFile(path.join(docsDir, "std/README.md"), "utf8"))
  const table = std.children.find((node): node is Table => node.type === "table")
  if (table) sections.push({ title: "Standard library", items: tableItems(table, "std") })

  const listed = new Set(sections.flatMap((s) => s.items.map((i) => i.slug)))
  const files = (await fs.readdir(docsDir, { recursive: true })).filter((f) => f.endsWith(".md")).sort()
  const rest: NavItem[] = []
  for (const file of files) {
    const slug = slugOf(file)
    if (listed.has(slug)) continue
    const page = await render(slug)
    rest.push({ slug, title: page?.title || slug })
  }
  if (rest.length) sections.push({ title: "More", items: rest })
  return sections
}

/** `- [Title](page.md): what it covers`. */
function listItems(list: List, base: string): NavItem[] {
  return list.children.flatMap((item): NavItem[] => {
    const paragraph = item.children[0]
    if (paragraph?.type !== "paragraph") return []
    const [first, ...rest] = paragraph.children
    if (first?.type !== "link" || !first.url.endsWith(".md") || /^[a-z]+:/.test(first.url)) return []
    const description = sentence(rest.map((n) => toString(n)).join(""))
    return [{ slug: slugOf(path.join(base, first.url)), title: toString(first), description }]
  })
}

/** `| [`os`](os.md) | Files, directories, … |`. */
function tableItems(table: Table, base: string): NavItem[] {
  return table.children.slice(1).flatMap((row): NavItem[] => {
    const [name, description] = row.children
    const link = name?.children.find((n) => n.type === "link")
    if (link?.type !== "link" || !link.url.endsWith(".md")) return []
    return [{ slug: slugOf(path.join(base, link.url)), title: toString(link), description: description ? sentence(toString(description)) : undefined }]
  })
}

/** ": the language by example, …" → "The language by example, …". */
function sentence(text: string) {
  const s = text.replace(/^[\s:]+/, "").replace(/\s+/g, " ").trim()
  return s.charAt(0).toUpperCase() + s.slice(1)
}

export async function loadDoc(slug: string): Promise<Doc | undefined> {
  const [page, sections, file] = await Promise.all([render(slug), loadNav(), fileOf(slug)])
  if (!page || !file) return undefined
  // A page listed twice, as std is, keeps its first place.
  const order = sections.flatMap((s) => s.items).filter((item, i, all) => all.findIndex((x) => x.slug === item.slug) === i)
  const index = order.findIndex((i) => i.slug === slug)
  const item = order[index]
  const link = (i: NavItem | undefined) => (i ? { slug: i.slug, title: i.title } : undefined)
  // std's table repeats the first paragraph of each module's page.
  const plain = (s: string) => s.toLowerCase().replace(/[^a-z0-9]+/g, " ").trim()
  const summary = item?.description && plain(item.description) !== plain(page.lead) ? item.description : undefined
  return {
    slug,
    title: page.title,
    description: item?.description || page.lead,
    summary,
    section: sections.find((s) => s.items.some((i) => i.slug === slug))?.title,
    html: page.html,
    toc: page.toc,
    prev: index > 0 ? link(order[index - 1]) : undefined,
    next: index >= 0 ? link(order[index + 1]) : undefined,
    editUrl: `${site.repo}/edit/main/docs/${file}`,
  }
}

/** Every heading of every page, with its text, for the search of the site. */
export async function loadSearchIndex(): Promise<SearchEntry[]> {
  const slugs = new Set((await loadNav()).flatMap((s) => s.items.map((i) => i.slug)))
  const entries: SearchEntry[] = []
  for (const slug of slugs) {
    const page = await render(slug)
    if (!page) continue
    for (const section of page.sections) {
      entries.push({ slug, page: page.title, id: section.id, heading: section.heading, text: section.text })
    }
  }
  return entries
}
