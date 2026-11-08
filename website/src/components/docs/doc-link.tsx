import { Link } from "@tanstack/solid-router"
import { splitProps, type JSX } from "solid-js"

type Props = { slug: string; class?: string; activeClass?: string; onClick?: () => void; children: JSX.Element }

/**
 * A link to a page of the docs by its slug, "" for the index. Only the page
 * itself is active: /docs/std is not when /docs/std/os is.
 */
export function DocLink(props: Props) {
  const [local, rest] = splitProps(props, ["slug", "activeClass"])
  return local.slug ? (
    <Link to="/docs/$" params={{ _splat: local.slug }} activeOptions={{ exact: true }} activeProps={{ class: local.activeClass ?? "" }} {...rest} />
  ) : (
    <Link to="/docs" activeOptions={{ exact: true }} activeProps={{ class: local.activeClass ?? "" }} {...rest} />
  )
}
