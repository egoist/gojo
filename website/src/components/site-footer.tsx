import { Link } from "@tanstack/solid-router"

import { site } from "@/lib/site"

export function SiteFooter() {
  return (
    <footer class="frame label flex flex-wrap items-center justify-between gap-4 border-t px-4 py-6 sm:px-6">
      <span>Gojo {__GOJO_VERSION__} · beta</span>
      <nav class="flex flex-wrap gap-x-6 gap-y-2">
        <Link to="/docs" class="hover:text-foreground">
          Docs
        </Link>
        <a href="/llms.txt" class="hover:text-foreground">
          llms.txt
        </a>
        <a href={`${site.repo}/releases`} class="hover:text-foreground">
          Releases
        </a>
        <a href={site.repo} class="hover:text-foreground">
          GitHub
        </a>
      </nav>
    </footer>
  )
}
