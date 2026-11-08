import { createFileRoute } from "@tanstack/solid-router"

import { NotFound } from "@/components/not-found"
import { notFoundScript } from "@/lib/not-found"

// Prerendered as 404.html, which Cloudflare serves for paths without a page.
export const Route = createFileRoute("/404")({
  head: () => ({
    meta: [{ title: "Page not found · Gojo" }],
    scripts: [{ children: notFoundScript }],
  }),
  component: NotFound,
})
