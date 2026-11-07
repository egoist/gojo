// API documentation from source: `kite doc`. The public declarations of a
// package, with the comments written directly above them, as Markdown.
//
// A comment block at the top of a file, separated from what follows by a
// blank line (or followed by `use`), documents the package.

use crate::ast::{Item, ItemKind, TypeExprKind};
use crate::{lexer, parser};
use std::collections::HashMap;

/// One package's documentation. `files` are (name, source) pairs.
pub fn package(name: &str, files: &[(String, String)]) -> Result<String, String> {
    let mut intro = String::new();
    let mut decls: Vec<Decl> = vec![];
    for (fname, src) in files {
        let toks = lexer::lex(0, src).map_err(|d| format!("{}: {}", fname, d.msg))?;
        let file = parser::parse_file(0, toks).map_err(|d| format!("{}: {}", fname, d.msg))?;
        if let Some(d) = package_comment(src) {
            if !intro.is_empty() {
                intro.push('\n');
            }
            intro.push_str(&d);
        }
        collect(src, &file.items, &mut decls);
    }
    Ok(render(name, &intro, &decls))
}

/// The first sentence of a package's documentation, for an index.
pub fn summary(files: &[(String, String)]) -> String {
    let intro = files.iter().find_map(|(_, s)| package_comment(s)).unwrap_or_default();
    let flat: String = intro.lines().take_while(|l| !l.trim().is_empty()).collect::<Vec<_>>().join(" ");
    match flat.find(". ") {
        Some(i) => flat[..=i].to_string(),
        None => flat,
    }
}

// ---------------------------------------------------------------------------

enum Kind {
    Func,
    Type,
    Const,
    /// Methods on a type declared elsewhere (`extend str { ... }`).
    Extend,
}

struct Decl {
    kind: Kind,
    /// what the heading shows: `readAll`, `struct Error`, `extend Vec[T]`
    title: String,
    /// type name, for attaching methods
    key: String,
    code: String,
    doc: String,
    methods: Vec<Method>,
}

struct Method {
    name: String,
    code: String,
    doc: String,
}

