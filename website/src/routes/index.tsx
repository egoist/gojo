import { Link, createFileRoute } from "@tanstack/solid-router"
import ArrowRightIcon from "lucide-solid/icons/arrow-right"
import { For, Show, createMemo, createSignal } from "solid-js"

import { InstallCommand } from "@/components/install-command"
import { getHome } from "@/lib/home"
import { site } from "@/lib/site"
import { button } from "@/lib/ui"

export const Route = createFileRoute("/")({
  loader: () => getHome(),
  component: Home,
})

// What Gojo leaves out, and what it makes.
const facts = [
  { value: "None", label: "Garbage collector" },
  { value: "None", label: "Null pointers" },
  { value: "None", label: "Lifetime annotations" },
  { value: "C", label: "Compiles to, then native" },
]

// The README and the tour, in short.
const features = [
  {
    title: "Ownership without lifetimes",
    text: "Memory is freed by ownership, as in Rust, but references can't be stored, only used, so the compiler checks them without annotations.",
    href: "/docs/tour#ownership",
  },
  {
    title: "Errors are values",
    text: "A function that can fail returns T ! E. try passes the error up, catch handles it, errdefer cleans up; none can be ignored by accident.",
    href: "/docs/tour#errors",
  },
  {
    title: "Structured concurrency",
    text: "go, channels and select, made structured: every task belongs to a scope that waits for it, with cancellation down the tree. Data races don't compile.",
    href: "/docs/tour#concurrency",
  },
  {
    title: "Generics and interfaces",
    text: "Generic types and functions with bounds, and interfaces a type satisfies by having the methods, without declaring it.",
    href: "/docs/tour#generics-and-interfaces",
  },
  {
    title: "Tools in one binary",
    text: "gojo run, build, test, fmt and doc, with the standard library and the runtime compiled in. Cross-compile static Linux binaries with zig.",
    href: "/docs/tour#getting-started",
  },
  {
    title: "Made for coding agents",
    text: "Diagnostics say what to write instead, gojo doc prints any module's API, and these docs are one file away at /llms.txt.",
    href: "/docs/std",
  },
]

function Home() {
  const home = Route.useLoaderData()
  const [tab, setTab] = createSignal("glance")
  const sample = createMemo(() => home().samples.find((s) => s.id === tab()) ?? home().samples[0]!)

  return (
    <main>
      <section class="relative overflow-hidden px-4 py-20 sm:px-10 md:py-28">
        <div class="relative">
          <a href={`${site.repo}/releases`} class="label inline-flex items-center gap-2 hover:text-foreground">
            <span class="size-1.5 rounded-full bg-brand" />v{__GOJO_VERSION__} beta · macOS, Linux
          </a>
          <h1 class="mt-6 max-w-3xl text-4xl leading-[1.05] font-semibold tracking-[-0.04em] sm:text-5xl md:text-6xl">
            Memory-safe systems code,
            <span class="block text-muted-foreground">for you and your agents.</span>
          </h1>
          <p class="mt-6 max-w-xl text-lg leading-8 text-muted-foreground">
            Ownership frees memory and rules out data races, with no lifetimes to annotate. Errors are values, every task lives in a scope, and it all
            compiles fast to C, then to one native binary.
          </p>
          <div class="mt-10 flex flex-wrap items-center gap-3">
            <InstallCommand command={site.install} />
            <Link to="/docs/$" params={{ _splat: "tour" }} class={`${button.primary} h-11 px-5`}>
              Take the tour <ArrowRightIcon />
            </Link>
            <Link to="/docs/$" params={{ _splat: "why-gojo" }} class={`${button.outline} h-11 px-5`}>
              Why Gojo?
            </Link>
          </div>
        </div>
      </section>

      <dl class="grid grid-cols-2 gap-px border-t bg-border md:grid-cols-4">
        <For each={facts}>
          {(fact) => (
            <div class="flex flex-col-reverse justify-end gap-2 bg-background px-4 py-6 sm:px-10 sm:py-8">
              <dt class="label">{fact.label}</dt>
              <dd class="text-3xl font-semibold tracking-tight sm:text-4xl">{fact.value}</dd>
            </div>
          )}
        </For>
      </dl>

      <div class="border-t bg-code">
        <div role="tablist" aria-label="Programs" class="flex overflow-x-auto border-b">
          <For each={home().samples}>
            {(s) => (
              <button
                type="button"
                role="tab"
                id={`tab-${s.id}`}
                aria-selected={tab() === s.id}
                aria-controls="code-panel"
                onClick={() => setTab(s.id)}
                class="label -mb-px shrink-0 border-r border-b border-b-transparent px-4 py-3 transition-colors hover:text-foreground aria-selected:border-b-brand aria-selected:bg-background aria-selected:text-foreground sm:px-6"
              >
                {s.label}
              </button>
            )}
          </For>
        </div>
        <div role="tabpanel" id="code-panel" aria-labelledby={`tab-${sample().id}`} class="grid lg:grid-cols-[1.4fr_1fr] lg:divide-x">
          <figure class="min-w-0">
            <figcaption class="label border-b px-4 py-3 sm:px-6">main.gojo</figcaption>
            <div class="overflow-x-auto px-4 py-5 sm:px-6" innerHTML={sample().html} />
          </figure>
          <div class="flex min-w-0 flex-col border-t lg:border-t-0">
            <p class="px-4 py-5 text-sm leading-6 text-muted-foreground sm:px-6">{sample().blurb}</p>
            <Show when={sample().output}>
              <figure class="border-t">
                <figcaption class="label border-b px-4 py-3 sm:px-6">
                  <span class="text-brand-ink select-none">$ </span>gojo run main.gojo
                </figcaption>
                <pre class="overflow-x-auto px-4 py-4 font-mono text-[13px] leading-[1.7] sm:px-6">{sample().output}</pre>
              </figure>
            </Show>
            <a href={sample().href} class="group mt-auto flex items-center gap-2 border-t px-4 py-4 text-sm font-medium transition-colors hover:bg-background sm:px-6">
              Read it in the tour
              <ArrowRightIcon class="size-4 text-muted-foreground transition-transform group-hover:translate-x-0.5 group-hover:text-foreground" />
            </a>
          </div>
        </div>
      </div>

      <ul class="grid gap-px border-t bg-border sm:grid-cols-2">
        <For each={features}>
          {(feature) => (
            <li class="bg-background">
              <a href={feature.href} class="group flex h-full flex-col px-4 py-8 transition-colors hover:bg-code sm:px-10">
                <h2 class="flex items-center justify-between font-semibold tracking-tight">
                  {feature.title}
                  <ArrowRightIcon class="size-4 text-muted-foreground transition-transform group-hover:translate-x-0.5 group-hover:text-foreground" />
                </h2>
                <p class="mt-2 max-w-md text-sm leading-6 text-muted-foreground">{feature.text}</p>
              </a>
            </li>
          )}
        </For>
      </ul>
    </main>
  )
}
