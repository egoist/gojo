# csv

CSV (RFC 4180): rows of fields separated by commas (or another separator).
A field may be quoted with `"`, and then hold separators, line breaks and
`""` for a quote. Lines end with `\n` or `\r\n`; empty lines are skipped.

```gojo
let rows = try csv.parse("name,age\n\"Lee, Ann\",30\n")
// [["name", "age"], ["Lee, Ann", "30"]]
csv.format(rows)                     // the same text back

var r = csv.reader(try os.open("big.csv"))
while let row = try r.readRow() { ... }  // one row at a time
```

**Types:** [`struct Error`](#struct-error), [`struct Reader`](#struct-reader)

**Functions:** [`parse`](#parse), [`parseSep`](#parsesep), [`reader`](#reader), [`readerSep`](#readersep), [`formatRow`](#formatrow), [`format`](#format), [`formatSep`](#formatsep)

## Types

### struct Error

```gojo
pub struct Error {
    pub msg: String
    // where, from 1
    pub line: int
    pub column: int
}
```

Text that isn't valid CSV, or a read that failed.

#### Error.message

```gojo
pub fn message(self) -> String
```

### struct Reader

```gojo
pub struct Reader[R: io.Reader] {
    // and private fields
}
```

Reads rows one at a time from a stream, so a large file never has to be in
memory at once.

#### Reader.readRow

```gojo
pub fn readRow(inout self) -> ?Vec[String] ! Error
```

The next row, or none at the end.

## Functions

### parse

```gojo
pub fn parse(text: str) -> Vec[Vec[String]] ! Error
```

The rows of comma-separated `text`. Rows may have different numbers of
fields.

### parseSep

```gojo
pub fn parseSep(text: str, sep: rune) -> Vec[Vec[String]] ! Error
```

The rows of `text` with `sep` between fields (`'\t'`, `';'`...), which must
be ASCII.

### reader

```gojo
pub fn reader[R: io.Reader](src: sink R) -> Reader[R]
```

A reader of comma-separated rows from `src`.

### readerSep

```gojo
pub fn readerSep[R: io.Reader](src: sink R, sep: rune) -> Reader[R]
```

A reader of rows with `sep` between fields.

### formatRow

```gojo
pub fn formatRow(fields: []String) -> String
```

One row as CSV, without a line break. Fields are quoted only when they
need it: with a separator, quote or line break, or spaces at either end.

### format

```gojo
pub fn format(rows: []Vec[String]) -> String
```

The rows as CSV, each ending with `\n`.

### formatSep

```gojo
pub fn formatSep(rows: []Vec[String], sep: rune) -> String
```

The rows with `sep` between fields.

