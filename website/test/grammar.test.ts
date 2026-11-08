// The Gojo grammar against the compiler: the same keywords as the lexer, and
// every Gojo program of the repository highlighted without an invalid token
// or a string, comment or bracket left open.

import { describe, expect, test } from "bun:test"
import fs from "node:fs"
import path from "node:path"
import { createHighlighterCore } from "shiki/core"
import { createOnigurumaEngine } from "shiki/engine/oniguruma"

import { contextual, gojoGrammar, keywords } from "../src/lib/gojo-grammar.ts"

const repo = path.resolve(import.meta.dir, "../..")
const highlighter = await createHighlighterCore({
  themes: [import("shiki/themes/github-light-default.mjs")],
  langs: [gojoGrammar],
  engine: createOnigurumaEngine(import("shiki/wasm")),
})

function files(dir: string, ext: string): string[] {
  return fs
    .readdirSync(path.join(repo, dir), { recursive: true, encoding: "utf8" })
    .filter((f) => f.endsWith(ext))
    .sort()
    .map((f) => path.join(dir, f))
}

/** The ```gojo blocks of a markdown file. */
function codeBlocks(file: string) {
  const source = fs.readFileSync(path.join(repo, file), "utf8")
  return [...source.matchAll(/^```gojo\n([\s\S]*?)^```$/gm)].map((m) => m[1]!)
}

/** Each token of the code, with the scope of the innermost rule that matched it. */
function tokens(code: string) {
  return highlighter
    .codeToTokensBase(code, { lang: "gojo", theme: "github-light-default", includeExplanation: true })
    .flat()
    .flatMap((t) => t.explanation!.map((e) => ({ text: e.content, scopes: e.scopes.map((s) => s.scopeName) })))
}

function scopeOf(code: string, text: string) {
  const token = tokens(code).find((t) => t.text.trim() === text)
  if (!token) throw new Error(`no token ${JSON.stringify(text)} in ${JSON.stringify(code)}`)
  return token.scopes.at(-1)
}

/** Problems with the highlighting of a program: invalid tokens, and rules still open at its end. */
function problems(code: string) {
  const out: string[] = []
  for (const t of tokens(code)) {
    if (t.scopes.some((s) => s.startsWith("invalid"))) out.push(`invalid token ${JSON.stringify(t.text)}`)
  }
  const open = highlighter.getLastGrammarState(code + "\n", { lang: "gojo", theme: "github-light-default" }).getScopes()
  if (open && open.join(" ") !== "source.gojo") out.push(`left open: ${open.join(" ")}`)
  return out
}

describe("keywords", () => {
  test("are the lexer's", () => {
    const lexer = fs.readFileSync(path.join(repo, "src/lexer.rs"), "utf8")
    const fn = /fn keyword\(s: &str\)[\s\S]*?\n}\n/.exec(lexer)![0]
    const fromLexer = [...fn.matchAll(/"([a-z]+)" =>/g)].map((m) => m[1]!).sort()
    expect(Object.values(keywords).flat().map(String).sort()).toEqual(fromLexer)
  })

  test("contextual keywords are not reserved", () => {
    const all: string[] = Object.values(keywords).flat()
    for (const word of contextual) expect(all).not.toContain(word)
  })
})

