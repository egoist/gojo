# path

Slash-separated file paths: joining, cleaning and taking them apart, and
matching them against glob patterns. Only the text is looked at, except in
`abs` (which reads the working directory).

```gojo
path.join("src", "main.gojo")          // "src/main.gojo"
path.ext("archive.tar.gz")             // ".gz"
path.clean("a/./b/../c//")             // "a/c"
path.match("*.gojo", "main.gojo")      // true
```

**Functions:** [`join`](#join), [`joinAll`](#joinall), [`clean`](#clean), [`base`](#base), [`dir`](#dir), [`ext`](#ext), [`stem`](#stem), [`withExt`](#withext), [`isAbsolute`](#isabsolute), [`components`](#components), [`abs`](#abs), [`relative`](#relative), [`match`](#match)

## Functions

### join

```gojo
pub fn join(a: str, b: str) -> String
```

`a` and `b` joined with one `/` and cleaned; `b` alone when it is absolute.
An empty part is skipped.

### joinAll

```gojo
pub fn joinAll(parts: []String) -> String
```

The parts joined in order, as with `join`.

### clean

```gojo
pub fn clean(p: str) -> String
```

The shortest path naming the same file, by text alone: repeated slashes
become one, `.` parts go, a `..` removes the part before it (but not at the
root, `/..` is `/`), and there is no trailing slash. "" becomes ".".

### base

```gojo
pub fn base(p: str) -> String
```

The last part: "a/b/c.txt" -> "c.txt", "a/b/" -> "b", "/" -> "/", "" -> ".".

### dir

```gojo
pub fn dir(p: str) -> String
```

Everything but the last part, cleaned: "a/b/c" -> "a/b", "c" -> ".",
"/c" -> "/".

### ext

```gojo
pub fn ext(p: str) -> String
```

The extension of the last part, with its dot: "a/b.tar.gz" -> ".gz";
"" when there is none. A leading dot is not an extension (".bashrc" -> "").

### stem

```gojo
pub fn stem(p: str) -> String
```

The last part without its extension: "a/b.tar.gz" -> "b.tar".

### withExt

```gojo
pub fn withExt(p: str, e: str) -> String
```

The path with its extension replaced by `e` (with or without its dot; ""
removes it): `withExt("a/b.txt", "md")` is "a/b.md".

### isAbsolute

```gojo
pub fn isAbsolute(p: str) -> bool
```

Whether the path starts at the root.

### components

```gojo
pub fn components(p: str) -> Vec[String]
```

The parts between slashes, without empty ones and without cleaning:
"/usr//lib/" -> ["usr", "lib"]. (`isAbsolute` tells whether it was rooted.)

### abs

```gojo
pub fn abs(p: str) -> String ! os.Error
```

An absolute, cleaned path: relative ones are taken from the working
directory.

### relative

```gojo
pub fn relative(from: str, target: str) -> ?String
```

A path to `target` relative to `from`, so that `join(from, rel)` names
`target`: `relative("/a/b", "/a/c/d")` is "../c/d". Both are cleaned first
and must be both absolute or both relative; none otherwise, or when `from`
has `..` parts that `target` lacks.

### match

```gojo
pub fn match(pattern: str, name: str) -> bool
```

Whether `name` matches the glob `pattern` as a whole: `*` is any run of
characters but `/`, `?` any one character but `/`, `[abc]` / `[a-z]` /
`[!a-z]` a character in (or not in) a set, `\` takes the next character
literally; `**` as a whole part matches any number of parts:
`match("src/**/*.gojo", "src/a/b/c.gojo")`.

