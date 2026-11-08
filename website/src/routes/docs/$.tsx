import { createFileRoute } from "@tanstack/solid-router"
import { Show } from "solid-js"

import { DocPage, docHead, loadDocPage } from "@/components/docs/doc-page"
import { NotFound } from "@/components/not-found"

// Every page but the index, nested ones too: /docs/std/os.
export const Route = createFileRoute("/docs/$")({
  loader: ({ params, parentMatchPromise }) => loadDocPage(params._splat ?? "", parentMatchPromise),
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
