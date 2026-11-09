// Markdown to HTML, on the server at build time: GitHub-flavored markdown,
// heading anchors that match GitHub's, links rewritten for the site, and code
// highlighted by Shiki for both color schemes, Gojo with its own grammar.

import GithubSlugger from "github-slugger"
import type { Element, ElementContent, Root, RootContent } from "hast"
import { toString } from "hast-util-to-string"
import rehypeStringify from "rehype-stringify"
import remarkGfm from "remark-gfm"
import remarkParse from "remark-parse"
import remarkRehype from "remark-rehype"
import { createHighlighter, type Highlighter } from "shiki"
import { unified } from "unified"
import { visit } from "unist-util-visit"

import type { TocItem } from "@/lib/docs"
import { gojoGrammar } from "@/lib/gojo-grammar"
import { omitRepositoryOnly, rewriteHref, type LinkOptions } from "@/lib/markdown-source"

export { slugOf } from "@/lib/markdown-source"

/** The text of a page under one heading, for search. */
export interface Section {
  id: string
  heading: string
  text: string
}

export interface RenderedMarkdown {
  title: string
  /** The first paragraph, as text. */
  lead: string
  html: string
  toc: TocItem[]
  sections: Section[]
}

const themes = { light: "github-light-default", dark: "github-dark-default" } as const

let highlighter: Promise<Highlighter> | undefined
function getHighlighter() {
  highlighter ??= createHighlighter({
    themes: Object.values(themes),
    langs: [gojoGrammar, "sh", "toml", "c", "json", "rust", "go"],
  })
  return highlighter
}

export async function renderMarkdown(source: string, file: string, links: LinkOptions): Promise<RenderedMarkdown> {
  const shiki = await getHighlighter()
  const out: RenderedMarkdown = { title: "", lead: "", html: "", toc: [], sections: [] }
  const processor = unified()
    .use(remarkParse)
    .use(remarkGfm)
    .use(() => omitRepositoryOnly)
    .use(remarkRehype)
    .use(() => (tree: Root) => transform(tree, out, shiki, file, links))
    .use(rehypeStringify)
  out.html = String(await processor.process(source))
  return out
}

function transform(tree: Root, out: RenderedMarkdown, shiki: Highlighter, file: string, links: LinkOptions) {
  const slugger = new GithubSlugger()
  let section: Section = { id: "", heading: "", text: "" }
  out.sections.push(section)

  // Headings are top-level, so one pass finds the title, the table of
  // contents and the sections in document order.
  tree.children = tree.children.flatMap((node): RootContent[] => {
    if (node.type !== "element") return [node]
    const depth = headingDepth(node)
    if (depth === 1 && !out.title) {
      out.title = section.heading = toString(node)
      slugger.slug(out.title) // GitHub counts the title in its anchors too
      return []
    }
    if (depth >= 2) {
      const text = toString(node)
      const id = slugger.slug(text)
      node.properties.id = id
      if (!containsLink(node)) {
        node.children = [{ type: "element", tagName: "a", properties: { href: `#${id}` }, children: node.children }]
      }
      if (depth <= 3) {
        out.toc.push({ id, text, depth })
        section = { id, heading: text, text: "" }
        out.sections.push(section)
      }
      return [node]
    }
    const text = toString(node).replace(/\s+/g, " ").trim()
    if (node.tagName === "p" && !out.lead) out.lead = text
    if (node.tagName !== "pre") section.text += " " + text
    return [node]
  })
  for (const s of out.sections) s.text = s.text.trim()

  // A ```text block right after a program is what the program prints.
  let previous: Element | undefined
  for (const node of tree.children) {
    if (node.type === "text" && !node.value.trim()) continue
    if (node.type === "element" && node.tagName === "pre" && langOf(node) === "text" && previous && langOf(previous) === "gojo") {
      node.properties.dataOutput = ""
    }
    previous = node.type === "element" ? node : undefined
  }

  visit(tree, "element", (node, index, parent) => {
    if (node.tagName === "a" && typeof node.properties.href === "string") {
      rewriteLink(node, node.properties.href, file, links)
    }
    if (!parent || index === undefined) return
    if (node.tagName === "pre") {
      parent.children[index] = codeBlock(node, shiki)
      return "skip"
    }
    // Not skipped: the visit goes on into the table, whose cells may have links.
    if (node.tagName === "table") {
      parent.children[index] = { type: "element", tagName: "div", properties: { className: ["table-wrap"] }, children: [node] }
    }
  })

  cardLists(tree)
}

function headingDepth(node: Element) {
  const m = /^h([1-6])$/.exec(node.tagName)
  return m ? Number(m[1]) : 0
}

function containsLink(node: Element): boolean {
  return node.children.some((c) => c.type === "element" && (c.tagName === "a" || containsLink(c)))
}

