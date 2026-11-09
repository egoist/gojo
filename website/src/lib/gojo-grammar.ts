// A TextMate grammar for Gojo, which Shiki uses to highlight the code of the
// site. It follows the lexer, src/lexer.rs: test/grammar.test.ts checks the
// keywords against it and highlights every Gojo program of the repository.

import type { LanguageRegistration } from "shiki"

/** The keywords of src/lexer.rs, by what they do. */
export const keywords = {
  control: ["if", "else", "for", "in", "while", "loop", "match", "return", "break", "continue", "yield"],
  errors: ["try", "catch", "defer", "errdefer"],
  concurrency: ["go", "select", "scope", "isolate"],
  declarations: ["fn", "struct", "enum", "interface", "type", "let", "var", "const", "extend", "test"],
  modifiers: ["pub", "copy", "inout", "sink", "unsafe", "extern"],
  other: ["use", "with", "where"],
  constants: ["true", "false", "none"],
} as const

/** Contextual keywords: names everywhere else. */
export const contextual = ["self", "as", "lib", "static", "send", "view", "of"] as const

/** The types the checker knows by name (builtin_type and the generic builtins). */
export const primitiveTypes = [
  "int", "uint", "i8", "i16", "i32", "i64", "u8", "u16", "u32", "u64",
  "f32", "f64", "bool", "str", "rune", "byte", "void", "never",
] as const

const words = (list: readonly string[]) => `\\b(?:${list.join("|")})\\b`

const ident = String.raw`[A-Za-z_\x{80}-\x{10FFFF}][A-Za-z0-9_\x{80}-\x{10FFFF}]*`

const escape = String.raw`\\(?:[ntr0\\"'$]|x[0-9A-Fa-f]{2}|u\{[0-9A-Fa-f]{1,6}\})`

