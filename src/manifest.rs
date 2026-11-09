// `gojo.toml`: the package manifest (see docs/packages.md).
//
// The compiler has no dependencies, so this is a small TOML reader of its
// own: tables (`[package]`, `[dependencies.yaml]`), bare and quoted keys,
// dotted keys, strings, integers, booleans, arrays and inline tables. That is
// all a manifest needs. Anything else is an error that says so, and so are
// unknown keys: a typo in a manifest should never be silently ignored.

use crate::diag::{Diag, SourceMap, Span};
use std::path::{Path, PathBuf};

pub const FILE: &str = "gojo.toml";
pub const EDITIONS: &[&str] = &["2026"];
pub const LATEST_EDITION: &str = "2026";

// ---------------------------------------------------------------------------
// TOML values

#[derive(Clone, Debug)]
pub enum V {
    Str(String),
    Int(i64),
    Bool(bool),
    Array(Vec<Val>),
    Table(Vec<(String, Span, Val)>),
}

#[derive(Clone, Debug)]
pub struct Val {
    pub v: V,
    pub span: Span,
}

impl Val {
    fn kind(&self) -> &'static str {
        match self.v {
            V::Str(_) => "a string",
            V::Int(_) => "an integer",
            V::Bool(_) => "a boolean",
            V::Array(_) => "an array",
            V::Table(_) => "a table",
        }
    }
}

struct Reader<'a> {
    s: &'a [u8],
    i: usize,
    file: u32,
}

type R<T> = Result<T, Diag>;

