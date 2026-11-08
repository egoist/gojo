const base =
  "inline-flex h-10 items-center justify-center gap-2 rounded-lg px-4 text-sm font-medium whitespace-nowrap transition-colors outline-none focus-visible:ring-3 focus-visible:ring-ring/50 [&_svg]:size-4"

/** The classes of the site's buttons, for buttons and links alike. */
export const button = {
  primary: `${base} bg-primary text-primary-foreground hover:bg-primary/85`,
  outline: `${base} border bg-background hover:bg-muted`,
}
