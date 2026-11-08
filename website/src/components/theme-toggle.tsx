import CheckIcon from "lucide-solid/icons/check"
import MonitorIcon from "lucide-solid/icons/monitor"
import MoonIcon from "lucide-solid/icons/moon"
import SunIcon from "lucide-solid/icons/sun"
import { For, Show, createSignal, onCleanup, onMount } from "solid-js"
import { Dynamic } from "solid-js/web"

import { setTheme, useTheme, type Theme } from "@/lib/theme"

const options: { value: Theme; label: string; icon: typeof SunIcon }[] = [
  { value: "light", label: "Light", icon: SunIcon },
  { value: "dark", label: "Dark", icon: MoonIcon },
  { value: "system", label: "System", icon: MonitorIcon },
]

/** A menu of the themes: light, dark, or the system's. */
export function ThemeToggle() {
  const theme = useTheme()
  const [open, setOpen] = createSignal(false)
  let root!: HTMLDivElement
  let trigger!: HTMLButtonElement
  let menu: HTMLDivElement | undefined

  const items = () => [...(menu?.querySelectorAll<HTMLButtonElement>("[role=menuitemradio]") ?? [])]

  function show() {
    setOpen(true)
    // Focus the chosen theme, as a native menu does.
    queueMicrotask(() => (items().find((i) => i.getAttribute("aria-checked") === "true") ?? items()[0])?.focus())
  }

  function close(refocus = true) {
    setOpen(false)
    if (refocus) trigger.focus()
  }

  function choose(value: Theme) {
    setTheme(value)
    close()
  }

  function onMenuKeyDown(e: KeyboardEvent) {
    const list = items()
    const at = list.indexOf(document.activeElement as HTMLButtonElement)
    if (e.key === "ArrowDown") list[(at + 1) % list.length]?.focus()
    else if (e.key === "ArrowUp") list[(at - 1 + list.length) % list.length]?.focus()
    else if (e.key === "Home") list[0]?.focus()
    else if (e.key === "End") list.at(-1)?.focus()
    else if (e.key === "Escape") close()
    else if (e.key === "Tab") close(false)
    else return
    e.preventDefault()
  }

  onMount(() => {
    const onPointerDown = (e: PointerEvent) => {
      if (open() && !root.contains(e.target as Node)) close(false)
    }
    document.addEventListener("pointerdown", onPointerDown)
    onCleanup(() => document.removeEventListener("pointerdown", onPointerDown))
  })

  return (
    <div ref={root} class="relative">
      <button
        ref={trigger}
        type="button"
        class="icon-button"
        aria-label="Theme"
        aria-haspopup="menu"
        aria-expanded={open()}
        onClick={() => (open() ? close() : show())}
        onKeyDown={(e) => {
          if (e.key === "ArrowDown" && !open()) {
            e.preventDefault()
            show()
          }
        }}
      >
        {/* Chosen by CSS, which knows the theme before the page hydrates. */}
        <SunIcon class="dark:hidden" />
        <MoonIcon class="hidden dark:block" />
      </button>
      <Show when={open()}>
        <div
          ref={menu}
          role="menu"
          aria-label="Theme"
          onKeyDown={onMenuKeyDown}
          class="absolute top-full right-0 z-50 mt-1.5 w-36 rounded-lg border bg-popover p-1 shadow-lg"
        >
          <For each={options}>
            {(option) => (
              <button
                type="button"
                role="menuitemradio"
                aria-checked={theme() === option.value}
                onClick={() => choose(option.value)}
                class="flex w-full items-center gap-2 rounded-md px-2 py-1.5 text-sm text-foreground outline-none hover:bg-muted focus-visible:bg-muted [&_svg]:size-4 [&_svg]:text-muted-foreground"
              >
                <Dynamic component={option.icon} />
                {option.label}
                <Show when={theme() === option.value}>
                  <CheckIcon class="ml-auto text-foreground!" />
                </Show>
              </button>
            )}
          </For>
        </div>
      </Show>
    </div>
  )
}