impl<'a> Reader<'a> {
    /// The end of the character at `i` (spans end on character boundaries).
    fn char_end(&self, i: usize) -> usize {
        let mut j = (i + 1).min(self.s.len());
        while j < self.s.len() && (self.s[j] & 0xC0) == 0x80 {
            j += 1;
        }
        j.max(i)
    }
    fn sp(&self, lo: usize) -> Span {
        let hi = if self.i > lo { self.i } else { self.char_end(lo) };
        Span::new(self.file, lo as u32, hi as u32)
    }
    fn err<T>(&self, msg: impl Into<String>) -> R<T> {
        Err(Diag::new(Span::new(self.file, self.i as u32, self.char_end(self.i) as u32), msg))
    }
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    /// Spaces and tabs (not newlines).
    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t')) {
            self.i += 1;
        }
    }
    fn comment(&mut self) {
        if self.peek() == Some(b'#') {
            while !matches!(self.peek(), None | Some(b'\n')) {
                self.i += 1;
            }
        }
    }
    /// Whitespace, comments and newlines (inside arrays).
    fn blank(&mut self) {
        loop {
            self.ws();
            self.comment();
            match self.peek() {
                Some(b'\n') | Some(b'\r') => self.i += 1,
                _ => return,
            }
        }
    }
    /// The end of a line: only a comment may follow.
    fn eol(&mut self) -> R<()> {
        self.ws();
        self.comment();
        match self.peek() {
            None => Ok(()),
            Some(b'\n') => {
                self.i += 1;
                Ok(())
            }
            Some(b'\r') if self.s.get(self.i + 1) == Some(&b'\n') => {
                self.i += 2;
                Ok(())
            }
            _ => self.err("expected the end of the line"),
        }
    }

    fn key(&mut self) -> R<(String, Span)> {
        self.ws();
        let lo = self.i;
        match self.peek() {
            Some(b'"') | Some(b'\'') => {
                let s = self.string()?;
                Ok((s, self.sp(lo)))
            }
            Some(c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'-' => {
                while matches!(self.peek(), Some(c) if c.is_ascii_alphanumeric() || c == b'_' || c == b'-') {
                    self.i += 1;
                }
                Ok((String::from_utf8_lossy(&self.s[lo..self.i]).into_owned(), self.sp(lo)))
            }
            _ => self.err("expected a key"),
        }
    }

    /// `a.b.c`
    fn dotted_key(&mut self) -> R<Vec<(String, Span)>> {
        let mut ks = vec![self.key()?];
        loop {
            self.ws();
            if self.peek() != Some(b'.') {
                return Ok(ks);
            }
            self.i += 1;
            ks.push(self.key()?);
        }
    }

    fn string(&mut self) -> R<String> {
        let q = self.peek().unwrap();
        if self.s[self.i..].starts_with(if q == b'"' { b"\"\"\"" } else { b"'''" }) {
            return self.err("multi-line strings are not supported in gojo.toml");
        }
        self.i += 1;
        let mut out = Vec::new();
        loop {
            match self.peek() {
                None | Some(b'\n') => return self.err("unterminated string"),
                Some(c) if c == q => {
                    self.i += 1;
                    return String::from_utf8(out).or_else(|_| self.err("a string must be UTF-8"));
                }
                Some(b'\\') if q == b'"' => {
                    self.i += 1;
                    let e = self.peek();
                    self.i += 1;
                    match e {
                        Some(b'"') => out.push(b'"'),
                        Some(b'\\') => out.push(b'\\'),
                        Some(b'n') => out.push(b'\n'),
                        Some(b't') => out.push(b'\t'),
                        Some(b'r') => out.push(b'\r'),
                        Some(b'u') | Some(b'U') => {
                            let n = if e == Some(b'u') { 4 } else { 8 };
                            let hex = std::str::from_utf8(self.s.get(self.i..self.i + n).unwrap_or(b"")).unwrap_or("");
                            let c = u32::from_str_radix(hex, 16).ok().and_then(char::from_u32);
                            let Some(c) = c else {
                                return self.err("bad unicode escape");
                            };
                            self.i += n;
                            let mut b = [0; 4];
                            out.extend_from_slice(c.encode_utf8(&mut b).as_bytes());
                        }
                        _ => {
                            self.i -= 1;
                            return self.err("unknown escape (use \\\\, \\\", \\n, \\t, \\r or \\u)");
                        }
                    }
                }
                Some(c) => {
                    out.push(c);
                    self.i += 1;
                }
            }
        }
    }

    fn value(&mut self) -> R<Val> {
        self.ws();
        let lo = self.i;
        let v = match self.peek() {
            Some(b'"') | Some(b'\'') => V::Str(self.string()?),
            Some(b'[') => {
                self.i += 1;
                let mut items = vec![];
                loop {
                    self.blank();
                    if self.peek() == Some(b']') {
                        self.i += 1;
                        break;
                    }
                    items.push(self.value()?);
                    self.blank();
                    match self.peek() {
                        Some(b',') => self.i += 1,
                        Some(b']') => {}
                        _ => return self.err("expected `,` or `]`"),
                    }
                }
                V::Array(items)
            }
            Some(b'{') => {
                self.i += 1;
                let mut t = vec![];
                self.ws();
                if self.peek() == Some(b'}') {
                    self.i += 1;
                } else {
                    loop {
                        let ks = self.dotted_key()?;
                        self.ws();
                        if self.peek() != Some(b'=') {
                            return self.err("expected `=`");
                        }
                        self.i += 1;
                        let v = self.value()?;
                        insert(&mut t, &ks, v)?;
                        self.ws();
                        match self.peek() {
                            Some(b',') => self.i += 1,
                            Some(b'}') => {
                                self.i += 1;
                                break;
                            }
                            _ => return self.err("expected `,` or `}` (an inline table is on one line)"),
                        }
                    }
                }
                V::Table(t)
            }
            Some(b't') if self.s[self.i..].starts_with(b"true") => {
                self.i += 4;
                V::Bool(true)
            }
            Some(b'f') if self.s[self.i..].starts_with(b"false") => {
                self.i += 5;
                V::Bool(false)
            }
            Some(c) if c.is_ascii_digit() || c == b'-' || c == b'+' => {
                self.i += 1;
                while matches!(self.peek(), Some(c) if c.is_ascii_digit() || c == b'_') {
                    self.i += 1;
                }
                let t: String = String::from_utf8_lossy(&self.s[lo..self.i]).replace('_', "");
                match t.parse::<i64>() {
                    Ok(n) if !matches!(self.peek(), Some(b'.' | b'e' | b'E' | b':')) => V::Int(n),
                    _ if matches!(self.peek(), Some(b'.')) => return self.err("a fraction isn't supported in gojo.toml (a version is a string: `version = \"1.0.0\"`)"),
                    _ => return self.err("only strings, integers, booleans, arrays and tables are supported in gojo.toml"),
                }
            }
            _ => return self.err("expected a value"),
        };
        Ok(Val { v, span: self.sp(lo) })
    }
}

