import { useNavigate } from "@tanstack/solid-router"
import FileTextIcon from "lucide-solid/icons/file-text"
import HashIcon from "lucide-solid/icons/hash"
import SearchIcon from "lucide-solid/icons/search"
import { For, Show, createEffect, createMemo, createSignal, onCleanup, onMount } from "solid-js"

import { docHref, type SearchEntry } from "@/lib/docs"

// Prerendered by the /docs/search.json route, and fetched the first time the
// search opens.
let index: Promise<SearchEntry[]> | undefined
function loadIndex() {
  index ??= fetch("/docs/search.json").then((r) => {
    if (!r.ok) throw new Error(`search index: ${r.status}`)
    return r.json() as Promise<SearchEntry[]>
  })
  index.catch(() => (index = undefined))
  return index
}

interface Result {
  entry: SearchEntry
  score: number
}

/** Entries that contain every term, best first: in the heading, then the page's title, then the text. */
function search(entries: SearchEntry[], terms: string[]): Result[] {
  const results: Result[] = []
  for (const entry of entries) {
    const heading = entry.heading.toLowerCase()
    const page = entry.page.toLowerCase()
    const text = entry.text.toLowerCase()
    let score = 0
    for (const term of terms) {
      const s = heading.startsWith(term) ? 12 : heading.includes(term) ? 8 : page.includes(term) ? 3 : text.includes(term) ? 1 : 0
      if (!s) {
        score = 0
        break
      }
      score += s
    }
    if (score) results.push({ entry, score: entry.id ? score : score + 2 })
  }
  return results.sort((a, b) => b.score - a.score).slice(0, 40)
}

function snippet(text: string, terms: string[]) {
  const lower = text.toLowerCase()
  const at = Math.min(...terms.map((t) => lower.indexOf(t)).filter((i) => i >= 0))
  if (!Number.isFinite(at)) return text.slice(0, 140)
  const start = Math.max(0, lower.lastIndexOf(" ", Math.max(0, at - 40)))
  return (start > 0 ? "…" : "") + text.slice(start, start + 160).trim()
}

function Highlight(props: { text: string; terms: string[] }) {
  // New objects for every change, so that each part renders afresh.
  const parts = () => {
    if (!props.terms.length) return [{ text: props.text, mark: false }]
    const pattern = new RegExp(`(${props.terms.map((t) => t.replace(/[.*+?^${}()|[\]\\]/g, "\\$&")).join("|")})`, "gi")
    return props.text.split(pattern).map((text, i) => ({ text, mark: i % 2 === 1 }))
  }
  return <For each={parts()}>{(part) => (part.mark ? <mark class="rounded-sm bg-brand/20 text-foreground">{part.text}</mark> : part.text)}</For>
}

