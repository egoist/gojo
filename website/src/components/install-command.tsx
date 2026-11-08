import CheckIcon from "lucide-solid/icons/check"
import CopyIcon from "lucide-solid/icons/copy"
import { Show, createSignal } from "solid-js"

export function InstallCommand(props: { command: string; class?: string }) {
  const [copied, setCopied] = createSignal(false)
  return (
    <button
      type="button"
      aria-label={`Copy "${props.command}"`}
      title={props.command}
      onClick={() =>
        void navigator.clipboard.writeText(props.command).then(() => {
          setCopied(true)
          setTimeout(() => setCopied(false), 1500)
        })
      }
      class={`group inline-flex h-11 max-w-full items-center gap-3 rounded-lg border bg-card pr-3 pl-4 font-mono text-[13px] transition-colors hover:border-foreground/25 ${props.class ?? ""}`}
    >
      <span class="text-brand-ink select-none">$</span>
      <span class="min-w-0 truncate">{props.command}</span>
      <span class="ml-2 shrink-0 text-muted-foreground transition-colors group-hover:text-foreground [&_svg]:size-4">
        <Show when={copied()} fallback={<CopyIcon />}>
          <CheckIcon />
        </Show>
      </span>
    </button>
  )
}