fn insert(t: &mut Vec<(String, Span, Val)>, ks: &[(String, Span)], v: Val) -> R<()> {
    let (k, ksp) = &ks[0];
    if ks.len() == 1 {
        if t.iter().any(|(n, _, _)| n == k) {
            return Err(Diag::new(*ksp, format!("`{}` is set twice", k)));
        }
        t.push((k.clone(), *ksp, v));
        return Ok(());
    }
    let idx = match t.iter().position(|(n, _, _)| n == k) {
        Some(i) => i,
        None => {
            t.push((k.clone(), *ksp, Val { v: V::Table(vec![]), span: *ksp }));
            t.len() - 1
        }
    };
    match &mut t[idx].2.v {
        V::Table(inner) => insert(inner, &ks[1..], v),
        _ => Err(Diag::new(*ksp, format!("`{}` is not a table", k))),
    }
}

/// Parses TOML text into its top-level table.
pub fn parse_toml(file: u32, text: &str) -> R<Vec<(String, Span, Val)>> {
    let mut r = Reader { s: text.as_bytes(), i: 0, file };
    let mut root: Vec<(String, Span, Val)> = vec![];
    // the key path of the current `[table]`
    let mut current: Vec<(String, Span)> = vec![];
    let mut headers: Vec<String> = vec![];
    loop {
        r.blank();
        let Some(c) = r.peek() else { break };
        if c == b'[' {
            if r.s.get(r.i + 1) == Some(&b'[') {
                return r.err("arrays of tables (`[[...]]`) are not supported in gojo.toml");
            }
            r.i += 1;
            let ks = r.dotted_key()?;
            r.ws();
            if r.peek() != Some(b']') {
                return r.err("expected `]`");
            }
            r.i += 1;
            r.eol()?;
            let name = ks.iter().map(|k| k.0.as_str()).collect::<Vec<_>>().join(".");
            if headers.contains(&name) {
                return Err(Diag::new(ks[0].1, format!("the table `[{}]` appears twice", name)));
            }
            headers.push(name);
            // make sure the table exists
            let mut t = &mut root;
            for (k, ksp) in &ks {
                let idx = match t.iter().position(|(n, _, _)| n == k) {
                    Some(i) => i,
                    None => {
                        t.push((k.clone(), *ksp, Val { v: V::Table(vec![]), span: *ksp }));
                        t.len() - 1
                    }
                };
                t = match &mut t[idx].2.v {
                    V::Table(inner) => inner,
                    _ => return Err(Diag::new(*ksp, format!("`{}` is not a table", k))),
                };
            }
            current = ks;
            continue;
        }
        let ks = r.dotted_key()?;
        r.ws();
        if r.peek() != Some(b'=') {
            return r.err("expected `=`");
        }
        r.i += 1;
        let v = r.value()?;
        r.eol()?;
        let mut full = current.clone();
        full.extend(ks);
        insert(&mut root, &full, v)?;
    }
    Ok(root)
}

// ---------------------------------------------------------------------------
// The manifest

#[derive(Clone, Debug)]
pub enum Source {
    Path(PathBuf),
    /// Not resolved yet: a registry version requirement.
    Registry(String),
    Git(String),
}

#[derive(Clone, Debug)]
pub struct Dependency {
    /// The name it is imported by: the key in `[dependencies]`.
    pub name: String,
    /// The package's own name, when it differs (`package = "@bob/yaml"`).
    pub package: Option<String>,
    pub source: Source,
    pub span: Span,
}

#[derive(Clone, Debug)]
pub struct Package {
    pub name: String,
    pub version: String,
    pub edition: String,
    /// Subpackages other packages may import, as directories relative to the
    /// package root (`""` is the root package).
    pub exports: Vec<String>,
}

/// `[unsafe]`: which packages may contain code the compiler can't check
/// (`unsafe` blocks, `unsafe fn`, `unsafe send` types, `extern` blocks).
#[derive(Clone, Debug, Default)]
pub struct UnsafePolicy {
    /// Dependencies, anywhere in the tree, that may: by package name. Only the
    /// manifest of the package being built counts.
    pub allow: Vec<(String, Span)>,
    /// This package's own code may not (`forbid = true`).
    pub forbid: bool,
    /// Where the table is, when there is one.
    pub span: Option<Span>,
}