fn collect(src: &str, items: &[Item], out: &mut Vec<Decl>) {
    let text = |lo: u32, hi: u32| src[lo as usize..hi as usize].trim().to_string();
    let mut methods: Vec<(String, String, Method)> = vec![];
    // the last function or constant, while the next may join it: a
    // declaration on the very next line, without a comment of its own, shares
    // its entry (`floor`, `ceil`, `round` under one comment)
    let mut last: Option<(usize, u32)> = None;
    for it in items {
        let lo = it.span.lo;
        let doc = doc_before(src, lo as usize);
        let joins = |last: Option<(usize, u32)>, out: &Vec<Decl>, kind: fn(&Kind) -> bool| match last {
            Some((i, hi)) if doc.is_empty() && kind(&out[i].kind) => {
                let gap = &src[hi as usize..lo as usize];
                (gap.matches('\n').count() == 1 && gap.trim().is_empty()).then_some(i)
            }
            _ => None,
        };
        match &it.kind {
            ItemKind::Func(f) => {
                if !it.is_pub || f.name.starts_with("__") {
                    continue;
                }
                let end = f.body.as_ref().map_or(it.span.hi, |b| b.span.lo);
                let code = text(lo, end);
                let owner = f.recv.as_ref().map(|r| &r.ty).or(f.owner.as_ref());
                match owner {
                    Some(t) => {
                        let shown = src[t.span.lo as usize..t.span.hi as usize].to_string();
                        let key = match &t.kind {
                            TypeExprKind::Named { path, .. } => path.last().cloned().unwrap_or_default(),
                            _ => shown.clone(),
                        };
                        methods.push((key, shown, Method { name: f.name.clone(), code, doc }));
                    }
                    None => {
                        if let Some(i) = joins(last, out, |k| matches!(k, Kind::Func)) {
                            out[i].title = format!("{}, {}", out[i].title, f.name);
                            out[i].code = format!("{}\n{}", out[i].code, code);
                        } else {
                            out.push(Decl { kind: Kind::Func, title: f.name.clone(), key: String::new(), code, doc, methods: vec![] });
                        }
                        last = Some((out.len() - 1, it.span.hi));
                        continue;
                    }
                }
            }
            ItemKind::Struct(s) if it.is_pub => {
                let header = header(src, lo as usize);
                let mut lines: Vec<String> = vec![];
                let mut hidden = false;
                for fd in &s.fields {
                    if !fd.is_pub {
                        hidden = true;
                        continue;
                    }
                    let l = line_at(src, fd.span.lo as usize);
                    let mut d = doc_before(src, line_start(src, fd.span.lo as usize));
                    if !lines.iter().any(|x| x.ends_with(&l)) {
                        if !d.is_empty() {
                            d = d.lines().map(|x| format!("    // {}", x).trim_end().to_string()).collect::<Vec<_>>().join("\n") + "\n";
                        }
                        lines.push(format!("{}    {}", d, l));
                    }
                }
                if hidden {
                    lines.push("    // and private fields".into());
                }
                let code = if lines.is_empty() { format!("{}}}", header) } else { format!("{}\n{}\n}}", header, lines.join("\n")) };
                out.push(Decl { kind: Kind::Type, title: format!("struct {}", s.name), key: s.name.clone(), code, doc, methods: vec![] });
            }
            ItemKind::Enum(e) if it.is_pub => {
                let header = header(src, lo as usize);
                let first = line_at(src, lo as usize);
                let code = if e.variants.iter().all(|v| line_start(src, v.span.lo as usize) == line_start(src, lo as usize)) {
                    first
                } else {
                    let mut lines: Vec<String> = vec![];
                    for v in &e.variants {
                        let l = line_at(src, v.span.lo as usize);
                        if !lines.iter().any(|x| x.ends_with(&l)) {
                            let d = doc_before(src, line_start(src, v.span.lo as usize));
                            let d: String = d.lines().map(|x| format!("    // {}\n", x)).collect();
                            lines.push(format!("{}    {}", d, l));
                        }
                    }
                    format!("{}\n{}\n}}", header, lines.join("\n"))
                };
                out.push(Decl { kind: Kind::Type, title: format!("enum {}", e.name), key: e.name.clone(), code, doc, methods: vec![] });
            }
            ItemKind::Interface(i) if it.is_pub => {
                out.push(Decl { kind: Kind::Type, title: format!("interface {}", i.name), key: i.name.clone(), code: text(lo, it.span.hi), doc, methods: vec![] });
            }
            ItemKind::TypeAlias { name, .. } if it.is_pub => {
                out.push(Decl { kind: Kind::Type, title: format!("type {}", name), key: name.clone(), code: text(lo, it.span.hi), doc, methods: vec![] });
            }
            ItemKind::Const { name, .. } if it.is_pub => {
                let code = text(lo, it.span.hi);
                if let Some(i) = joins(last, out, |k| matches!(k, Kind::Const)) {
                    out[i].title = format!("{}, {}", out[i].title, name);
                    out[i].code = format!("{}\n{}", out[i].code, code);
                } else {
                    out.push(Decl { kind: Kind::Const, title: name.clone(), key: String::new(), code, doc, methods: vec![] });
                }
                last = Some((out.len() - 1, it.span.hi));
                continue;
            }
            _ => {}
        }
        last = None;
    }
    // methods go with their type, or under `extend T`
    for (key, shown, m) in methods {
        if let Some(d) = out.iter_mut().find(|d| matches!(d.kind, Kind::Type) && d.key == key) {
            d.methods.push(m);
        } else if let Some(d) = out.iter_mut().find(|d| matches!(d.kind, Kind::Extend) && d.title == format!("extend {}", shown)) {
            d.methods.push(m);
        } else {
            out.push(Decl { kind: Kind::Extend, title: format!("extend {}", shown), key: shown, code: String::new(), doc: String::new(), methods: vec![m] });
        }
    }
}

