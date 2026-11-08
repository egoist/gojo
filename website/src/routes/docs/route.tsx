import { Outlet, createFileRoute, useMatches } from "@tanstack/solid-router"

import { DocsNav } from "@/components/docs/docs-nav"
import { getDocsNav, type Doc } from "@/lib/docs"

export const Route = createFileRoute("/docs")({
  loader: () => getDocsNav(),
  staleTime: Infinity,
  component: DocsLayout,
})

function DocsLayout() {
  const nav = Route.useLoaderData()
  const page = useMatches({ select: (matches) => matches.at(-1)?.loaderData as Doc | null | undefined })
  return (
    <div class="flex-1 lg:grid lg:grid-cols-[15rem_minmax(0,1fr)]">
      <aside class="sticky top-14 hidden h-[calc(100svh-3.5rem)] overflow-y-auto border-r px-3 py-8 lg:block">
        <DocsNav sections={nav()} page={page()?.toc ? page()! : undefined} />
      </aside>
      <Outlet />
    </div>
  )
}