#[derive(Clone, Debug)]
pub struct Manifest {
    /// The directory holding `gojo.toml`.
    pub root: PathBuf,
    pub file: u32,
    pub package: Option<Package>,
    pub dependencies: Vec<Dependency>,
    /// Member directories of a workspace (globs expanded).
    pub members: Vec<PathBuf>,
    pub unsafe_policy: UnsafePolicy,
    pub authority: AuthorityPolicy,
}

/// The `[authority]` table: which kinds of I/O a dependency may do (see
/// `authority`). Only the manifest of the package being built counts.
#[derive(Clone, Debug, Default)]
pub struct AuthorityPolicy {
    /// Package name (or `*`: every dependency not listed), where it is, and
    /// the kinds it may use.
    pub grants: Vec<(String, Span, Vec<String>)>,
    /// Where the table is, when there is one.
    pub span: Option<Span>,
}

impl Manifest {
    pub fn dependency(&self, name: &str) -> Option<&Dependency> {
        self.dependencies.iter().find(|d| d.name == name)
    }
}

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_') && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `name` or `@scope/name`, in lower case.
pub fn valid_package_name(s: &str) -> bool {
    let part = |p: &str| !p.is_empty() && p.len() <= 64 && p.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') && !p.starts_with('-');
    match s.strip_prefix('@') {
        Some(rest) => matches!(rest.split_once('/'), Some((scope, name)) if part(scope) && part(name)),
        None => part(s),
    }
}

/// `1.2.3`, with an optional pre-release (`1.2.3-beta.1`).
pub fn valid_version(s: &str) -> bool {
    let (core, pre) = match s.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (s, None),
    };
    let nums: Vec<&str> = core.split('.').collect();
    nums.len() == 3
        && nums.iter().all(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()) && (n.len() == 1 || !n.starts_with('0')))
        && pre.is_none_or(|p| !p.is_empty() && p.split('.').all(|x| !x.is_empty() && x.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')))
}

/// The nearest directory at or above `dir` that holds a `gojo.toml`.
pub fn find_root(dir: &Path) -> Option<PathBuf> {
    let mut d = dir.canonicalize().ok()?;
    loop {
        if d.join(FILE).is_file() {
            return Some(d);
        }
        if !d.pop() {
            return None;
        }
    }
}

/// Reads and checks `<root>/gojo.toml`.
pub fn read(sm: &mut SourceMap, root: &Path) -> Result<Manifest, Vec<Diag>> {
    let path = root.join(FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => {
            let f = sm.add(path.display().to_string(), String::new());
            return Err(vec![Diag::new(Span::new(f, 0, 0), format!("cannot read {}: {}", path.display(), e))]);
        }
    };
    from_text(sm, root, path.display().to_string(), text)
}

/// Checks manifest text as if it were `<root>/gojo.toml`.
pub fn from_text(sm: &mut SourceMap, root: &Path, name: String, text: String) -> Result<Manifest, Vec<Diag>> {
    let file = sm.add(name, text.clone());
    let top = parse_toml(file, &text).map_err(|d| vec![d])?;
    let mut diags = vec![];
    let mut m = Manifest { root: root.to_path_buf(), file, package: None, dependencies: vec![], members: vec![], unsafe_policy: UnsafePolicy::default(), authority: AuthorityPolicy::default() };
    let whole = Span::new(file, 0, 0);
    for (k, ksp, v) in &top {
        match (k.as_str(), &v.v) {
            ("package", V::Table(t)) => m.package = check_package(root, t, *ksp, &mut diags),
            ("dependencies", V::Table(t)) => m.dependencies = check_dependencies(root, t, &mut diags),
            ("workspace", V::Table(t)) => m.members = check_workspace(root, t, &mut diags),
            ("unsafe", V::Table(t)) => m.unsafe_policy = check_unsafe(t, *ksp, &mut diags),
            ("authority", V::Table(t)) => m.authority = check_authority(t, *ksp, &mut diags),
            ("package" | "dependencies" | "workspace" | "unsafe" | "authority", _) => diags.push(Diag::new(*ksp, format!("`{}` must be a table: `[{}]`", k, k))),
            _ => diags.push(Diag::new(*ksp, format!("unknown section `{}` (gojo.toml has [package], [dependencies], [unsafe], [authority] and [workspace])", k))),
        }
    }
    if m.package.is_none() && m.members.is_empty() && diags.is_empty() {
        diags.push(Diag::new(whole, "gojo.toml needs a [package] (or a [workspace] with members)"));
    }
    if m.package.is_none() && !m.dependencies.is_empty() {
        diags.push(Diag::new(whole, "[dependencies] belong to a [package]"));
    }
    if let (None, Some(sp)) = (&m.package, m.unsafe_policy.span) {
        diags.push(Diag::new(sp, "[unsafe] belongs to a [package]: each member of a workspace has its own"));
    }
    if let (None, Some(sp)) = (&m.package, m.authority.span) {
        diags.push(Diag::new(sp, "[authority] belongs to a [package]: each member of a workspace has its own"));
    }
    if diags.is_empty() { Ok(m) } else { Err(diags) }
}

fn string_of<'v>(v: &'v Val, what: &str, diags: &mut Vec<Diag>) -> Option<&'v str> {
    match &v.v {
        V::Str(s) => Some(s),
        _ => {
            diags.push(Diag::new(v.span, format!("`{}` must be a string, not {}", what, v.kind())));
            None
        }
    }
}

