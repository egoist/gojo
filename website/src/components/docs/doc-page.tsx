import { getRouteApi } from "@tanstack/solid-router"
import ArrowLeftIcon from "lucide-solid/icons/arrow-left"
import ArrowRightIcon from "lucide-solid/icons/arrow-right"
import ChevronRightIcon from "lucide-solid/icons/chevron-right"
import MenuIcon from "lucide-solid/icons/menu"
import XIcon from "lucide-solid/icons/x"
import { Show } from "solid-js"

import { DocContent } from "@/components/docs/doc-content"
import { DocLink } from "@/components/docs/doc-link"
import { DocsNav } from "@/components/docs/docs-nav"
import { getDoc, type Doc, type NavSection } from "@/lib/docs"

/**
 * The loader of a page: null, before anything is fetched, for a slug without
 * a page, which the docs show as not found. (Rather than throwing notFound():
 * the router renders routes as the prerendered 404.html does, not errors.)
 */
export async function loadDocPage(slug: string, parent: Promise<{ loaderData?: NavSection[] }>) {
  const nav = (await parent).loaderData
  if (!nav?.some((s) => s.items.some((i) => i.slug === slug))) return null
  return getDoc({ data: slug })
}

export function docHead(doc: Doc | null | undefined) {
  if (!doc) return {}
  const title = doc.slug ? `${doc.section === "Standard library" ? `std/${doc.title}` : doc.title} · Gojo` : "Documentation · Gojo"
  return {
    meta: [
      { title },
      { name: "description", content: doc.description },
      { property: "og:title", content: title },
      { property: "og:description", content: doc.description },
    ],
    links: [
      { rel: "alternate", type: "text/markdown", href: (doc.slug ? `/docs/${doc.slug}` : "/docs") + ".md" },
      { rel: "describedby", href: "/llms.txt" },
    ],
  }
}

const docsRoute = getRouteApi("/docs")

export function DocPage(props: { doc: Doc }) {
  const nav = docsRoute.useLoaderData()
  let menu!: HTMLDialogElement

  return (
    <div class="min-w-0">
      <div class="sticky top-14 z-30 flex h-11 items-center gap-1 border-b bg-background/85 px-2 backdrop-blur-xl lg:hidden">
        <button
          type="button"
          onClick={() => menu.showModal()}
          class="inline-flex h-8 items-center gap-1.5 rounded-md px-2.5 text-sm font-medium hover:bg-muted [&_svg]:size-4"
        >
          <MenuIcon /> Menu
        </button>
        <ChevronRightIcon class="size-3.5 shrink-0 text-muted-foreground" />
        <span class="truncate text-sm">{props.doc.title}</span>
      </div>
      <dialog
        ref={menu}
        aria-label="Documentation"
        onClick={(e) => e.target === menu && menu.close()}
        class="fixed inset-y-0 left-0 m-0 h-full max-h-none w-72 max-w-[85vw] overflow-y-auto border-r bg-background p-0 text-foreground backdrop:bg-black/40"
      >
        <div class="flex h-14 items-center justify-between border-b px-4">
          <span class="font-semibold">Documentation</span>
          <button type="button" class="icon-button" aria-label="Close" onClick={() => menu.close()}>
            <XIcon />
          </button>
        </div>
        <div class="px-2 py-6">
          <DocsNav sections={nav()} page={props.doc} onNavigate={() => menu.close()} />
        </div>
      </dialog>

      <article class="max-w-[52rem] px-4 pt-10 pb-20 sm:px-10 lg:pt-14">
        <header>
          <Show when={props.doc.section}>
            <p class="label">{props.doc.section}</p>
          </Show>
          <h1 class="mt-3 text-3xl font-semibold tracking-[-0.03em] sm:text-4xl">
            <Show when={props.doc.section === "Standard library"}>
              <span class="text-muted-foreground">std/</span>
            </Show>
            {props.doc.title}
          </h1>
          <Show when={props.doc.summary}>
            <p class="mt-3 text-lg leading-8 text-muted-foreground">{props.doc.summary}</p>
          </Show>
        </header>

        <div class="mt-10">
          <DocContent html={props.doc.html} />
        </div>

        <footer class="mt-16">
          <nav class="grid gap-px overflow-hidden rounded-lg border bg-border sm:grid-cols-2">
            <Show when={props.doc.prev} fallback={<span class="hidden bg-background sm:block" />}>
              {(prev) => (
                <DocLink slug={prev().slug} class="flex flex-col gap-1 bg-background p-4 transition-colors hover:bg-code">
                  <span class="label flex items-center gap-1.5">
                    <ArrowLeftIcon class="size-3" /> Previous
                  </span>
                  <span class="font-medium">{prev().title}</span>
                </DocLink>
              )}
            </Show>
            <Show when={props.doc.next}>
              {(next) => (
                <DocLink slug={next().slug} class="flex flex-col items-end gap-1 bg-background p-4 text-right transition-colors hover:bg-code">
                  <span class="label flex items-center gap-1.5">
                    Next <ArrowRightIcon class="size-3" />
                  </span>
                  <span class="font-medium">{next().title}</span>
                </DocLink>
              )}
            </Show>
          </nav>
          <div class="mt-6 flex flex-wrap gap-x-6 gap-y-2 text-sm text-muted-foreground">
            <a href={props.doc.editUrl} class="hover:text-foreground">
              Edit this page on GitHub
            </a>
            <a href={(props.doc.slug ? `/docs/${props.doc.slug}` : "/docs") + ".md"} class="hover:text-foreground">
              View as Markdown
            </a>
          </div>
        </footer>
      </article>
    </div>
  )
}
