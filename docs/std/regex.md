# regex

Regular expressions with the syntax of RE2 (as in Go and Rust). Matching
takes time linear in the length of the text whatever the pattern, so a
pattern from a user can't hang the program; in exchange there are no
backreferences and no lookaround.

```gojo
let date = regex.mustCompile(`(?P<y>\d{4})-(?P<m>\d\d)-(?P<d>\d\d)`)
date.isMatch("due 2026-11-08")                   // true
if let c = date.captures("due 2026-11-08") {
    c.name("y")                                  // some("2026")
}
date.replaceAll("2026-11-08", `$d/$m/$y`)        // "08/11/2026"
regex.mustCompile(`\s*,\s*`).split("a , b,c")    // ["a", "b", "c"]
```

Offsets are byte offsets into the text, as everywhere for `str`, and a
match is a `Span`: `re.find(s)?.of(s)` is its text. A compiled `Regex` can
be shared by every task, so a package-level `let` compiles it once:

```gojo
let word = regex.mustCompile(`\w+`)
```

Syntax:

```gojo
x            the character x; `\` before punctuation makes it literal
\n \t \r \f \v \a \x7F \x{10FFFF}   escapes; \Q...\E is literal text
.            any character but \n (any at all with the s flag)
[abc] [^a-z] a class; inside, also \d, \pL and [:alpha:] (ASCII)
\d \w \s     ASCII digit, word character [0-9A-Za-z_], space [\t\n\f\r ]
\D \W \S     anything else
\pL \p{Lu}   Unicode: L Lu Ll N Nd P S Cc White_Space (and the long
             names, Letter...); \PL \P{Lu} anything else
x* x+ x?     repetition, as many as possible; x{n} x{n,} x{n,m} (n, m
             at most 1000); with a ? after them, as few as possible
xy x|y       sequence, alternatives (the first that matches wins)
(x) (?P<name>x) (?<name>x)   capture groups; (?:x) doesn't capture
^ $          start and end of the text (of a line, with the m flag)
\A \z        start and end of the text
\b \B        an ASCII word boundary, and anything else
(?flags) (?flags:x)   set flags for the rest of the group, or for x;
             `-` turns them off: i case-insensitive (Unicode simple
             folding), m multi-line, s `.` matches \n, U swaps greedy
             and lazy
```

