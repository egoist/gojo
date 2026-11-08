import fs from "node:fs"
import path from "node:path"
import tailwindcss from "@tailwindcss/vite"
import { tanstackStart } from "@tanstack/solid-start/plugin/vite"
import { defineConfig } from "vite"
import viteSolid from "vite-plugin-solid"
import { llmsDocs } from "./llms.ts"

const repo = (file: string) => new URL(`../${file}`, import.meta.url)

// The pages of the repository's docs, std/os.md at /docs/std/os, and a
// directory's README at the directory, std/README.md at /docs/std. /docs
// itself, like every route without parameters, is found without being listed.
const docs = fs
  .readdirSync(repo("docs"), { recursive: true, encoding: "utf8" })
  .filter((file) => file.endsWith(".md") && file !== "README.md")
  .map((file) => `/docs/${file.slice(0, -3).split(path.sep).join("/").replace(/\/README$/, "")}`)

const version = /^version = "([^"]+)"/m.exec(fs.readFileSync(repo("Cargo.toml"), "utf8"))?.[1]

export default defineConfig({
  define: {
    __GOJO_VERSION__: JSON.stringify(version),
  },
  resolve: { tsconfigPaths: true },
  plugins: [
    llmsDocs(path.resolve(import.meta.dirname, "..")),
    tailwindcss(),
    tanstackStart({
      // A static site: every page is rendered to HTML at build time.
      prerender: { enabled: true, crawlLinks: true, filter: ({ path }) => !path.includes("#") },
      pages: [
        ...docs.map((path) => ({ path })),
        { path: "/docs/search.json" },
        // Served by Cloudflare for any path that has no page.
        { path: "/404", prerender: { enabled: true, outputPath: "/404.html" } },
      ],
    }),
    viteSolid({ ssr: true }),
  ],
})
