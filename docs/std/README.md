# The standard library

The prelude is available everywhere; other modules are imported with `use std/name` (`use std/{os, json}` for several).

| module | |
| --- | --- |
| [`prelude`](prelude.md) | The prelude: available in every Gojo file without an import. |
| [`fmt`](fmt.md) | Formatting and printing. |
| [`os`](os.md) | Files, directories, the environment and processes. |
| [`math`](math.md) | Floating-point math and integer limits. |
| [`time`](time.md) | Time: a monotonic clock for measuring, the wall clock and calendar dates. |
| [`task`](task.md) | Tasks and cancellation. |
| [`net`](net.md) | TCP networking. |
| [`io`](io.md) | Errors from the operating system, files, processes and the network, and the interfaces for streams of bytes. |
| [`bufio`](bufio.md) | Buffered reading and writing on top of io.Reader / io.Writer. |
| [`json`](json.md) | JSON: parse text into a `json.Value` tree and encode it back. |
| [`http`](http.md) | HTTP/1.1: a server built on tasks, scopes and cancellation, and a small client. |
| [`strings`](strings.md) | Building strings piece by piece. |