fn check_package(root: &Path, t: &[(String, Span, Val)], sp: Span, diags: &mut Vec<Diag>) -> Option<Package> {
    let mut name = None;
    let mut version = "0.1.0".to_string();
    let mut edition = LATEST_EDITION.to_string();
    let mut exports = vec![String::new()];
    for (k, ksp, v) in t {
        match k.as_str() {
            "name" => {
                if let Some(s) = string_of(v, "name", diags) {
                    if valid_package_name(s) {
                        name = Some(s.to_string());
                    } else {
                        diags.push(Diag::new(v.span, format!("`{}` is not a package name: lower-case letters, digits, `-` and `_`, optionally scoped (`@scope/name`)", s)));
                    }
                }
            }
            "version" => {
                if let Some(s) = string_of(v, "version", diags) {
                    if valid_version(s) {
                        version = s.to_string();
                    } else {
                        diags.push(Diag::new(v.span, format!("`{}` is not a version: write MAJOR.MINOR.PATCH, like `0.1.0`", s)));
                    }
                }
            }
            "edition" => {
                if let Some(s) = string_of(v, "edition", diags) {
                    if EDITIONS.contains(&s) {
                        edition = s.to_string();
                    } else {
                        diags.push(Diag::new(v.span, format!("unknown edition `{}` (this compiler knows {})", s, EDITIONS.join(", "))));
                    }
                }
            }
            "exports" => match &v.v {
                V::Array(items) => {
                    exports.clear();
                    for it in items {
                        let Some(s) = string_of(it, "exports", diags) else { continue };
                        let p = s.trim_start_matches("./").trim_end_matches('/');
                        let p = if p == "." { "" } else { p };
                        if p.split('/').any(|seg| seg == ".." || seg == ".") || p.starts_with('/') {
                            diags.push(Diag::new(it.span, "an export is a directory inside the package: `\".\"` or `\"schema\"`"));
                        } else if !root.join(p).is_dir() {
                            diags.push(Diag::new(it.span, format!("there is no directory `{}` in this package", s)));
                        } else if !exports.iter().any(|e| e == p) {
                            exports.push(p.to_string());
                        }
                    }
                }
                _ => diags.push(Diag::new(v.span, "`exports` is a list of directories: `exports = [\".\", \"schema\"]`")),
            },
            "description" | "license" | "repository" | "homepage" => {
                string_of(v, k, diags);
            }
            "authors" | "keywords" => {
                if !matches!(&v.v, V::Array(xs) if xs.iter().all(|x| matches!(x.v, V::Str(_)))) {
                    diags.push(Diag::new(v.span, format!("`{}` is a list of strings", k)));
                }
            }
            _ => diags.push(Diag::new(*ksp, format!("unknown key `{}` in [package] (known: name, version, edition, exports, description, license, repository, homepage, authors, keywords)", k))),
        }
    }
    match name {
        Some(name) => Some(Package { name, version, edition, exports }),
        None => {
            if !diags.iter().any(|d| d.msg.contains("package name")) {
                diags.push(Diag::new(sp, "[package] needs a `name`"));
            }
            None
        }
    }
}

