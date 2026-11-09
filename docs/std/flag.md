# flag

Command-line parsing: declare the options and arguments, then read them
back by name, typed. `--help` and the error messages come for free.

```gojo
var cli = flag.command("grep", "Search files for a regular expression.")
cli.bool("-i, --ignore-case", "match without regard to case")
cli.int("-C, --context N", 0, "show N lines around each match")
cli.strings("-e, --exclude GLOB", "skip files that match GLOB")
cli.arg("pattern", "the regular expression")
cli.rest("file", "files to search (default: standard input)")
let m = cli.parseOrExit()
m.bool("ignore-case")   m.int("context")   m.list("exclude")
m.string("pattern")     m.list("file")
```

A spec names a short form, a long form or both, then a placeholder for the
value of options that take one: `"-v"`, `"--dry-run"`, `"-o, --output
FILE"`. An option is read back by its long name without the dashes, or its
short letter when it has no long one.

The command line follows the usual conventions: `-abc` for several short
options, `-C3` or `-C 3`, `--context=3` or `--context 3`, options anywhere
among the arguments, and `--` before arguments that start with `-`.
Subcommands (`cli.subcommand(...)`) see the options of the commands above
them as well as their own.

**Types:** [`struct Error`](#struct-error), [`enum ErrorKind`](#enum-errorkind), [`struct Command`](#struct-command), [`struct Matches`](#struct-matches)

**Functions:** [`command`](#command)

## Types

### struct Error

```gojo
pub struct Error {
    pub kind: ErrorKind
    pub msg: String
    // the name of the command, as in usage: "git remote"
    pub command: String
}
```

Why parsing stopped: a mistake on the command line, or `--help` /
`--version`, whose text is the message.

#### Error.message

```gojo
pub fn message(self) -> String
```

### enum ErrorKind

```gojo
pub copy enum ErrorKind {
    // a mistake: an unknown option, a missing or bad value...
    Usage
    // `-h` or `--help`: the message is the help
    Help
    // `--version`: the message is the version
    Version
}
```

### struct Command

```gojo
pub struct Command {
    // and private fields
}
```

A command: its options, arguments and subcommands.

#### Command.version

```gojo
pub fn version(inout self, v: str)
```

Makes `--version` print `name version`.

#### Command.bool

```gojo
pub fn bool(inout self, spec: str, help: str)
```

An option without a value: true when it is given.

#### Command.int

```gojo
pub fn int(inout self, spec: str, initial: int, help: str)
```

An option with an integer value; `initial` when it isn't given.

#### Command.float

```gojo
pub fn float(inout self, spec: str, initial: f64, help: str)
```

An option with a number value.

#### Command.string

```gojo
pub fn string(inout self, spec: str, initial: str, help: str)
```

An option with a text value.

#### Command.strings

```gojo
pub fn strings(inout self, spec: str, help: str)
```

An option that can be given several times, each with a value: `-I a
-I b` reads back as `["a", "b"]`.

#### Command.arg

```gojo
pub fn arg(inout self, name: str, help: str)
```

A required argument, in order after the ones before it.

#### Command.optionalArg

```gojo
pub fn optionalArg(inout self, name: str, help: str)
```

An argument that may be left out (after the required ones).

#### Command.rest

```gojo
pub fn rest(inout self, name: str, help: str)
```

Any number of arguments, after all the others.

#### Command.subcommand

```gojo
pub fn subcommand(inout self, cmd: sink Command)
```

A subcommand: `git commit`. A command with subcommands takes no
arguments of its own.

#### Command.help

```gojo
pub fn help(self) -> String
```

The help text: what `--help` prints.

#### Command.parse

```gojo
pub fn parse(self, args: []String) -> Matches ! Error
```

Parses `args`, the command line without the program name.

#### Command.parseOrExit

```gojo
pub fn parseOrExit(self) -> Matches
```

Parses the program's command line. On `--help` or `--version` prints
the text and exits with 0; on a mistake prints it, with a hint to use
`--help`, and exits with 2.

### struct Matches

```gojo
pub struct Matches {
    // and private fields
}
```

The options and arguments of a parsed command line, by name. Reading a
name that wasn't declared, or as the wrong type, is a bug and panics.

#### Matches.bool

```gojo
pub fn bool(self, name: str) -> bool
```

The value of a bool option.

#### Matches.int

```gojo
pub fn int(self, name: str) -> int
```

The value of an integer option.

#### Matches.float

```gojo
pub fn float(self, name: str) -> f64
```

The value of a number option.

#### Matches.string

```gojo
pub fn string(self, name: str) -> String
```

The value of a text option or an argument ("" for an optional
argument that was left out).

#### Matches.list

```gojo
pub fn list(self, name: str) -> Vec[String]
```

The values of a `strings` option, or the `rest` arguments.

#### Matches.given

```gojo
pub fn given(self, name: str) -> bool
```

Whether the option or argument was on the command line.

#### Matches.command

```gojo
pub fn command(self) -> str
```

The subcommand that was run ("" for none), with its parents for
nested ones: "remote add".

## Functions

### command

```gojo
pub fn command(name: str, about: str) -> Command
```

A command with no options or arguments yet: `name` is how it is invoked,
`about` what it does.

