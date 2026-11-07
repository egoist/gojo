# Packages (M6 design)

Status: proposal. Once agreed, the rules move into the language reference
(`spec.md`) and this file keeps the reasons.

## What newer systems taught

| System | What it got right | What went wrong, or what we skip |
| --- | --- | --- |
| Cargo (Rust) | A manifest that names dependencies; a lock file; semver ranges by default; workspaces; crates.io with docs.rs building docs automatically; editions | `build.rs` runs arbitrary code at build time; features multiply the build matrix; flat names invite squatting |
| uv (Python), Dart pub, Gleam | The PubGrub solver: fast, and when resolution fails it explains why in a sentence. A cross-platform lock file. Speed as a feature | (Python's legacy, which Kite doesn't have) |
| Deno / JSR | Scoped names (`@scope/name`); versions can't change once published; an explicit `exports` list; docs and a quality score generated from source; publishing from CI with provenance and no tokens | URL imports in source were tried and abandoned: long URLs everywhere, no deduplication, no single place that says what the program depends on |
| Zig, Roc, Nix | Content addressing: a dependency is identified by the hash of its contents, so mirrors and caches are free to be wrong; nothing can change under you | No version resolution: duplicate dependencies pile up |
| Elm | The tool computes the API change and decides whether a release is major, minor or patch. Semver becomes a fact, not a promise | |
| Go | Builds never run dependency code. Reproducible by design. No registry required | Paths like `github.com/...` in every import; no ranges; one major version per path |
| npm | | Install scripts run code from every dependency; `node_modules` duplication |

## The design in one page

```toml
# kite.toml
[package]
name = "@ann/notes"
version = "0.3.0"
edition = "2026"

[dependencies]
yaml = "1.4"                                     # @kite/yaml from the registry, ^1.4
uuid = { package = "@carol/uuid", version = "0.3" }
store = { path = "../store" }                    # a local package
parse = { git = "https://github.com/bob/kite-parse", tag = "v2.0.1" }
```

```kite
use std/{fmt, os}                  // the standard library
use yaml, uuid                     // dependencies, by their names in kite.toml
use yaml/schema                    // a subpackage the dependency exports
use ./cli                          // a package inside this one
```

```sh
kite add yaml                      # resolves, locks, downloads
kite add @carol/uuid
kite update                        # newest versions the ranges allow
kite tree                          # what is in the build, and why
kite publish                       # checks the semver bump against the API diff
```

## Packages and names

- **A package is a directory with a `kite.toml`.** Inside it, directories are
  subpackages, imported as `./cli` (from within) or `name/cli` (from outside).
  A directory without a manifest works as today: `kite run hello.kite` needs
  no `kite.toml`.
- **Registry names are scoped:** `@scope/name`, like JSR and npm's scoped
  packages. A scope belongs to a verified account or organisation, so nobody
  can squat `yaml`. The standard project's scope is `@kite`, and `@kite/*`
  packages may be written without the scope (`yaml = "1.4"` above).
- **In source, dependencies have short names**, the keys of
  `[dependencies]`. That is the lesson of Deno's URL imports: the manifest is
  the one place that says where code comes from, and imports stay short.
- **The standard library is one name, `std`** (as in Rust, Zig and Gleam):
  `use std/os`, `use std/{fmt, os, time}`. Where an import comes from is
  visible on the line: `std/...` is the standard library, `./...` is this
  package, and a bare name is a dependency from `kite.toml`. Only `std` is
  reserved, so the standard library can add modules without colliding with
  anyone's dependency names. `use std/os` still binds `os`, and the prelude
  (`println`, `assert`...) still needs no import. This is already
  implemented, ahead of the rest.
- **`exports`.** A package lists the subpackages other packages may import:
  `exports = [".", "schema"]` (default: just the root). Everything else is
  private to the package. This replaces Go's `internal/` convention, and it
  defines the API that semver checking compares (below).

## Versions and resolution

- **Ranges, Cargo semantics.** `"1.4"` means `^1.4` (>= 1.4.0, < 2.0.0);
  `"0.3"` means >= 0.3.0, < 0.4.0; `"=1.4.2"`, `">=1.2, <1.6"` and `"*"` are
  also allowed.