fn check_unsafe(t: &[(String, Span, Val)], sp: Span, diags: &mut Vec<Diag>) -> UnsafePolicy {
    let mut p = UnsafePolicy { span: Some(sp), ..UnsafePolicy::default() };
    for (k, ksp, v) in t {
        match (k.as_str(), &v.v) {
            ("allow", V::Array(items)) => {
                for it in items {
                    let Some(s) = string_of(it, "allow", diags) else { continue };
                    if !valid_package_name(s) {
                        diags.push(Diag::new(it.span, format!("`{}` is not a package name (`allow` lists the dependencies that may use unsafe code, by the name in their gojo.toml)", s)));
                    } else if p.allow.iter().any(|(n, _)| n == s) {
                        diags.push(Diag::new(it.span, format!("`{}` is listed twice", s)));
                    } else {
                        p.allow.push((s.to_string(), it.span));
                    }
                }
            }
            ("allow", _) => diags.push(Diag::new(v.span, "`allow` is a list of package names: `allow = [\"zlib\"]`")),
            ("forbid", V::Bool(b)) => p.forbid = *b,
            ("forbid", _) => diags.push(Diag::new(v.span, "`forbid` is `true` or `false`")),
            _ => diags.push(Diag::new(*ksp, format!("unknown key `{}` in [unsafe] (known: allow, forbid)", k))),
        }
    }
    p
}

fn check_authority(t: &[(String, Span, Val)], sp: Span, diags: &mut Vec<Diag>) -> AuthorityPolicy {
    let mut p = AuthorityPolicy { span: Some(sp), ..AuthorityPolicy::default() };
    let kinds = crate::authority::KINDS.iter().map(|(k, _)| format!("\"{}\"", k)).collect::<Vec<_>>().join(", ");
    for (k, ksp, v) in t {
        if k != "*" && !valid_package_name(k) {
            diags.push(Diag::new(*ksp, format!("`{}` is not a package name ([authority] lists dependencies by the name in their gojo.toml, or `\"*\"` for every one not listed)", k)));
            continue;
        }
        let V::Array(items) = &v.v else {
            diags.push(Diag::new(v.span, format!("`{}` takes a list of what it may do: `{} = [\"net\"]` (known: {}), or `[]` for nothing", k, if k == "*" { "\"*\"" } else { k }, kinds)));
            continue;
        };
        let mut granted: Vec<String> = vec![];
        for it in items {
            let Some(s) = string_of(it, k, diags) else { continue };
            if !crate::authority::KINDS.iter().any(|(n, _)| *n == s) {
                diags.push(Diag::new(it.span, format!("unknown authority `{}` (known: {})", s, kinds)));
            } else if granted.iter().any(|g| g == s) {
                diags.push(Diag::new(it.span, format!("`{}` is listed twice", s)));
            } else {
                granted.push(s.to_string());
            }
        }
        p.grants.push((k.clone(), *ksp, granted));
    }
    p
}

