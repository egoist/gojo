import { For, Show } from "solid-js"

import { DocLink } from "@/components/docs/doc-link"
import { useActiveHeading } from "@/components/docs/toc"
import type { NavSection, TocItem } from "@/lib/docs"

const item = "flex items-center gap-1 rounded-md px-2.5 py-1.5 text-muted-foreground transition-colors hover:text-foreground"

/** The pages of the docs, with the sections of the current page under it. */
export function DocsNav(props: { sections: NavSection[]; page?: { slug: string; toc: TocItem[] }; onNavigate?: () => void }) {
  const headings = () => props.page?.toc.filter((t) => t.depth === 2) ?? []
  const active = useActiveHeading(headings)
  // std is in the reference and has a section of its own: its headings go under the first.
  const firstOf = (slug: string, section: NavSection) => props.sections.find((s) => s.items.some((i) => i.slug === slug)) === section
  return (
    <nav class="flex flex-col gap-6 text-sm">
      <For each={props.sections}>
        {(section) => (
          <div>
            <h4 class="label mb-2 px-2.5">{section.title}</h4>
            <ul class="flex flex-col gap-px">
              <For each={section.items}>
                {(p) => (
                  <li>
                    <DocLink slug={p.slug} onClick={props.onNavigate} class={item} activeClass="bg-muted font-medium text-foreground!">
                      {p.title}
                    </DocLink>
                    <Show when={p.slug === props.page?.slug && headings().length > 0 && firstOf(p.slug, section)}>
                      <ul class="my-1.5 ml-3.5 border-l">
                        <For each={headings()}>
                          {(h) => (
                            <li>
                              <a
                                href={`#${h.id}`}
                                onClick={props.onNavigate}
                                data-active={active() === h.id || undefined}
                                class="-ml-px block border-l border-transparent py-1 pr-2 pl-3 text-[13px] leading-5 text-muted-foreground transition-colors hover:text-foreground data-active:border-brand data-active:text-foreground"
                              >
                                {h.text}
                              </a>
                            </li>
                          )}
                        </For>
                      </ul>
                    </Show>
                  </li>
                )}
              </For>
            </ul>
          </div>
        )}
      </For>
    </nav>
  )
}