export const gojoGrammar: LanguageRegistration = {
  name: "gojo",
  displayName: "Gojo",
  scopeName: "source.gojo",
  fileTypes: ["gojo"],
  patterns: [{ include: "#statements" }],
  repository: {
    statements: {
      patterns: [
        { include: "#comments" },
        { include: "#use" },
        { include: "#extern" },
        { include: "#label" },
        { include: "#fields" },
        { include: "#declarations" },
        { include: "#expressions" },
      ],
    },

    expressions: {
      patterns: [
        { include: "#comments" },
        { include: "#strings" },
        { include: "#runes" },
        { include: "#numbers" },
        { include: "#keywords" },
        { include: "#lambda" },
        { include: "#types" },
        { include: "#calls" },
        { include: "#members" },
        { include: "#operators" },
        { include: "#groups" },
        { include: "#punctuation" },
      ],
    },

    comments: {
      patterns: [
        {
          name: "comment.block.gojo",
          begin: String.raw`/\*`,
          end: String.raw`\*/`,
          captures: { 0: { name: "punctuation.definition.comment.gojo" } },
        },
        {
          name: "comment.line.double-slash.gojo",
          begin: "//",
          end: "$",
          beginCaptures: { 0: { name: "punctuation.definition.comment.gojo" } },
        },
      ],
    },

    // use std/{fmt, os}, use ./geom.{Vec2, Shape}, use ../shared/util as u
    use: {
      name: "meta.use.gojo",
      begin: String.raw`\b(use)\b`,
      beginCaptures: { 1: { name: "keyword.control.import.gojo" } },
      end: String.raw`(?=;|//|/\*)|$`,
      patterns: [
        { match: String.raw`\bas\b`, name: "keyword.control.as.gojo" },
        { match: String.raw`\.\.?(?=/)`, name: "entity.name.namespace.gojo" },
        { match: "/", name: "punctuation.separator.path.gojo" },
        { match: String.raw`\b[A-Z]\w*`, name: "entity.name.type.gojo" },
        { match: ident, name: "entity.name.namespace.gojo" },
        { match: String.raw`[{}]`, name: "punctuation.definition.block.gojo" },
        { match: ",", name: "punctuation.separator.comma.gojo" },
        { match: String.raw`\.`, name: "punctuation.accessor.gojo" },
      ],
    },

    // extern "C" { ... }, extern "C" static "m" { ... }, extern "C" lib "libz" { ... }
    extern: {
      match: String.raw`\b(extern)\s*("[^"]*")(?:\s*\b(lib|static)\b\s*("[^"]*"))?`,
      captures: {
        1: { name: "storage.modifier.extern.gojo" },
        2: { name: "string.quoted.double.gojo" },
        3: { name: "storage.modifier.gojo" },
        4: { name: "string.quoted.double.gojo" },
      },
    },

    // outer: for row in grid { ... continue outer }
    label: {
      patterns: [
        {
          match: String.raw`^\s*(${ident})\s*(:)(?=\s*(?:for|while|loop)\b)`,
          captures: { 1: { name: "entity.name.label.gojo" }, 2: { name: "punctuation.separator.label.gojo" } },
        },
        {
          match: String.raw`\b(break|continue)\s+(?!(?:if|else|true|false|none|self)\b)(${ident})`,
          captures: { 1: { name: "keyword.control.gojo" }, 2: { name: "entity.name.label.gojo" } },
        },
      ],
    },

    // name: Type in a struct, name: value in a literal, one per line.
    fields: {
      match: String.raw`^\s*(?:(pub)\s+)?(${ident})\s*(?=:(?!:))`,
      captures: { 1: { name: "storage.modifier.gojo" }, 2: { name: "variable.other.property.gojo" } },
    },

    declarations: {
      patterns: [
        {
          // fn name, fn name[T: Bound]
          match: String.raw`\b(fn)\s+(${ident})`,
          captures: { 1: { name: "storage.type.function.gojo" }, 2: { name: "entity.name.function.gojo" } },
        },
        {
          // A send struct is safe to move between tasks; a view struct holds
          // views and is one.
          match: String.raw`\b(send|view)\s+(?=(?:struct|enum)\b)`,
          captures: { 1: { name: "storage.modifier.gojo" } },
        },
        {
          // fn first(a: str, b: str) -> str of a { ... }
          match: String.raw`\b(of)\s+(?=${ident}(?:\s*,\s*${ident})*\s*(?:\{|\bwhere\b|$))`,
          captures: { 1: { name: "storage.modifier.gojo" } },
        },
        {
          match: String.raw`\b(struct|enum|interface)\s+(${ident})`,
          captures: { 1: { name: "storage.type.$1.gojo" }, 2: { name: "entity.name.type.gojo" } },
        },
        {
          match: String.raw`\b(type)\s+(${ident})`,
          captures: { 1: { name: "storage.type.type.gojo" }, 2: { name: "entity.name.type.gojo" } },
        },
        {
          match: String.raw`\b(extend)\s+(${ident})`,
          captures: { 1: { name: "storage.type.extend.gojo" }, 2: { name: "entity.name.type.gojo" } },
        },
        {
          match: String.raw`\b(const)\s+(${ident})`,
          captures: { 1: { name: "storage.type.const.gojo" }, 2: { name: "variable.other.constant.gojo" } },
        },
        {
          match: String.raw`\b(let|var)\s+(?!(?:${["if", "else", "self", "true", "false", "none"].join("|")})\b)(${ident})`,
          captures: { 1: { name: "storage.type.$1.gojo" }, 2: { name: "variable.other.declaration.gojo" } },
        },
        {
          match: String.raw`\b(test)\s+(?=")`,
          captures: { 1: { name: "storage.type.test.gojo" } },
        },
      ],
    },

    keywords: {
      patterns: [
        { match: words(keywords.control), name: "keyword.control.gojo" },
        { match: words(keywords.errors), name: "keyword.control.exception.gojo" },
        { match: words(keywords.concurrency), name: "keyword.control.concurrency.gojo" },
        { match: String.raw`\bfn\b`, name: "storage.type.function.gojo" },
        { match: words(keywords.declarations.filter((k) => k !== "fn")), name: "storage.type.gojo" },
        { match: words(keywords.modifiers), name: "storage.modifier.gojo" },
        { match: words(keywords.other), name: "keyword.other.gojo" },
        { match: String.raw`\bas\b`, name: "keyword.operator.as.gojo" },
        { match: words(keywords.constants), name: "constant.language.gojo" },
        { match: String.raw`\bself\b`, name: "variable.language.self.gojo" },
      ],
    },

    // x => x * 2, (a, b) => a + b
    lambda: {
      match: String.raw`\b(${ident})\s*(?==>)`,
      captures: { 1: { name: "variable.parameter.gojo" } },
    },

    types: {
      patterns: [
        { match: words(primitiveTypes), name: "support.type.primitive.gojo" },
        { match: String.raw`\b[A-Z][A-Za-z0-9_]*\b`, name: "entity.name.type.gojo" },
      ],
    },

    calls: {
      // f(x), obj.method(x), list.map[int](f)
      match: String.raw`\b(${ident})(?=\s*(?:\[[^\[\]]*\])?\()`,
      captures: { 1: { name: "entity.name.function.call.gojo" } },
    },

    members: {
      patterns: [
        {
          // t.0.1: elements of a tuple
          match: String.raw`(?<=\.)[0-9]+`,
          name: "constant.numeric.integer.decimal.gojo",
        },
        {
          // cfg.name, .field on a line of its own
          match: String.raw`(?<=[^.]\.|^\.)\s*(${ident})`,
          captures: { 1: { name: "variable.other.property.gojo" } },
        },
      ],
    },

    strings: {
      patterns: [
        {
          name: "string.quoted.double.gojo",
          begin: '"',
          end: String.raw`"|$`,
          beginCaptures: { 0: { name: "punctuation.definition.string.begin.gojo" } },
          endCaptures: { 0: { name: "punctuation.definition.string.end.gojo" } },
          patterns: [{ include: "#escapes" }, { include: "#interpolation" }],
        },
        {
          // Raw: no escapes, no interpolation, may span lines.
          name: "string.quoted.other.raw.gojo",
          begin: "`",
          end: "`",
          beginCaptures: { 0: { name: "punctuation.definition.string.begin.gojo" } },
          endCaptures: { 0: { name: "punctuation.definition.string.end.gojo" } },
        },
      ],
    },

    escapes: {
      patterns: [
        { match: escape, name: "constant.character.escape.gojo" },
        { match: String.raw`\\.?`, name: "invalid.illegal.unknown-escape.gojo" },
      ],
    },

    // "${n}", "${n:>8}": an expression, then perhaps a format of fmt.format,
    // which starts at a ':' outside brackets.
    interpolation: {
      name: "meta.interpolation.gojo",
      begin: String.raw`\$\{`,
      end: String.raw`\}`,
      beginCaptures: { 0: { name: "punctuation.section.interpolation.begin.gojo" } },
      endCaptures: { 0: { name: "punctuation.section.interpolation.end.gojo" } },
      contentName: "source.gojo.embedded",
      patterns: [
        {
          match: String.raw`(:)([^}"]*)(?=\})`,
          captures: { 1: { name: "punctuation.separator.format.gojo" }, 2: { name: "constant.other.format-spec.gojo" } },
        },
        { include: "#expressions" },
      ],
    },

    runes: {
      match: String.raw`(')(?:(${escape})|([^'\\]))(')`,
      captures: {
        1: { name: "punctuation.definition.string.begin.gojo" },
        2: { name: "constant.character.escape.gojo" },
        3: { name: "constant.character.gojo" },
        4: { name: "punctuation.definition.string.end.gojo" },
      },
      name: "string.quoted.single.gojo",
    },

    numbers: {
      patterns: [
        { match: String.raw`\b0x[0-9A-Fa-f_]+\b`, name: "constant.numeric.integer.hexadecimal.gojo" },
        { match: String.raw`\b0b[01_]+\b`, name: "constant.numeric.integer.binary.gojo" },
        { match: String.raw`\b0o[0-7_]+\b`, name: "constant.numeric.integer.octal.gojo" },
        {
          match: String.raw`(?<!\.)\b[0-9][0-9_]*(?:\.[0-9][0-9_]*(?:[eE][+-]?[0-9]+)?|[eE][+-]?[0-9]+)\b`,
          name: "constant.numeric.float.gojo",
        },
        { match: String.raw`\b[0-9][0-9_]*\b`, name: "constant.numeric.integer.decimal.gojo" },
      ],
    },

    operators: {
      patterns: [
        { match: "->|=>", name: "keyword.operator.arrow.gojo" },
        { match: String.raw`\?\?|\?\.`, name: "keyword.operator.optional.gojo" },
        { match: "<-", name: "keyword.operator.channel.gojo" },
        { match: String.raw`\.\.[=.]?`, name: "keyword.operator.range.gojo" },
        { match: "<<=|>>=|[-+*]%=|[-+*/%&|^]=", name: "keyword.operator.assignment.compound.gojo" },
        { match: "==|!=|<=|>=", name: "keyword.operator.comparison.gojo" },
        { match: "&&|\\|\\|", name: "keyword.operator.logical.gojo" },
        { match: "<<|>>", name: "keyword.operator.bitwise.shift.gojo" },
        { match: "[-+*]%", name: "keyword.operator.arithmetic.wrapping.gojo" },
        { match: "=", name: "keyword.operator.assignment.gojo" },
        { match: "[<>]", name: "keyword.operator.comparison.gojo" },
        { match: "[-+*/%]", name: "keyword.operator.arithmetic.gojo" },
        { match: "[&|^~]", name: "keyword.operator.bitwise.gojo" },
        // T ! E, !ok
        { match: "!", name: "keyword.operator.error.gojo" },
        // ?T
        { match: String.raw`\?`, name: "keyword.operator.optional.gojo" },
      ],
    },

    // Brackets nest, so that a ':' or '}' inside them stays in the
    // expression of an interpolation.
    groups: {
      patterns: [
        {
          begin: String.raw`\{`,
          end: String.raw`\}`,
          beginCaptures: { 0: { name: "punctuation.definition.block.begin.gojo" } },
          endCaptures: { 0: { name: "punctuation.definition.block.end.gojo" } },
          patterns: [{ include: "#literalFields" }, { include: "#statements" }],
        },
        {
          begin: String.raw`\(`,
          end: String.raw`\)`,
          beginCaptures: { 0: { name: "punctuation.definition.parameters.begin.gojo" } },
          endCaptures: { 0: { name: "punctuation.definition.parameters.end.gojo" } },
          patterns: [{ include: "#parameters" }, { include: "#statements" }],
        },
        {
          begin: String.raw`\[`,
          end: String.raw`\]`,
          beginCaptures: { 0: { name: "punctuation.definition.brackets.begin.gojo" } },
          endCaptures: { 0: { name: "punctuation.definition.brackets.end.gojo" } },
          patterns: [{ include: "#statements" }],
        },
      ],
    },

    // name: Type, in parameters and fields: the name is a parameter.
    parameters: {
      match: String.raw`(?:(?<=\(|,)|^)\s*(?:(inout|sink)\s+)?(${ident})\s*(?=:(?!:))`,
      captures: { 1: { name: "storage.modifier.gojo" }, 2: { name: "variable.parameter.gojo" } },
    },

    // Config{name: text}, .{x: 1, y: 2}
    literalFields: {
      match: String.raw`(?<=\{|,)\s*(${ident})\s*(?=:(?!:))`,
      captures: { 1: { name: "variable.other.property.gojo" } },
    },

    punctuation: {
      patterns: [
        { match: ",", name: "punctuation.separator.comma.gojo" },
        { match: ";", name: "punctuation.terminator.statement.gojo" },
        { match: ":", name: "punctuation.separator.colon.gojo" },
        { match: String.raw`\.`, name: "punctuation.accessor.gojo" },
        { match: "[@#]", name: "punctuation.other.gojo" },
      ],
    },
  },
}