describe("highlights", () => {
  const cases: [string, string, string][] = [
    ["fn main() {}", "fn", "storage.type.function.gojo"],
    ["fn main() {}", "main", "entity.name.function.gojo"],
    ["struct Config { port: int = 8080 }", "Config", "entity.name.type.gojo"],
    ["struct Config {\n    port: int = 8080\n}", "port", "variable.other.property.gojo"],
    ["struct Config { port: int = 8080 }", "int", "support.type.primitive.gojo"],
    ["pub unsafe send struct Atomic[T: Integer] {}", "send", "storage.modifier.gojo"],
    ["fn bump(inout self, by: int) {}", "inout", "storage.modifier.gojo"],
    ["fn bump(inout self, by: int) {}", "self", "variable.language.self.gojo"],
    ["fn bump(inout self, by: int) {}", "by", "variable.parameter.gojo"],
    ["fn load(path: str) -> Config ! os.Error {}", "->", "keyword.operator.arrow.gojo"],
    ["fn load(path: str) -> Config ! os.Error {}", "!", "keyword.operator.error.gojo"],
    ["let text = try os.readText(path)", "try", "keyword.control.exception.gojo"],
    ["let text = try os.readText(path)", "text", "variable.other.declaration.gojo"],
    ["let text = try os.readText(path)", "readText", "entity.name.function.call.gojo"],
    ["let v = list.map[int](f)", "map", "entity.name.function.call.gojo"],
    ["println(cfg.port)", "port", "variable.other.property.gojo"],
    ["let p = Config{name: n}", "name", "variable.other.property.gojo"],
    ["use std/{fmt, os}", "use", "keyword.control.import.gojo"],
    ["use std/{fmt, os}", "fmt", "entity.name.namespace.gojo"],
    ["use ./geom.{Vec2}", "Vec2", "entity.name.type.gojo"],
    ["use ../shared/util as u", "as", "keyword.control.as.gojo"],
    ['let s = "a\\tb"', "\\t", "constant.character.escape.gojo"],
    ['let s = "${n:>8}"', ">8", "constant.other.format-spec.gojo"],
    ['let s = "${n:>8}"', "${", "punctuation.section.interpolation.begin.gojo"],
    ['let s = "${m["k"]}"', "k", "string.quoted.double.gojo"],
    ['let s = "${p.{x: 1}.x}"', "}", "punctuation.definition.block.end.gojo"],
    ["let raw = `a\\nb`", "a\\nb", "string.quoted.other.raw.gojo"],
    ["let r = '\\n'", "\\n", "constant.character.escape.gojo"],
    ["let x = 0x_ff + 0b1010 + 0o17", "0x_ff", "constant.numeric.integer.hexadecimal.gojo"],
    ["let x = 2.5e3", "2.5e3", "constant.numeric.float.gojo"],
    ["for i in 0..10 {}", "..", "keyword.operator.range.gojo"],
    ["for i in 0..10 {}", "10", "constant.numeric.integer.decimal.gojo"],
    ["let a = t.0.1", "1", "constant.numeric.integer.decimal.gojo"],
    ["let n = a +% b", "+%", "keyword.operator.arithmetic.wrapping.gojo"],
    ["let name = cfg.proxy ?? none", "??", "keyword.operator.optional.gojo"],
    ["let name = cfg.proxy ?? none", "none", "constant.language.gojo"],
    ["let v = <-ch", "<-", "keyword.operator.channel.gojo"],
    ["scope s { s.go(work) }", "scope", "keyword.control.concurrency.gojo"],
    ["outer: for x in xs { continue outer }", "outer", "entity.name.label.gojo"],
    ['extern "C" lib "libz" {}', "lib", "storage.modifier.gojo"],
    ['test "doubling" {}', "test", "storage.type.test.gojo"],
    ["let ys = xs.map(x => x * 2)", "x", "variable.parameter.gojo"],
    ["// a comment", "a comment", "comment.line.double-slash.gojo"],
    ["/* a\ncomment */ fn f() {}", "fn", "storage.type.function.gojo"],
  ]
  for (const [code, text, scope] of cases) {
    test(`${JSON.stringify(text)} in ${JSON.stringify(code)}`, () => {
      expect(scopeOf(code, text)).toBe(scope)
    })
  }

  test("an unknown escape is invalid", () => {
    expect(problems('let s = "\\q"')).toEqual(['invalid token "\\\\q"'])
  })

  test("an unclosed block is reported", () => {
    expect(problems("fn main() {")).not.toEqual([])
  })
})

describe("the repository's Gojo", () => {
  const programs = [...files("std", ".gojo"), ...files("examples", ".gojo"), ...files("tests/run", ".gojo")]
  for (const file of programs) {
    test(file, () => {
      expect(problems(fs.readFileSync(path.join(repo, file), "utf8"))).toEqual([])
    })
  }

  for (const file of ["README.md", ...files("docs", ".md")]) {
    const blocks = codeBlocks(file)
    if (!blocks.length) continue
    test(`${file} (${blocks.length} blocks)`, () => {
      const found = blocks.flatMap((code, i) => problems(code).map((p) => `block ${i + 1}: ${p}\n${code}`))
      expect(found).toEqual([])
    })
  }
})
