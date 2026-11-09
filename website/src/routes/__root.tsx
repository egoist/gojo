import { HeadContent, Outlet, Scripts, createRootRoute } from "@tanstack/solid-router"
import { Suspense, type JSX } from "solid-js"
import { HydrationScript } from "solid-js/web"

import { NotFound } from "@/components/not-found"
import { SiteFooter } from "@/components/site-footer"
import { SiteHeader } from "@/components/site-header"
import { site } from "@/lib/site"
import { themeScript } from "@/lib/theme"
import appCss from "@/styles.css?url"

export const Route = createRootRoute({
  head: () => ({
    meta: [
      { charSet: "utf-8" },
      { name: "viewport", content: "width=device-width, initial-scale=1" },
      { title: site.title },
      { name: "description", content: site.description },
      { property: "og:type", content: "website" },
      { property: "og:site_name", content: site.name },
      { property: "og:title", content: site.title },
      { property: "og:description", content: site.description },
      { name: "twitter:card", content: "summary" },
    ],
    links: [
      { rel: "stylesheet", href: appCss },
    ],
    scripts: [
      { children: themeScript },
      // Analytics only in production builds, never in local development.
      ...(import.meta.env.PROD
        ? [
            {
              src: "https://u.egoist.dev/script.js",
              defer: true,
              "data-website-id": "bd622d42-4751-4cc6-8697-953da87fd050",
            },
          ]
        : []),
    ],
  }),
  shellComponent: RootDocument,
  component: RootLayout,
  notFoundComponent: NotFound,
})

function RootDocument(props: { children: JSX.Element }) {
  return (
    <html lang="en">
      <head>
        <HydrationScript />
      </head>
      <body>
        <HeadContent />
        <Suspense>{props.children}</Suspense>
        <Scripts />
      </body>
    </html>
  )
}

function RootLayout() {
  return (
    <div class="flex min-h-svh flex-col">
      <SiteHeader />
      <div class="frame flex flex-1 flex-col">
        <Outlet />
      </div>
      <SiteFooter />
    </div>
  )
}