export function Search() {
  let dialog!: HTMLDialogElement
  let input!: HTMLInputElement
  let list!: HTMLUListElement
  const navigate = useNavigate()
  const [mac, setMac] = createSignal(true)
  const [query, setQuery] = createSignal("")
  const [selected, setSelected] = createSignal(0)
  // A plain signal rather than a resource: reading a pending resource would
  // suspend the root <Suspense>, which detaches the page, and so closes the
  // dialog the first time it opens.
  const [entries, setEntries] = createSignal<SearchEntry[]>()
  const [failed, setFailed] = createSignal(false)

  const terms = createMemo(() => query().toLowerCase().split(/\s+/).filter(Boolean))
  const results = createMemo(() => {
    const all = entries()
    if (!all) return []
    return terms().length ? search(all, terms()).map((r) => r.entry) : all.filter((e) => !e.id)
  })

  function show() {
    if (!entries()) {
      setFailed(false)
      loadIndex().then(setEntries, () => setFailed(true))
    }
    dialog.showModal()
    input.select()
  }

  function go(entry: SearchEntry | undefined) {
    if (!entry) return
    dialog.close()
    void navigate({ href: docHref(entry.slug, entry.id) })
  }

  createEffect(() => {
    results()
    setSelected(0)
    list?.scrollTo({ top: 0 })
  })

  createEffect(() => {
    list?.querySelector(`[data-index="${selected()}"]`)?.scrollIntoView({ block: "nearest" })
  })

  onMount(() => {
    setMac(/Mac|iPhone|iPad/.test(navigator.platform))
    function onKeyDown(e: KeyboardEvent) {
      const typing = e.target instanceof HTMLElement && (e.target.isContentEditable || /^(INPUT|TEXTAREA|SELECT)$/.test(e.target.tagName))
      if ((e.key === "k" && (e.metaKey || e.ctrlKey)) || (e.key === "/" && !typing)) {
        e.preventDefault()
        if (dialog.open) dialog.close()
        else show()
      }
    }
    document.addEventListener("keydown", onKeyDown)
    onCleanup(() => document.removeEventListener("keydown", onKeyDown))
  })

  function onInputKeyDown(e: KeyboardEvent) {
    const count = results().length
    if (e.key === "ArrowDown") setSelected((i) => (count ? (i + 1) % count : 0))
    else if (e.key === "ArrowUp") setSelected((i) => (count ? (i - 1 + count) % count : 0))
    else if (e.key === "Enter") go(results()[selected()])
    else return
    e.preventDefault()
  }

  return (
    <>
      <button
        type="button"
        onClick={show}
        onPointerEnter={() => void loadIndex().catch(() => {})}
        class="hidden h-8 w-56 items-center gap-2 rounded-md border bg-muted/40 px-2.5 text-sm text-muted-foreground transition-colors hover:bg-muted lg:flex dark:bg-muted/30 [&_svg]:size-4"
      >
        <SearchIcon />
        Search docs…
        <kbd class="ml-auto rounded border bg-background px-1.5 font-sans text-[11px]">{mac() ? "⌘K" : "Ctrl K"}</kbd>
      </button>
      <button type="button" onClick={show} class="icon-button lg:hidden" aria-label="Search docs">
        <SearchIcon />
      </button>
      <dialog
        ref={dialog}
        class="search-dialog"
        aria-label="Search the docs"
        onClose={() => setQuery("")}
        onClick={(e) => e.target === dialog && dialog.close()}
      >
        <div class="flex items-center gap-2 border-b px-3.5 [&_svg]:size-4 [&_svg]:text-muted-foreground">
          <SearchIcon />
          <input
            ref={input}
            type="search"
            value={query()}
            onInput={(e) => setQuery(e.currentTarget.value)}
            onKeyDown={onInputKeyDown}
            placeholder="Search the docs…"
            aria-label="Search the docs"
            aria-controls="search-results"
            aria-activedescendant={results().length ? `search-result-${selected()}` : undefined}
            role="combobox"
            aria-expanded="true"
            autocomplete="off"
            spellcheck={false}
            class="h-12 flex-1 bg-transparent text-[15px] outline-none placeholder:text-muted-foreground [&::-webkit-search-cancel-button]:hidden"
          />
          <kbd class="rounded border px-1.5 font-sans text-[11px] text-muted-foreground">Esc</kbd>
        </div>
        <ul ref={list} id="search-results" role="listbox" class="max-h-[min(60vh,26rem)] overflow-y-auto p-1.5">
          <Show
            when={!failed()}
            fallback={<li class="py-10 text-center text-sm text-muted-foreground">The search index could not be loaded.</li>}
          >
            <Show when={entries()} fallback={<li class="py-10 text-center text-sm text-muted-foreground">Loading…</li>}>
              <Show when={results().length} fallback={<li class="py-10 text-center text-sm text-muted-foreground">No results for “{query()}”</li>}>
                <For each={results()}>
                  {(entry, i) => (
                    <li
                      id={`search-result-${i()}`}
                      data-index={i()}
                      role="option"
                      aria-selected={selected() === i()}
                      onClick={() => go(entry)}
                      onPointerMove={() => setSelected(i())}
                      class="flex cursor-pointer items-start gap-3 rounded-md px-2.5 py-2 text-sm aria-selected:bg-muted [&_svg]:mt-0.5 [&_svg]:size-4 [&_svg]:shrink-0 [&_svg]:text-muted-foreground"
                    >
                      {entry.id ? <HashIcon /> : <FileTextIcon />}
                      <div class="min-w-0 flex-1">
                        <div class="truncate font-medium">
                          <Highlight text={entry.heading} terms={terms()} />
                          <Show when={entry.id && terms().length}>
                            <span class="ml-2 font-normal text-muted-foreground">{entry.page}</span>
                          </Show>
                        </div>
                        <Show when={terms().length > 0 && entry.text}>
                          <div class="mt-0.5 line-clamp-2 text-xs leading-5 text-muted-foreground">
                            <Highlight text={snippet(entry.text, terms())} terms={terms()} />
                          </div>
                        </Show>
                      </div>
                    </li>
                  )}
                </For>
              </Show>
            </Show>
          </Show>
        </ul>
      </dialog>
    </>
  )
}