- **A PubGrub solver,** as in uv, Dart and Gleam. It picks the newest
  versions that satisfy every range. When that is impossible, it explains why
  in plain steps: "`@ann/notes` needs `yaml ^2`, `uuid 0.3.1` needs `yaml
  ^1.2`, so no version of `uuid 0.3` works; try `uuid 0.4`". That message is
  written for the agent that has to fix it.
- **Incompatible major versions can coexist** (Cargo): `yaml 1.x` and `yaml
  2.x` may both be in a build, and get different internal names. When values
  of the two meet, the type error names the versions ("`yaml.Doc` from yaml
  1.9.0 is not `yaml.Doc` from yaml 2.1.0").
- **`kite.lock`** records every package with its exact version, source and
  content hash. It is written by `kite add`, `update` and `remove`, and
  committed for applications and libraries alike (as Cargo now recommends).
  Builds use only the lock file; with `--locked` (for CI) a lock file that
  doesn't match the manifest is an error rather than being updated.
- **Workspaces:** `[workspace] members = ["app", "lib/*"]` in a top-level
  `kite.toml`. Members share one lock file and depend on each other by path.

## The registry

- **Published versions are immutable,** and every published version has a
  content hash. The registry can withdraw ("yank") a version, but never
  change it.
- **The index is static files over HTTPS** (Cargo's "sparse" index): one file
  per package listing its versions, dependencies and hashes, and archives
  stored by hash. It needs no database to read, and it can sit on object
  storage behind a CDN. Mirrors are trivial and can't lie, because the hashes
  come from the index and the lock file.
- **Publishing** goes through a small API. Logins are bound to a GitHub (or
  other) identity, and CI can publish with an OIDC token instead of a stored
  secret, which also records where the release was built (provenance, as
  JSR and npm do).
- **On publish the registry generates docs** with `kite doc`, and shows
  them on the package page.
- **Git and path sources** work without the registry, so they cover the beta
  until the registry runs, as well as private code. The registry is the
  primary source once it exists.

## Semver the compiler checks

`kite publish` compares the public API of the new version with the last
published one. The API is what `kite doc` extracts, made precise:
signatures, fields, variants, capabilities, and the parameter modes.

- A removal, or any incompatible change, needs a major version.
- An addition needs a minor version.
- Neither needs at least a patch version.

A breaking change under a minor version is refused, with the list of what
broke. This is Elm's rule. It works here because the compiler knows exactly
what is public and what a change can break: for example, adding a field
without a default to a public struct breaks struct literals, and adding a
method to a public interface breaks implementers. Agents can then trust that
`kite update` within a range won't break the build.

## No code runs when you install or build

There are no build scripts, install hooks, or manifests written as code:
- `kite.toml` is data;
- the C compiler only sees C that Kite generated;
- `extern "C" static "z"` link names must be plain names
  (`[A-Za-z0-9_.+-]`, not starting with `-`).

Today that last check is missing: the name goes into the generated C
unchecked, and a newline in it injects C code. That gets fixed first,
whatever else is decided.

## Building and caching

- **The cache is content-addressed:** `~/.cache/kite/pkg/<sha256>/`, written
  once and read-only. Two projects using the same version share it, and a
  hash mismatch is an error naming the package.
- `KITE_OFFLINE=1`, or `--offline`, uses only the cache.
- `kite vendor` copies every dependency into `vendor/` for air-gapped builds.
- **The compiler stays whole-program:** dependencies compile from source
  with the program. Per-package compilation caching is separate work, and
  this design doesn't block it: the lock file plus content hashes are exactly
  the cache keys it will need.
- **Editions.** `edition = "2026"` selects the language rules a package is
  written for. A stricter rule in a later compiler applies only to packages
  that opt into the newer edition, so a compiler upgrade never breaks a
  dependency nobody is editing (Rust's editions).
- **Errors in dependencies** name the package and version, and point at the
  cached file.

## For coding agents

- Every error says which command fixes it, for example `` `yaml` is not a
  dependency: run `kite add yaml` ``.
- `kite add`, `tree`, `update`, `publish --dry-run` and `doc` take `--json`.
- `kite doc yaml` and `kite doc yaml/schema` work on any dependency.
- Resolution failures are explained (PubGrub), not just reported.
- One command fixes the manifest after editing imports: `kite add` with no
  arguments adds what is imported but missing, and `kite remove --unused`
  removes the rest.

## What we deliberately don't take

- **Build scripts** (`build.rs`, `Package.swift`, npm scripts): a supply-chain
  door and a build-reproducibility hole.
- **Cargo features:** conditional compilation per dependency is a large
  source of combinatorial breakage. Optional dependencies may come later.
- **Import paths in source** (Go, early Deno): see above.
- **Unscoped registry names:** squatting and typo-squatting.

## Decisions to confirm

1. **`kite.toml` + `kite.lock`, with short names in source** (Cargo/JSR).
   Decided: the standard library is `std/...`, and permissions are not part
   of the design (Cargo manages without them, and they add friction to every
   package that does I/O).
2. **A PubGrub solver with Cargo-style ranges,** major versions coexisting.
   The compiler has no third-party dependencies, so this means writing the
   solver (and a TOML subset parser) in-tree; I estimate 1,500–2,000 lines.
3. **A scoped registry with a static index,** immutable versions, OIDC
   publishing, and generated docs. Running it is a service: it needs a
   domain, object storage and a small publish API. Git and path sources ship
   first, so the beta doesn't wait for it.
4. **Compiler-checked semver on publish.**

## Implementation plan

1. **The fix and local packages.**
   - The link-name fix.
   - `kite.toml` (a TOML subset parser), package roots, path dependencies,
     dependency names shadowing std, `use std/...`.
   - `exports`, workspaces.
2. **Resolution and git sources.**
   - Semver and ranges, the PubGrub solver with explained failures.
   - `kite.lock`, the content-addressed cache with SHA-256 (in-tree).
   - Git sources, `--offline`, `--locked`.
   - `kite add`, `remove`, `update`, `tree`, `vendor`, `--json`, editions.
3. **Publishing.**
   - The API diff behind `kite publish --dry-run`.
   - The index format and archive layout.
   - The registry service and `kite publish`, once it is hosted.
4. **Tests,** all offline:
   - local index and git fixtures;
   - solver cases (diamonds, conflicts with their explanations, coexisting
     majors, pre-releases, yanked versions);
   - a hash mismatch;
   - export violations;
   - semver diffs for each kind of API change.

   Plus a fuzzer for `kite.toml` and the solver.
