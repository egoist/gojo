import MonitorIcon from "lucide-solid/icons/monitor"
import MoonIcon from "lucide-solid/icons/moon"
import SunIcon from "lucide-solid/icons/sun"
import { Match, Switch } from "solid-js"

import { setTheme, useTheme, type Theme } from "@/lib/theme"

const order: Theme[] = ["system", "light", "dark"]
const names: Record<Theme, string> = { system: "System", light: "Light", dark: "Dark" }

/** Cycles through the system's theme, light and dark. */
export function ThemeToggle() {
  const theme = useTheme()
  const next = () => order[(order.indexOf(theme()) + 1) % order.length]!
  return (
    <button
      type="button"
      class="icon-button"
      aria-label={`Theme: ${names[theme()]}. Switch to ${names[next()]}`}
      title={`Theme: ${names[theme()]}`}
      onClick={() => setTheme(next())}
    >
      <Switch>
        <Match when={theme() === "light"}>
          <SunIcon />
        </Match>
        <Match when={theme() === "dark"}>
          <MoonIcon />
        </Match>
        <Match when={theme() === "system"}>
          <MonitorIcon />
        </Match>
      </Switch>
    </button>
  )
}
