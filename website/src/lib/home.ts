import { createServerFn } from "@tanstack/solid-start"
import { staticFunctionMiddleware } from "@tanstack/start-static-server-functions"

import { loadHome } from "@/lib/home.server"

export const getHome = createServerFn({ method: "GET" })
  .middleware([staticFunctionMiddleware])
  .handler(() => loadHome())