function rewriteLink(node: Element, href: string, file: string, links: LinkOptions) {
  const rewritten = rewriteHref(href, file, links)
  node.properties.href = rewritten
  if (/^https?:/.test(rewritten)) node.properties.rel = ["noreferrer"]
}

/** The language of remark-rehype's `<pre><code class="language-x">`, "text" for none. */
function langOf(pre: Element) {
  const code = pre.children.find((c): c is Element => c.type === "element" && c.tagName === "code")
  const classes = (code?.properties.className as string[] | undefined) ?? []
  return classes.find((c) => c.startsWith("language-"))?.slice("language-".length) ?? "text"
}

/** A highlighted code block with a copy button; the output of a program has a caption instead. */
function codeBlock(pre: Element, shiki: Highlighter): Element {
  const output = pre.properties.dataOutput !== undefined
  const code = pre.children.find((c): c is Element => c.type === "element" && c.tagName === "code")
  const highlighted = highlight(shiki, toString(code ?? pre).replace(/\n$/, ""), langOf(pre))
  if (output) {
    return {
      type: "element",
      tagName: "figure",
      properties: { className: ["code-block", "code-output"] },
      children: [
        { type: "element", tagName: "figcaption", properties: {}, children: [{ type: "text", value: "Output" }] },
        highlighted,
      ],
    }
  }
  return {
    type: "element",
    tagName: "div",
    properties: { className: ["code-block"] },
    children: [
      highlighted,
      {
        type: "element",
        tagName: "button",
        properties: { type: "button", className: ["copy-button"], ariaLabel: "Copy code" },
        children: [
          icon("icon-copy", ["M8 8h12a2 2 0 0 1 2 2v10a2 2 0 0 1-2 2H10a2 2 0 0 1-2-2z", "M4 16a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h10a2 2 0 0 1 2 2"]),
          icon("icon-check", ["M20 6 9 17l-5-5"]),
        ],
      },
    ],
  }
}

function icon(className: string, paths: string[]): Element {
  return {
    type: "element",
    tagName: "svg",
    properties: {
      className: [className],
      viewBox: "0 0 24 24",
      fill: "none",
      stroke: "currentColor",
      strokeWidth: "2",
      strokeLinecap: "round",
      strokeLinejoin: "round",
      ariaHidden: "true",
    },
    children: paths.map((d) => ({ type: "element", tagName: "path", properties: { d }, children: [] })),
  }
}

function highlight(shiki: Highlighter, code: string, lang: string): Element {
  const known = shiki.getLoadedLanguages().includes(lang)
  const root = shiki.codeToHast(code, { lang: known ? lang : "text", themes, defaultColor: false })
  const pre = root.children[0] as Element
  // The page styles the background, which Shiki's inline style would override.
  delete pre.properties.style
  delete pre.properties.tabindex
  return pre
}

/** Code highlighted for both color schemes, as HTML. */
export async function highlightCode(code: string, lang: string) {
  const pre = highlight(await getHighlighter(), code, lang)
  return unified().use(rehypeStringify).stringify({ type: "root", children: [pre] })
}

/** Lists of pages become cards: `- [Title](page.md): what it covers`. */
function cardLists(tree: Root) {
  tree.children = tree.children.map((node) => {
    if (node.type !== "element" || node.tagName !== "ul") return node
    const items = node.children.filter((c): c is Element => c.type === "element" && c.tagName === "li")
    const cards = items.map(card)
    if (!cards.every((c) => c !== undefined)) return node
    return { type: "element", tagName: "div", properties: { className: ["doc-cards"] }, children: cards }
  })
}

function card(li: Element): Element | undefined {
  const [first] = li.children.filter((c) => c.type !== "text" || c.value.trim())
  const inline = first?.type === "element" && first.tagName === "p" ? first.children : li.children
  const [head, ...rest] = inline
  if (head?.type !== "element" || head.tagName !== "a") return
  const href = head.properties.href
  if (typeof href !== "string" || !href.startsWith("/docs")) return
  if (rest[0]?.type !== "text" || !rest[0].value.startsWith(":")) return
  return {
    type: "element",
    tagName: "a",
    properties: { href, className: ["doc-card"] },
    children: [
      { type: "element", tagName: "span", properties: { className: ["doc-card-title"] }, children: head.children },
      { type: "element", tagName: "span", properties: { className: ["doc-card-description"] }, children: capitalize(rest) },
    ],
  }
}

/** Drops the ": " between a card's link and its description, and capitalizes the description. */
function capitalize(nodes: ElementContent[]): ElementContent[] {
  const [first, ...rest] = nodes
  if (first?.type !== "text") return nodes
  const value = first.value.replace(/^[\s:]+/, "")
  return [{ ...first, value: value.charAt(0).toUpperCase() + value.slice(1) }, ...rest]
}