fn check_dependencies(root: &Path, t: &[(String, Span, Val)], diags: &mut Vec<Diag>) -> Vec<Dependency> {
    let mut out = vec![];
    for (k, ksp, v) in t {
        if !is_ident(k) {
            let suggestion: String = k.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '_' }).collect();
            diags.push(Diag::new(*ksp, format!("`{}` can't be imported with `use` (a dependency's name is an identifier): name it `{} = {{ package = \"{}\", ... }}`", k, suggestion, k)));
            continue;
        }
        if k == "std" {
            diags.push(Diag::new(*ksp, "`std` is the standard library: a dependency needs another name"));
            continue;
        }
        let mut package = None;
        let mut source = None;
        match &v.v {
            V::Str(req) => source = Some(Source::Registry(req.clone())),
            V::Table(fields) => {
                let mut path = None;
                let mut version = None;
                let mut git = None;
                let mut refs = 0;
                for (fk, fsp, fv) in fields {
                    match fk.as_str() {
                        "path" => path = string_of(fv, "path", diags).map(|s| (s.to_string(), fv.span)),
                        "version" => version = string_of(fv, "version", diags).map(|s| s.to_string()),
                        "package" => {
                            if let Some(s) = string_of(fv, "package", diags) {
                                if valid_package_name(s) {
                                    package = Some(s.to_string());
                                } else {
                                    diags.push(Diag::new(fv.span, format!("`{}` is not a package name", s)));
                                }
                            }
                        }
                        "git" => git = string_of(fv, "git", diags).map(|s| s.to_string()),
                        "tag" | "rev" | "branch" => {
                            refs += 1;
                            string_of(fv, fk, diags);
                        }
                        _ => diags.push(Diag::new(*fsp, format!("unknown key `{}` in dependency `{}` (known: path, version, package, git, tag, rev, branch)", fk, k))),
                    }
                }
                if refs > 1 {
                    diags.push(Diag::new(v.span, "give one of `tag`, `rev` and `branch`"));
                }
                source = match (path, git, version) {
                    (Some((p, psp)), None, _) => {
                        let dir = root.join(&p);
                        match dir.canonicalize() {
                            Ok(d) if d.join(FILE).is_file() => Some(Source::Path(d)),
                            Ok(_) => {
                                diags.push(Diag::new(psp, format!("`{}` has no gojo.toml: a dependency is a package with its own manifest", p)));
                                None
                            }
                            Err(_) => {
                                diags.push(Diag::new(psp, format!("there is no directory `{}` (relative to {})", p, root.display())));
                                None
                            }
                        }
                    }
                    (None, Some(g), _) => Some(Source::Git(g)),
                    (None, None, Some(req)) => Some(Source::Registry(req)),
                    (Some(_), Some(_), _) => {
                        diags.push(Diag::new(v.span, "give either `path` or `git`, not both"));
                        None
                    }
                    (None, None, None) => {
                        diags.push(Diag::new(v.span, format!("where does `{}` come from? give a `version`, a `path` or a `git` URL", k)));
                        None
                    }
                };
            }
            _ => diags.push(Diag::new(v.span, format!("a dependency is a version (`{} = \"1.4\"`) or a table (`{} = {{ path = \"../{}\" }}`)", k, k, k))),
        }
        if let Some(source) = source {
            out.push(Dependency { name: k.clone(), package, source, span: *ksp });
        }
    }
    out
}