fn render(name: &str, intro: &str, decls: &[Decl]) -> String {
    let mut out = format!("# {}\n\n", name);
    if !intro.is_empty() {
        out.push_str(&markdown(intro));
        out.push('\n');
    }
    let mut anchors: HashMap<String, usize> = HashMap::new();
    let mut anchor = |title: &str| {
        let a: String = title.to_lowercase().chars().filter_map(|c| if c == ' ' { Some('-') } else if c.is_alphanumeric() || c == '-' || c == '_' { Some(c) } else { None }).collect();
        let n = anchors.entry(a.clone()).or_insert(0);
        *n += 1;
        if *n == 1 { a } else { format!("{}-{}", a, *n - 1) }
    };
    let groups: [(&str, fn(&Kind) -> bool); 4] = [
        ("Constants", |k| matches!(k, Kind::Const)),
        ("Types", |k| matches!(k, Kind::Type)),
        ("Functions", |k| matches!(k, Kind::Func)),
        ("Methods on other types", |k| matches!(k, Kind::Extend)),
    ];
    // contents first, with the anchors the headings below will get
    let mut body = String::new();
    let mut contents = String::new();
    for (gname, is) in groups {
        let ds: Vec<&Decl> = decls.iter().filter(|d| is(&d.kind)).collect();
        if ds.is_empty() {
            continue;
        }
        body.push_str(&format!("## {}\n\n", gname));
        let mut names = vec![];
        for d in ds {
            let a = anchor(&d.title);
            names.push(format!("[`{}`](#{})", d.title, a));
            body.push_str(&format!("### {}\n\n", d.title));
            if !d.code.is_empty() {
                body.push_str(&format!("```kite\n{}\n```\n\n", d.code));
            }
            if !d.doc.is_empty() {
                body.push_str(&markdown(&d.doc));
                body.push('\n');
            }
            for m in &d.methods {
                let owner = if matches!(d.kind, Kind::Extend) { d.key.as_str() } else { d.title.split(' ').nth(1).unwrap_or("") };
                let title = format!("{}.{}", owner, m.name);
                anchor(&title);
                body.push_str(&format!("#### {}\n\n```kite\n{}\n```\n\n", title, m.code));
                if !m.doc.is_empty() {
                    body.push_str(&markdown(&m.doc));
                    body.push('\n');
                }
            }
        }
        contents.push_str(&format!("**{}:** {}\n\n", gname, names.join(", ")));
    }
    out.push_str(&contents);
    out.push_str(&body);
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// Comment text to Markdown: lines indented by two or more spaces are code.
fn markdown(doc: &str) -> String {
    let lines: Vec<&str> = doc.lines().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < lines.len() {
        if lines[i].starts_with("  ") {
            let mut j = i;
            while j < lines.len() && (lines[j].starts_with("  ") || (lines[j].trim().is_empty() && lines[j + 1..].iter().find(|l| !l.trim().is_empty()).is_some_and(|l| l.starts_with("  ")))) {
                j += 1;
            }
            let indent = lines[i..j].iter().filter(|l| !l.trim().is_empty()).map(|l| l.len() - l.trim_start().len()).min().unwrap_or(0);
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push('\n');
            }
            out.push_str("```kite\n");
            for l in &lines[i..j] {
                out.push_str(l.get(indent..).unwrap_or("").trim_end());
                out.push('\n');
            }
            out.push_str("```\n\n");
            i = j;
        } else {
            if lines[i].trim().is_empty() {
                if !out.ends_with("\n\n") && !out.is_empty() {
                    out.push('\n');
                }
            } else {
                out.push_str(lines[i].trim_end());
                out.push('\n');
            }
            i += 1;
        }
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    out
}

/// The package comment: the comment block that starts the file, when a blank
/// line or `use` follows it.
fn package_comment(src: &str) -> Option<String> {
    let mut block = vec![];
    let mut rest = None;
    for l in src.lines() {
        let t = l.trim();
        match t.strip_prefix("//") {
            Some(c) => block.push(c.strip_prefix(' ').unwrap_or(c)),
            None => {
                rest = Some(t);
                break;
            }
        }
    }
    let ok = match rest {
        Some(r) => r.is_empty() || r.starts_with("use ") || r == "use",
        None => true,
    };
    if block.is_empty() || !ok {
        return None;
    }
    Some(block.join("\n"))
}

/// The comment lines directly above the line containing `pos`.
fn doc_before(src: &str, pos: usize) -> String {
    let start = line_start(src, pos);
    let before = &src[..start];
    let mut lines: Vec<&str> = vec![];
    for l in before.lines().rev() {
        match l.trim().strip_prefix("//") {
            Some(c) => lines.push(c.strip_prefix(' ').unwrap_or(c)),
            None => break,
        }
    }
    // the package comment is not the first declaration's
    if lines.len() == before.lines().count() && package_comment(src).is_some() {
        return String::new();
    }
    lines.reverse();
    lines.join("\n")
}

fn line_start(src: &str, pos: usize) -> usize {
    src[..pos].rfind('\n').map_or(0, |i| i + 1)
}

fn line_at(src: &str, pos: usize) -> String {
    let s = line_start(src, pos);
    let e = src[pos..].find('\n').map_or(src.len(), |i| pos + i);
    src[s..e].trim().trim_end_matches(',').to_string()
}

/// A type declaration up to and including its `{`.
fn header(src: &str, lo: usize) -> String {
    let e = src[lo..].find('{').map_or(src.len(), |i| lo + i + 1);
    src[lo..e].trim().to_string()
}
