import { createFileRoute } from "@tanstack/solid-router"
import { Show } from "solid-js"

import { DocPage, docHead, loadDocPage } from "@/components/docs/doc-page"
import { NotFound } from "@/components/not-found"

export const Route = createFileRoute("/docs/")({
  loader: ({ parentMatchPromise }) => loadDocPage("", parentMatchPromise),
  head: ({ loaderData }) => docHead(loaderData),
  component: DocRoute,
})

function DocRoute() {
  const doc = Route.useLoaderData()
  return (
    <Show when={doc()} fallback={<NotFound />}>
      {(d) => <DocPage doc={d()} />}
    </Show>
  )
}
