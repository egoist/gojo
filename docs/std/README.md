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
| [`http`](http.md) | HTTP/1.1: a server built on tasks, scopes and cancellation, and a small client, over plain TCP or TLS (`serveTls`, `https://` URLs). |
| [`strings`](strings.md) | Building strings piece by piece. |
| [`rand`](rand.md) | Random numbers: fast pseudo-random values for simulations, sampling and shuffling, a seeded generator for reproducible sequences, and cryptographically secure bytes from the operating system. |
| [`path`](path.md) | Slash-separated file paths: joining, cleaning and taking them apart, and matching them against glob patterns. |
| [`hex`](hex.md) | Hexadecimal: bytes as pairs of hex digits, and back. |
| [`base64`](base64.md) | Base64 (RFC 4648): bytes as text, in the standard alphabet (with `+`, `/` and `=` padding) or the URL-safe one (with `-`, `_` and no padding, as in JWTs). |
| [`url`](url.md) | URLs (RFC 3986): parsing them into their parts and back, query strings, and percent-encoding. |
| [`csv`](csv.md) | CSV (RFC 4180): rows of fields separated by commas (or another separator). |
| [`collections`](collections.md) | Collections beyond the prelude's `Vec`, `Map` and `Set`: a double-ended queue, a priority queue, maps and sets kept in key order, and a set of small integers. |
| [`crypto`](crypto.md) | Cryptographic hashes (SHA-2, and SHA-1 and MD5 for old formats), HMAC, PBKDF2, and comparison in constant time. |
| [`crc32`](crc32.md) | CRC-32 checksums (IEEE 802.3: the one in zip, gzip and PNG). |
| [`binary`](binary.md) | Fixed-size numbers as bytes, in either byte order, and variable-length integers (varints, as in Protocol Buffers and Go's encoding/binary). |
| [`tls`](tls.md) | TLS: encrypted, authenticated connections (what HTTPS runs on), through the system's OpenSSL 1.1 or 3. |
| [`regex`](regex.md) | Regular expressions with the syntax of RE2 (as in Go and Rust). |
| [`flag`](flag.md) | Command-line parsing: declare the options and arguments, then read them back by name, typed. |
| [`log`](log.md) | Leveled, structured logging: one line per event, with key=value fields, to standard error. |
| [`simd`](simd.md) | Fixed-width vectors of numbers, for code that works on several values at once: `Simd[f32, 8]` holds 8 lanes. |

