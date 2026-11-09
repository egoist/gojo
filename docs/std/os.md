# os

Files, directories, the environment and processes.

```gojo
let text = try os.readText("notes.txt")
for name in try os.readDir(".") {
    println(name)
}
let r = try os.run("git", ["status", "--short"])
print(r.stdout)
```

**Types:** [`type Error`](#type-error), [`struct File`](#struct-file), [`enum FileKind`](#enum-filekind), [`struct Info`](#struct-info), [`struct Output`](#struct-output), [`enum Stdio`](#enum-stdio), [`struct Command`](#struct-command), [`struct Process`](#struct-process), [`enum Signal`](#enum-signal), [`struct Signals`](#struct-signals), [`enum Walk`](#enum-walk)

**Functions:** [`args`](#args), [`exit`](#exit), [`open`](#open), [`create`](#create), [`append`](#append), [`stdin, stdout, stderr`](#stdin-stdout-stderr), [`readFile`](#readfile), [`readText`](#readtext), [`writeFile`](#writefile), [`writeText`](#writetext), [`stat`](#stat), [`lstat`](#lstat), [`exists`](#exists), [`readDir`](#readdir), [`mkdir`](#mkdir), [`mkdirAll`](#mkdirall), [`remove`](#remove), [`removeAll`](#removeall), [`rename`](#rename), [`cwd`](#cwd), [`chdir`](#chdir), [`tempDir`](#tempdir), [`joinPath`](#joinpath), [`dirName`](#dirname), [`baseName`](#basename), [`getenv`](#getenv), [`setenv`](#setenv), [`unsetenv`](#unsetenv), [`run`](#run), [`runWithInput`](#runwithinput), [`call`](#call), [`pid`](#pid), [`environ`](#environ), [`command`](#command), [`notify`](#notify), [`walk`](#walk)

## Types

### type Error

```gojo
pub type Error = io.Error
```

Errors here are io.Error (with an io.ErrorKind to match on).

### struct File

```gojo
pub struct File {
    // and private fields
}
```

An open file. It is closed when it goes out of scope.

#### File.read

```gojo
pub fn read(self, buf: inout Vec[byte], max: int) -> int ! Error
```

Appends at most `max` bytes to `buf`; returns how many, 0 at the end.

#### File.write

```gojo
pub fn write(self, data: []byte) ! Error
```

Writes all of `data`.

#### File.writeText

```gojo
pub fn writeText(self, s: str) ! Error
```

Writes the bytes of `s`.

#### File.readAll

```gojo
pub fn readAll(self) -> Vec[byte] ! Error
```

Reads to the end.

#### File.readText

```gojo
pub fn readText(self) -> String ! Error
```

Reads to the end, as a string.

#### File.flush

```gojo
pub fn flush(self)
```

Writes out what `print` and the standard streams have buffered.

### enum FileKind

```gojo
pub copy enum FileKind { File, Directory, Symlink, Other }
```

What a path names. (`Other`: a device, socket or pipe.)

### struct Info

```gojo
pub struct Info {
    pub kind: FileKind
    pub size: int
    // last modification, in nanoseconds since 1970 (`time.local(info.modified)`)
    pub modified: int
    // permission bits, like 0o644
    pub mode: int
}
```

What `stat` reports about a file.

#### Info.isDir

```gojo
pub fn isDir(self) -> bool
```

Whether it is a directory.

#### Info.isFile

```gojo
pub fn isFile(self) -> bool
```

Whether it is a regular file.

### struct Output

```gojo
pub struct Output {
    // the exit code, or 128 + the signal that ended it
    pub status: int
    pub stdout: String
    pub stderr: String
}
```

What a finished process printed, and how it ended.

#### Output.ok

```gojo
pub fn ok(self) -> bool
```

Whether the process exited with status 0.

### enum Stdio

```gojo
pub copy enum Stdio {
    // this process's own stream
    Inherit
    // a pipe: the `Process` holds the other end as a `File`
    Pipe
    // nothing: reads see the end at once, writes go nowhere
    Null
    // for stderr: wherever stdout goes (`2>&1`)
    Stdout
}
```

How a child's standard stream is set up.

### struct Command

```gojo
pub struct Command {
    pub stdin: Stdio = .Inherit
    pub stdout: Stdio = .Inherit
    pub stderr: Stdio = .Inherit
    // and private fields
}
```

A program to start, and how: build one with `command`, adjust it, then
`start`, `run` or `output` it (as often as needed).

```gojo
var c = os.command("git", ["log", "--oneline"])
c.dir("/src/app")
c.env("GIT_PAGER", "cat")
let out = try c.output()          // waits; out.stdout, out.status

var sort = os.command("sort", [])
sort.stdin = .Pipe
sort.stdout = .Pipe
var p = try sort.start()
if let w = p.stdin { try w.writeText("b\na\n") }
p.stdin = none                    // closes it: sort sees the end
let sorted = try p.stdout?.readText()
let status = try p.wait()
```

#### Command.arg

```gojo
pub fn arg(inout self, a: str)
```

Adds an argument.

#### Command.env

```gojo
pub fn env(inout self, name: str, value: str)
```

Sets an environment variable for the child.

#### Command.unsetEnv

```gojo
pub fn unsetEnv(inout self, name: str)
```

Removes an environment variable for the child.

#### Command.clearEnv

```gojo
pub fn clearEnv(inout self)
```

Starts the child with no environment variables but those set with
`env`.

#### Command.dir

```gojo
pub fn dir(inout self, path: str)
```

Runs the child in directory `path`.

#### Command.start

```gojo
pub fn start(self) -> Process ! Error
```

Starts the child without waiting for it.

#### Command.run

```gojo
pub fn run(self) -> int ! Error
```

Starts the child and waits for it: its status (the exit code, or 128 +
the signal that ended it).

#### Command.output

```gojo
pub fn output(self) -> Output ! Error
```

Starts the child with its output piped (unless set otherwise), and
waits for it, collecting what it prints. Stdin is empty unless set.

### struct Process

```gojo
pub struct Process {
    pub stdin: ?File
    pub stdout: ?File
    pub stderr: ?File
    // and private fields
}
```

A started child process. Its pipe ends close when they are dropped (set
`stdin` to none to let the child see the end of its input). Wait for it:
a child that is dropped while still running is left running.

#### Process.id

```gojo
pub fn id(self) -> int
```

The process ID.

#### Process.wait

```gojo
pub fn wait(inout self) -> int ! Error
```

Closes stdin, then waits for the child to end: its status (the exit
code, or 128 + the signal that ended it). Cancelling the task stops
the wait (`isCancelled()`), not the child.

#### Process.tryWait

```gojo
pub fn tryWait(inout self) -> ?int
```

The status if the child has ended, without waiting.

#### Process.signal

```gojo
pub fn signal(self, sig: Signal) ! Error
```

Sends the child a signal (`.Term` asks it to stop).

#### Process.kill

```gojo
pub fn kill(self) ! Error
```

Ends the child at once (SIGKILL); wait for it afterwards.

### enum Signal

```gojo
pub copy enum Signal {
    Hup
    Int
    Quit
    Kill
    Term
    Usr1
    Usr2
    Winch
    Pipe
    Alrm
    Chld
    Cont
    Stop
    Tstp
}
```

The POSIX signals, by name.

### struct Signals

```gojo
pub struct Signals {
    // and private fields
}
```

Signals as they arrive; see `notify`.

#### Signals.next

```gojo
pub fn next(self) -> Signal ! Error
```

Waits for the next signal. Cancelling the task stops the wait.

### enum Walk

```gojo
pub copy enum Walk {
    // go on (into it, for a directory)
    Continue
    // don't go into this directory
    SkipDir
    // stop the walk
    Stop
}
```

What `walk` does after visiting a path.

## Functions

### args

```gojo
pub fn args() -> Vec[String]
```

The command line: the program's name, then its arguments.

### exit

```gojo
pub fn exit(code: int) -> never
```

Ends the program with an exit code, after flushing standard output. Deferred
code does not run.

### open

```gojo
pub fn open(path: str) -> File ! Error
```

Opens a file for reading.

### create

```gojo
pub fn create(path: str) -> File ! Error
```

Creates a file for writing, or empties an existing one.

### append

```gojo
pub fn append(path: str) -> File ! Error
```

Opens a file for writing at its end, creating it if needed.

### stdin, stdout, stderr

```gojo
pub fn stdin() -> File
pub fn stdout() -> File
pub fn stderr() -> File
```

The standard streams. They are not closed when the `File` is dropped.

### readFile

```gojo
pub fn readFile(path: str) -> Vec[byte] ! Error
```

The whole contents of a file.

### readText

```gojo
pub fn readText(path: str) -> String ! Error
```

The whole contents of a file, as a string (the bytes aren't checked to be
UTF-8).

### writeFile

```gojo
pub fn writeFile(path: str, data: []byte) ! Error
```

Creates or replaces a file with `data`. Not atomic: write to a temporary
name and `rename` it into place for that.

### writeText

```gojo
pub fn writeText(path: str, text: str) ! Error
```

Creates or replaces a file with the bytes of `text`.

### stat

```gojo
pub fn stat(path: str) -> Info ! Error
```

What `path` is; follows symbolic links.

### lstat

```gojo
pub fn lstat(path: str) -> Info ! Error
```

Like `stat`, but describes a symbolic link itself.

### exists

```gojo
pub fn exists(path: str) -> bool
```

Whether `path` names something (following symbolic links). To act on a
file, just try it and handle the error: it may change in between.

### readDir

```gojo
pub fn readDir(path: str) -> Vec[String] ! Error
```

The names of the entries in a directory, sorted.

### mkdir

```gojo
pub fn mkdir(path: str) ! Error
```

Creates a directory; fails if it exists or its parent doesn't.

### mkdirAll

```gojo
pub fn mkdirAll(path: str) ! Error
```

Creates a directory and any missing parents; fine if it exists already.

### remove

```gojo
pub fn remove(path: str) ! Error
```

Removes a file or an empty directory.

### removeAll

```gojo
pub fn removeAll(path: str) ! Error
```

Removes a file, or a directory and everything in it. Symbolic links are
removed, not followed.

### rename

```gojo
pub fn rename(from: str, to: str) ! Error
```

Renames or moves a file or directory, replacing `to` if it is a file. Both
must be on the same file system.

### cwd

```gojo
pub fn cwd() -> String ! Error
```

The working directory.

### chdir

```gojo
pub fn chdir(path: str) ! Error
```

Changes the working directory, for the whole process.

### tempDir

```gojo
pub fn tempDir() -> String
```

The directory for temporary files ($TMPDIR, or /tmp).

### joinPath

```gojo
pub fn joinPath(a: str, b: str) -> String
```

"a/b" + "c" = "a/b/c".

### dirName

```gojo
pub fn dirName(path: str) -> String
```

The directory part of a path: "a/b/c" -> "a/b", "c" -> ".", "/c" -> "/".

### baseName

```gojo
pub fn baseName(path: str) -> String
```

The last part of a path: "a/b/c.txt" -> "c.txt".

### getenv

```gojo
pub fn getenv(name: str) -> ?String
```

The value of an environment variable.

### setenv

```gojo
pub fn setenv(name: str, value: str) ! Error
```

Sets an environment variable, for this process and the programs it starts.

### unsetenv

```gojo
pub fn unsetenv(name: str) ! Error
```

Removes an environment variable.

### run

```gojo
pub fn run(prog: str, args: []String) -> Output ! Error
```

Runs a program (looked up in $PATH) and waits for it, collecting what it
prints. Fails only if it can't be started: check `status` for how it ended.

### runWithInput

```gojo
pub fn runWithInput(prog: str, args: []String, input: str) -> Output ! Error
```

Like `run`, with `input` as the program's standard input.

### call

```gojo
pub fn call(prog: str, args: []String) -> int ! Error
```

Runs a program on this process's standard streams and returns its exit code.

### pid

```gojo
pub fn pid() -> int
```

This process's ID.

### environ

```gojo
pub fn environ() -> Vec[(String, String)]
```

The environment variables of this process, as (name, value) pairs.

### command

```gojo
pub fn command(prog: str, args: []String) -> Command
```

A program to run, with these arguments (looked up in $PATH unless it holds
a `/`). Its standard streams are this process's unless set otherwise.

### notify

```gojo
pub fn notify(sigs: []Signal) -> Signals ! Error
```

Starts listening for these signals: each one that arrives is delivered to
`Signals.next` instead of having its usual effect. Ctrl-C (`.Int`) and
`.Term` then no longer shut the program down; when the `Signals` is
dropped, they do again.

```gojo
var sigs = try os.notify([.Hup, .Term])
loop {
    match try sigs.next() {
        .Hup => reload()
        _ => break
    }
}
```

### walk

```gojo
pub fn walk(root: str, visit: fn(str, Info) -> Walk) ! Error
```

Visits `root` and everything below it, depth first, a directory's entries
in name order after the directory itself. Symbolic links are visited, not
followed. An entry that can't be read ends the walk with its error.

```gojo
try os.walk("src", (p, info) => {
    if info.isDir() && os.baseName(p) == ".git" { return .SkipDir }
    if p.endsWith(".gojo") { files.push(p.toString()) }
    return .Continue
})
```