fn check_workspace(root: &Path, t: &[(String, Span, Val)], diags: &mut Vec<Diag>) -> Vec<PathBuf> {
    let mut members = vec![];
    for (k, ksp, v) in t {
        if k != "members" {
            diags.push(Diag::new(*ksp, format!("unknown key `{}` in [workspace] (known: members)", k)));
            continue;
        }
        let V::Array(items) = &v.v else {
            diags.push(Diag::new(v.span, "`members` is a list of directories: `members = [\"app\", \"libs/*\"]`"));
            continue;
        };
        for it in items {
            let Some(s) = string_of(it, "members", diags) else { continue };
            // `dir`, or `dir/*`: every subdirectory with a gojo.toml
            let found: Vec<PathBuf> = match s.strip_suffix("/*") {
                Some(parent) => {
                    let mut ds: Vec<PathBuf> = std::fs::read_dir(root.join(parent)).map(|rd| rd.filter_map(|e| e.ok().map(|e| e.path())).filter(|p| p.join(FILE).is_file()).collect()).unwrap_or_default();
                    ds.sort();
                    ds
                }
                None if s.contains('*') => {
                    diags.push(Diag::new(it.span, "only a final `/*` is supported in member patterns"));
                    continue;
                }
                None => vec![root.join(s)],
            };
            if found.is_empty() {
                diags.push(Diag::new(it.span, format!("`{}` matches no package", s)));
            }
            for d in found {
                match d.canonicalize() {
                    Ok(c) if c.join(FILE).is_file() => {
                        if !members.contains(&c) {
                            members.push(c);
                        }
                    }
                    _ => diags.push(Diag::new(it.span, format!("`{}` is not a package (no gojo.toml in {})", s, d.display()))),
                }
            }
        }
    }
    members
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toml(s: &str) -> Vec<(String, Span, Val)> {
        parse_toml(0, s).unwrap_or_else(|d| panic!("{}", d.msg))
    }

    #[test]
    fn reads_toml() {
        let t = toml("# c\n[package]\nname = \"@ann/notes\" # c\nversion='1.0.0'\n\n[dependencies]\nyaml = \"1.4\"\nstore = { path = \"../store\", package = \"x\" }\n[dependencies.parse]\ngit = \"https://x\"\n[workspace]\nmembers = [\n  \"a\", # one\n  \"b/*\",\n]\nn = -1_000\nb = true\n");
        assert_eq!(t.len(), 3);
        let V::Table(deps) = &t[1].2.v else { panic!() };
        assert_eq!(deps.iter().map(|d| d.0.as_str()).collect::<Vec<_>>(), ["yaml", "store", "parse"]);
        let V::Table(ws) = &t[2].2.v else { panic!() };
        assert!(matches!(&ws[0].2.v, V::Array(a) if a.len() == 2));
        assert!(matches!(ws[1].2.v, V::Int(-1000)));
        assert!(matches!(ws[2].2.v, V::Bool(true)));
        let d = toml("a.b = 1\na.c = \"\\u00e9\\n\"\n");
        let V::Table(a) = &d[0].2.v else { panic!() };
        assert!(matches!(&a[1].2.v, V::Str(s) if s == "é\n"));
    }

    #[test]
    fn rejects() {
        for bad in ["a = 1\na = 2\n", "[a]\n[a]\n", "a = 1.5\n", "a = \"x\n", "[[a]]\n", "a = { b = 1\n", "a = 1 b\n", "a = \"\"\"x\"\"\"\n", "a = 2026-11-07\n", "= 1\n"] {
            assert!(parse_toml(0, bad).is_err(), "{:?} should be rejected", bad);
        }
    }

    #[test]
    fn names_and_versions() {
        for ok in ["yaml", "@ann/notes", "gojo-yaml", "a_b2"] {
            assert!(valid_package_name(ok), "{}", ok);
        }
        for bad in ["Yaml", "@ann", "@/x", "a/b", "-x", "", "@a/b/c"] {
            assert!(!valid_package_name(bad), "{}", bad);
        }
        for ok in ["0.1.0", "10.20.30", "1.0.0-beta.1", "2.0.0-rc-1"] {
            assert!(valid_version(ok), "{}", ok);
        }
        for bad in ["1.0", "01.0.0", "1.0.0-", "v1.0.0", "1.0.0-a..b", "1.0.x"] {
            assert!(!valid_version(bad), "{}", bad);
        }
    }

    #[test]
    fn authority_table() {
        let mut sm = SourceMap::default();
        let root = Path::new("/nonexistent");
        let m = from_text(&mut sm, root, "a.toml".into(), "[package]\nname = \"app\"\n[authority]\nleftpad = []\n\"@c/s3\" = [\"net\", \"fs\"]\n\"*\" = [\"env\"]\n".into()).unwrap_or_else(|d| panic!("{}", d[0].msg));
        let g: Vec<(&str, Vec<&str>)> = m.authority.grants.iter().map(|(n, _, k)| (n.as_str(), k.iter().map(|s| s.as_str()).collect())).collect();
        assert_eq!(g, vec![("leftpad", vec![]), ("@c/s3", vec!["net", "fs"]), ("*", vec!["env"])]);
        for (bad, msg) in [
            ("[package]\nname = \"app\"\n[authority]\nx = [\"disk\"]\n", "unknown authority `disk`"),
            ("[package]\nname = \"app\"\n[authority]\nx = [\"net\", \"net\"]\n", "listed twice"),
            ("[package]\nname = \"app\"\n[authority]\nx = \"net\"\n", "takes a list"),
            ("[package]\nname = \"app\"\n[authority]\nX = []\n", "not a package name"),
            ("authority = 1\n[package]\nname = \"app\"\n", "must be a table"),
            ("[workspace]\nmembers = []\n[authority]\nx = []\n", "belongs to a [package]"),
        ] {
            let errs = from_text(&mut sm, root, "b.toml".into(), bad.into()).err().unwrap_or_else(|| panic!("{:?} should be rejected", bad));
            assert!(errs.iter().any(|d| d.msg.contains(msg)), "{:?}: {:?}", bad, errs.iter().map(|d| &d.msg).collect::<Vec<_>>());
        }
    }
}
