import { createSignal, onCleanup, onMount } from "solid-js"

export type Theme = "light" | "dark" | "system"

const key = "theme"
const query = "(prefers-color-scheme: dark)"

/** Runs in <head> before the page paints, so a dark page never flashes light. */
export const themeScript = `try{var t=localStorage.getItem("${key}");document.documentElement.classList.toggle("dark",t==="dark"||(t!=="light"&&matchMedia("${query}").matches))}catch(e){}`

function stored(): Theme {
  try {
    const t = localStorage.getItem(key)
    return t === "light" || t === "dark" ? t : "system"
  } catch {
    return "system"
  }
}

function apply(theme: Theme) {
  const dark = theme === "dark" || (theme === "system" && matchMedia(query).matches)
  const root = document.documentElement
  if (root.classList.contains("dark") === dark) return
  // Switch colors at once, without every transition of the page running.
  const style = document.createElement("style")
  style.textContent = "*,*::before,*::after{transition:none!important}"
  document.head.append(style)
  root.classList.toggle("dark", dark)
  getComputedStyle(root).colorScheme
  requestAnimationFrame(() => style.remove())
}

// One signal for the page: "system" until it hydrates.
const [theme, setThemeSignal] = createSignal<Theme>("system")

export function setTheme(next: Theme) {
  try {
    if (next === "system") localStorage.removeItem(key)
    else localStorage.setItem(key, next)
  } catch {}
  apply(next)
  setThemeSignal(next)
}

/** The chosen theme, which follows the system's while it is "system". */
export function useTheme() {
  onMount(() => {
    setThemeSignal(stored())
    const media = matchMedia(query)
    const onChange = () => apply(stored())
    media.addEventListener("change", onChange)
    onCleanup(() => media.removeEventListener("change", onChange))
  })
  return theme
}