**Types:** [`struct Error`](#struct-error), [`struct Regex`](#struct-regex), [`struct Captures`](#struct-captures)

**Functions:** [`compile`](#compile), [`mustCompile`](#mustcompile), [`isMatch`](#ismatch), [`escape`](#escape)

## Types

### struct Error

```gojo
pub struct Error {
    pub msg: String
    pub pattern: String
    // the byte offset of the problem in the pattern
    pub offset: int
}
```

A pattern that can't be compiled.

#### Error.message

```gojo
pub fn message(self) -> String
```

### struct Regex

```gojo
pub struct Regex {
    // and private fields
}
```

A compiled regular expression. It holds no state while matching, so tasks
can share one.

#### Regex.toString

```gojo
pub fn toString(self) -> String
```

The pattern it was compiled from.

#### Regex.groupCount

```gojo
pub fn groupCount(self) -> int
```

The number of capture groups, not counting the whole match.

#### Regex.groupIndex

```gojo
pub fn groupIndex(self, name: str) -> ?int
```

The number of the group named `name`.

#### Regex.isMatch

```gojo
pub fn isMatch(self, text: str) -> bool
```

Whether it matches somewhere in `text`.

#### Regex.find

```gojo
pub fn find(self, text: str) -> ?Span
```

The leftmost match, as a span of `text`.

#### Regex.findAt

```gojo
pub fn findAt(self, text: str, start: int) -> ?Span
```

The leftmost match that starts at or after byte `start`. The text
before `start` still counts for `^`, `\b` and the like.

#### Regex.findText

```gojo
pub fn findText(self, text: str) -> ?str
```

The text of the leftmost match.

#### Regex.findAll

```gojo
pub fn findAll(self, text: str) -> Vec[Span]
```

Every match, left to right, without overlaps. An empty match right
after another match doesn't count.

#### Regex.findAllText

```gojo
pub fn findAllText(self, text: str) -> Vec[String]
```

The text of every match.

#### Regex.captures

```gojo
pub fn captures(self, text: str) -> ?Captures
```

The leftmost match and its groups.

#### Regex.capturesAll

```gojo
pub fn capturesAll(self, text: str) -> Vec[Captures]
```

Every match (as `findAll`) with its groups.

#### Regex.replace

```gojo
pub fn replace(self, text: str, template: str) -> String
```

`text` with the leftmost match replaced by `template`, in which `$1`
or `${1}` is the text of group 1, `$name` or `${name}` that of a named
group, and `$$` a `$`. A group that doesn't exist or didn't take part
is empty. Write a template with `${...}` as a raw string, so that it
isn't interpolation: `` re.replace(s, `${y}`) ``.

#### Regex.replaceAll

```gojo
pub fn replaceAll(self, text: str, template: str) -> String
```

`text` with every match replaced by `template` (as in `replace`).

#### Regex.replaceN

```gojo
pub fn replaceN(self, text: str, template: str, n: int) -> String
```

`text` with at most `n` matches replaced (all of them when `n` < 0).

#### Regex.replaceAllWith

```gojo
pub fn replaceAllWith(self, text: str, f: fn(Captures) -> String) -> String
```

`text` with every match replaced by what `f` gives for it.

```gojo
re.replaceAllWith(s, c => c.text().toUpper())
```

#### Regex.split

```gojo
pub fn split(self, text: str) -> Vec[String]
```

The pieces of `text` between the matches.

#### Regex.splitN

```gojo
pub fn splitN(self, text: str, n: int) -> Vec[String]
```

At most `n` pieces (all of them when `n` < 0); the last holds the rest
of the text.

### struct Captures

```gojo
pub struct Captures {
    // and private fields
}
```

A match and the text of its capture groups. It owns a copy of the matched
text, so it can outlive the text that was searched.

#### Captures.text

```gojo
pub fn text(self) -> str
```

The text of the whole match.

#### Captures.get

```gojo
pub fn get(self, i: int) -> ?str
```

The text of group `i` (0 is the whole match), or none when there is
no such group or it didn't take part in the match: `(a)|b` on "b".

#### Captures.name

```gojo
pub fn name(self, name: str) -> ?str
```

The text of the group named `name`.

#### Captures.span

```gojo
pub fn span(self, i: int) -> ?Span
```

Where group `i` is in the text that was searched.

#### Captures.len

```gojo
pub fn len(self) -> int
```

The number of groups, the whole match included.

#### Captures.expand

```gojo
pub fn expand(self, template: str) -> String
```

`template` with `$1`, `${name}` and `$$` replaced, as in
`Regex.replace`.

#### Captures.toString

```gojo
pub fn toString(self) -> String
```

The groups' texts: `["2026-11-08", "2026", none]`.

## Functions

### compile

```gojo
pub fn compile(pattern: str) -> Regex ! Error
```

The compiled form of `pattern`.

### mustCompile

```gojo
pub fn mustCompile(pattern: str) -> Regex
```

The compiled form of `pattern`, which must be valid: for patterns written
in the program, as in a package-level `let`. Panics with the error.

### isMatch

```gojo
pub fn isMatch(pattern: str, text: str) -> bool ! Error
```

Whether `pattern` matches somewhere in `text`, compiling it each time (keep
a `Regex` to match many times).

### escape

```gojo
pub fn escape(s: str) -> String
```

`s` with the characters that mean something in a pattern escaped, so the
result matches `s` literally: `escape("1.5*")` is `1\.5\*`.

