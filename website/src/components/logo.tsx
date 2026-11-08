import { splitProps, type JSX } from "solid-js"

// The mark: a G whose gap is closed by a vermillion seal. A seal marks what
// is owned, and the circle it completes has nothing missing: ownership, and
// no nil. public/favicon.svg is the same drawing.

/** The G: one stroke, the ring and its bar. */
export const gPath = "M19.17 8.44A8.2 8.2 0 1 0 24.2 16H16.1"

/** The seal, centered on the ring in the gap, square to it. */
export function Seal(props: JSX.RectSVGAttributes<SVGRectElement>) {
  return <rect x="19.95" y="8.38" width="4.4" height="4.4" rx="0.7" transform="rotate(-41.42 22.15 10.58)" {...props} />
}

/** The mark on its paper tile. */
export function Mark(props: JSX.SvgSVGAttributes<SVGSVGElement>) {
  return (
    <svg viewBox="0 0 32 32" aria-hidden="true" {...props}>
      <rect x="0.3" y="0.3" width="31.4" height="31.4" rx="7.5" fill="#F5F0E6" stroke="#17140F" stroke-opacity="0.14" stroke-width="0.6" />
      <path d={gPath} fill="none" stroke="#17140F" stroke-width="4.4" />
      <Seal fill="#F0532A" />
    </svg>
  )
}

export function Wordmark(props: JSX.HTMLAttributes<HTMLSpanElement>) {
  const [local, rest] = splitProps(props, ["class"])
  return (
    <span class={`inline-flex items-center gap-2 font-semibold tracking-tight ${local.class ?? ""}`} {...rest}>
      <Mark class="size-6" />
      Gojo
    </span>
  )
}
