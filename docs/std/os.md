# os

Files, directories, the environment and processes.

```kite
let text = try os.readText("notes.txt")
for name in try os.readDir(".") {
    println(name)
}
let r = try os.run("git", ["status", "--short"])
print(r.stdout)
```

**Types:** [`type Error`](#type-error), [`struct File`](#struct-file), [`enum FileKind`](#enum-filekind), [`struct Info`](#struct-info), [`struct Output`](#struct-output)

**Functions:** [`args`](#args), [`exit`](#exit), [`open`](#open), [`create`](#create), [`append`](#append), [`stdin, stdout, stderr`](#stdin-stdout-stderr), [`readFile`](#readfile), [`readText`](#readtext), [`writeFile`](#writefile), [`writeText`](#writetext), [`stat`](#stat), [`lstat`](#lstat), [`exists`](#exists), [`readDir`](#readdir), [`mkdir`](#mkdir), [`mkdirAll`](#mkdirall), [`remove`](#remove), [`removeAll`](#removeall), [`rename`](#rename), [`cwd`](#cwd), [`chdir`](#chdir), [`tempDir`](#tempdir), [`joinPath`](#joinpath), [`dirName`](#dirname), [`baseName`](#basename), [`getenv`](#getenv), [`setenv`](#setenv), [`unsetenv`](#unsetenv), [`run`](#run), [`runWithInput`](#runwithinput), [`call`](#call)

## Types

### type Error

```kite
pub type Error = io.Error
```

Errors here are io.Error (with an io.ErrorKind to match on).

### struct File

```kite
pub struct File {
    // and private fields
}
```

An open file. It is closed when it goes out of scope.

#### File.read

```kite
pub fn read(self, buf inout Vec[byte], max int) int ! Error
```

Appends at most `max` bytes to `buf`; returns how many, 0 at the end.

#### File.write

```kite
pub fn write(self, data []byte) ! Error
```

Writes all of `data`.

#### File.writeText

```kite
pub fn writeText(self, s str) ! Error
```

Writes the bytes of `s`.

#### File.readAll

```kite
pub fn readAll(self) Vec[byte] ! Error
```

Reads to the end.

#### File.readText

```kite
pub fn readText(self) String ! Error
```

Reads to the end, as a string.

#### File.flush

```kite
pub fn flush(self)
```

Writes out what `print` and the standard streams have buffered.

### enum FileKind

```kite
pub copy enum FileKind { File, Directory, Symlink, Other }
```

What a path names. (`Other`: a device, socket or pipe.)

### struct Info

```kite
pub struct Info {
    pub kind FileKind
    pub size int
    // last modification, in nanoseconds since 1970 (`time.local(info.modified)`)
    pub modified int
    // permission bits, like 0o644
    pub mode int
}
```

What `stat` reports about a file.

#### Info.isDir

```kite
pub fn isDir(self) bool
```

Whether it is a directory.

#### Info.isFile

```kite
pub fn isFile(self) bool
```

Whether it is a regular file.

### struct Output

```kite
pub struct Output {
    // the exit code, or 128 + the signal that ended it
    pub status int
    pub stdout String
    pub stderr String
}
```

What a finished process printed, and how it ended.

#### Output.ok

```kite
pub fn ok(self) bool
```

Whether the process exited with status 0.

## Functions

### args

```kite
pub fn args() Vec[String]
```

The command line: the program's name, then its arguments.

### exit

```kite
pub fn exit(code int) never
```

Ends the program with an exit code, after flushing standard output. Deferred
code does not run.

### open

```kite
pub fn open(path str) File ! Error
```

Opens a file for reading.

### create

```kite
pub fn create(path str) File ! Error
```

Creates a file for writing, or empties an existing one.

### append

```kite
pub fn append(path str) File ! Error
```

Opens a file for writing at its end, creating it if needed.

### stdin, stdout, stderr

```kite
pub fn stdin() File
pub fn stdout() File
pub fn stderr() File
```

The standard streams. They are not closed when the `File` is dropped.

### readFile

```kite
pub fn readFile(path str) Vec[byte] ! Error
```

The whole contents of a file.

### readText

```kite
pub fn readText(path str) String ! Error
```

The whole contents of a file, as a string (the bytes aren't checked to be
UTF-8).

### writeFile

```kite
pub fn writeFile(path str, data []byte) ! Error
```

Creates or replaces a file with `data`. Not atomic: write to a temporary
name and `rename` it into place for that.

### writeText

```kite
pub fn writeText(path str, text str) ! Error
```

Creates or replaces a file with the bytes of `text`.

### stat

```kite
pub fn stat(path str) Info ! Error
```

What `path` is; follows symbolic links.

### lstat

```kite
pub fn lstat(path str) Info ! Error
```

Like `stat`, but describes a symbolic link itself.

### exists

```kite
pub fn exists(path str) bool
```

Whether `path` names something (following symbolic links). To act on a
file, just try it and handle the error: it may change in between.

### readDir

```kite
pub fn readDir(path str) Vec[String] ! Error
```

The names of the entries in a directory, sorted.

### mkdir

```kite
pub fn mkdir(path str) ! Error
```

Creates a directory; fails if it exists or its parent doesn't.

### mkdirAll

```kite
pub fn mkdirAll(path str) ! Error
```

Creates a directory and any missing parents; fine if it exists already.

### remove

```kite
pub fn remove(path str) ! Error
```

Removes a file or an empty directory.

### removeAll

```kite
pub fn removeAll(path str) ! Error
```

Removes a file, or a directory and everything in it. Symbolic links are
removed, not followed.

### rename

```kite
pub fn rename(from str, to str) ! Error
```

Renames or moves a file or directory, replacing `to` if it is a file. Both
must be on the same file system.

### cwd

```kite
pub fn cwd() String ! Error
```

The working directory.

### chdir

```kite
pub fn chdir(path str) ! Error
```

Changes the working directory, for the whole process.

### tempDir

```kite
pub fn tempDir() String
```

The directory for temporary files ($TMPDIR, or /tmp).

### joinPath

```kite
pub fn joinPath(a str, b str) String
```

"a/b" + "c" = "a/b/c".

### dirName

```kite
pub fn dirName(path str) String
```

The directory part of a path: "a/b/c" -> "a/b", "c" -> ".", "/c" -> "/".

### baseName

```kite
pub fn baseName(path str) String
```

The last part of a path: "a/b/c.txt" -> "c.txt".

### getenv

```kite
pub fn getenv(name str) ?String
```

The value of an environment variable.

### setenv

```kite
pub fn setenv(name str, value str) ! Error
```

Sets an environment variable, for this process and the programs it starts.

### unsetenv

```kite
pub fn unsetenv(name str) ! Error
```

Removes an environment variable.

### run

```kite
pub fn run(prog str, args []String) Output ! Error
```

Runs a program (looked up in $PATH) and waits for it, collecting what it
prints. Fails only if it can't be started: check `status` for how it ended.

### runWithInput

```kite
pub fn runWithInput(prog str, args []String, input str) Output ! Error
```

Like `run`, with `input` as the program's standard input.

### call

```kite
pub fn call(prog str, args []String) int ! Error
```

Runs a program on this process's standard streams and returns its exit code.

