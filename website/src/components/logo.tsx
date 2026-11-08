import { splitProps, type JSX } from "solid-js"

export function Wordmark(props: JSX.HTMLAttributes<HTMLSpanElement>) {
  const [local, rest] = splitProps(props, ["class"])
  return (
    <span class={`font-semibold tracking-tight ${local.class ?? ""}`} {...rest}>
      Gojo
    </span>
  )
}
