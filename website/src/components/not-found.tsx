import { Link } from "@tanstack/solid-router"

import { button } from "@/lib/ui"

export function NotFound() {
  return (
    <main class="flex flex-1 flex-col items-center justify-center px-6 py-32 text-center">
      <p class="font-mono text-sm text-muted-foreground">404</p>
      <h1 class="mt-3 text-3xl font-semibold tracking-tight">This page does not exist</h1>
      <p class="mt-3 max-w-sm text-muted-foreground">It may have moved. The docs, or their search, will have what you are looking for.</p>
      <div class="mt-8 flex gap-3">
        <Link to="/docs" class={button.primary}>
          Read the docs
        </Link>
        <Link to="/" class={button.outline}>
          Home
        </Link>
      </div>
    </main>
  )
}
