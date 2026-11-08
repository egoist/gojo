import { useNavigate } from "@tanstack/solid-router"

/** A page rendered from markdown: its links navigate within the site, and its code blocks copy. */
export function DocContent(props: { html: string }) {
  const navigate = useNavigate()

  function onClick(e: MouseEvent) {
    const target = e.target as Element
    const copy = target.closest<HTMLButtonElement>(".copy-button")
    if (copy) {
      const code = copy.parentElement?.querySelector("pre")?.textContent ?? ""
      void navigator.clipboard.writeText(code).then(() => {
        copy.dataset.copied = ""
        setTimeout(() => delete copy.dataset.copied, 1500)
      })
      return
    }
    const a = target.closest("a")
    if (!a || e.defaultPrevented || e.button !== 0 || e.metaKey || e.ctrlKey || e.shiftKey || e.altKey) return
    const url = new URL(a.href)
    if (url.origin !== location.origin || a.target || /\.(md|txt|json)$/.test(url.pathname)) return
    e.preventDefault()
    void navigate({ href: url.pathname + url.search + url.hash })
  }

  return <div class="prose-docs" onClick={onClick} innerHTML={props.html} />
}
