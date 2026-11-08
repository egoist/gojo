import { Link, useLocation } from "@tanstack/solid-router"
import MenuIcon from "lucide-solid/icons/menu"
import XIcon from "lucide-solid/icons/x"
import { For, Show, createEffect, createSignal, on } from "solid-js"

import { GitHubIcon } from "@/components/icons"
import { Wordmark } from "@/components/logo"
import { Search } from "@/components/search"
import { ThemeToggle } from "@/components/theme-toggle"
import { slugOfPath } from "@/lib/docs"
import { site } from "@/lib/site"

/** The header's links; a page of the docs belongs to the first whose slugs it starts with. */
const links = [
  { title: "Tour", slug: "tour", match: ["tour"] },
  { title: "Reference", slug: "spec", match: ["spec"] },
  { title: "Std", slug: "std", match: ["std"] },
  { title: "Design", slug: "design", match: ["design", "concurrency", "packages"] },
]

function linkOf(pathname: string) {
  if (!pathname.startsWith("/docs")) return undefined
  const slug = slugOfPath(pathname)
  return links.find((l) => l.match.some((m) => slug === m || slug.startsWith(`${m}/`)))?.slug ?? "docs"
}

const navLink = "text-muted-foreground transition-colors hover:text-foreground data-current:text-foreground"

export function SiteHeader() {
  const location = useLocation()
  const current = () => linkOf(location().pathname)
  const [menuOpen, setMenuOpen] = createSignal(false)
  createEffect(on(() => location().pathname, () => setMenuOpen(false), { defer: true }))

  return (
    <header class="frame sticky top-0 z-40 border-b bg-background/80 backdrop-blur-xl">
      <div class="flex h-14 items-center gap-6 px-4 sm:px-6">
        <Link to="/" aria-label="Gojo home">
          <Wordmark />
        </Link>
        <nav class="hidden items-center gap-5 text-sm sm:flex">
          <Link to="/docs" class={navLink} data-current={current() === "docs" || undefined}>
            Docs
          </Link>
          <For each={links}>
            {(link) => (
              <Link to="/docs/$" params={{ _splat: link.slug }} class={navLink} data-current={current() === link.slug || undefined}>
                {link.title}
              </Link>
            )}
          </For>
        </nav>
        <div class="ml-auto flex items-center gap-1">
          <Search />
          <a href={site.repo} aria-label="GitHub" class="icon-button">
            <GitHubIcon />
          </a>
          <ThemeToggle />
          <button
            type="button"
            class="icon-button sm:hidden"
            aria-label="Navigation menu"
            aria-expanded={menuOpen()}
            aria-controls="mobile-nav"
            onClick={() => setMenuOpen((o) => !o)}
          >
            <Show when={menuOpen()} fallback={<MenuIcon />}>
              <XIcon />
            </Show>
          </button>
        </div>
      </div>
      <Show when={menuOpen()}>
        <nav id="mobile-nav" class="flex flex-col border-t p-2 text-sm sm:hidden">
          <Link to="/docs" class="rounded-md px-3 py-2.5 data-current:bg-muted" data-current={current() === "docs" || undefined}>
            Docs
          </Link>
          <For each={links}>
            {(link) => (
              <Link
                to="/docs/$"
                params={{ _splat: link.slug }}
                class="rounded-md px-3 py-2.5 data-current:bg-muted"
                data-current={current() === link.slug || undefined}
              >
                {link.title}
              </Link>
            )}
          </For>
        </nav>
      </Show>
    </header>
  )
}
