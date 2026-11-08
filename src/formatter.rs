// `gojo fmt`: the one layout for Gojo code.
//
// Works on tokens, not on the syntax tree, so comments stay where they are and
// even code that doesn't type-check (only lex) can be formatted. It keeps the
// line structure of the source (in Gojo a line break can end a statement), and
// normalizes everything else:
//
//  - indentation: four spaces per open bracket (once per line, however many
//    brackets a line opens), one more for a line continuing an expression that
//    the previous line ended with an operator;
//  - spacing: one space between words and around binary operators, `=>` and
//    `=`; none inside `()` and `[]`, before `,` `;` `:` `.`, after unary
//    operators, around `..`; `P{x: 1}` for literals and `{ x }` for blocks;
//  - at most one blank line in a row, none at the start or end of a block or
//    of the file, no trailing spaces, one newline at the end.
//
// The result must lex to the same tokens as the input, or `format` refuses.

use crate::lexer::{self, Kw, Tok, Token};

enum Item {
    Tok(usize),
    Comment(String),
}

struct Entry {
    item: Item,
    /// line breaks between this and what came before
    newlines: usize,
}

fn split_gaps(src: &str, toks: &[Token]) -> Vec<Entry> {
    let mut out = vec![];
    let mut pos = 0usize;
    let mut pending_nl = 0usize;
    let b = src.as_bytes();
    let scan = |from: usize, to: usize, out: &mut Vec<Entry>, pending_nl: &mut usize| {
        let mut i = from;
        while i < to {
            if b[i] == b'\n' {
                *pending_nl += 1;
                i += 1;
            } else if b[i] == b'/' && i + 1 < to && b[i + 1] == b'/' {
                let start = i;
                while i < to && b[i] != b'\n' {
                    i += 1;
                }
                out.push(Entry { item: Item::Comment(src[start..i].trim_end().to_string()), newlines: std::mem::take(pending_nl) });
            } else if b[i] == b'/' && i + 1 < to && b[i + 1] == b'*' {
                let start = i;
                i += 2;
                while i + 1 < to && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(to);
                out.push(Entry { item: Item::Comment(src[start..i].to_string()), newlines: std::mem::take(pending_nl) });
            } else {
                i += 1;
            }
        }
    };
    for (k, t) in toks.iter().enumerate() {
        if matches!(t.tok, Tok::Newline | Tok::Eof) {
            continue;
        }
        let (lo, hi) = (t.span.lo as usize, t.span.hi as usize);
        scan(pos, lo, &mut out, &mut pending_nl);
        out.push(Entry { item: Item::Tok(k), newlines: std::mem::take(&mut pending_nl) });
        pos = hi;
    }
    scan(pos, src.len(), &mut out, &mut pending_nl);
    out
}

/// A line starting with `.name` that continues the expression above it (a
/// method chain), as the parser reads it: not a match arm (`.A(x) => ...`).
fn dot_continues(toks: &[Token], k: usize) -> bool {
    if k == 0 || toks[k - 1].tok != Tok::Newline || toks[k].tok != Tok::P(".") || !matches!(toks.get(k + 1).map(|t| &t.tok), Some(Tok::Ident(_))) {
        return false;
    }
    let mut depth = 0i32;
    for t in &toks[k..] {
        match t.tok {
            Tok::P("(" | "[" | "{") => depth += 1,
            Tok::P(")" | "]" | "}") => {
                depth -= 1;
                if depth < 0 {
                    return true;
                }
            }
            Tok::P("=>") if depth == 0 => return false,
            Tok::Newline | Tok::Eof if depth == 0 => return true,
            _ => {}
        }
    }
    true
}

fn is_word(t: &Tok) -> bool {
    matches!(t, Tok::Ident(_) | Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::Interp(_) | Tok::Char(_) | Tok::Kw(_))
}

/// Ends a value: after it, `-` `&` `*` `!` `<-` are binary.
fn ends_value(t: &Tok) -> bool {
    match t {
        Tok::Ident(_) | Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::Interp(_) | Tok::Char(_) => true,
        Tok::Kw(k) => matches!(k, Kw::True | Kw::False | Kw::None),
        Tok::P(p) => matches!(*p, ")" | "]" | "}"),
        _ => false,
    }
}

const BINARY: &[&str] = &[
    "+", "-", "*", "/", "%", "+%", "-%", "*%", "&", "|", "^", "<<", ">>", "&&", "||", "==", "!=", "<", "<=", ">", ">=", "??", "=", "+=", "-=", "*=", "/=", "%=",
    "&=", "|=", "^=", "<<=", ">>=", "+%=", "-%=", "*%=", "=>", "->", "<-", "!",
];

/// Keywords whose statement's `{` opens a block or a declaration body.
fn header_kw(k: Kw) -> bool {
    matches!(k, Kw::If | Kw::For | Kw::Match | Kw::With | Kw::Select | Kw::Else | Kw::Struct | Kw::Enum | Kw::Interface | Kw::Extend | Kw::Func | Kw::Test | Kw::Unsafe | Kw::Extern | Kw::Catch | Kw::Defer | Kw::ErrDefer | Kw::Scope | Kw::Isolate)
}

struct Open {
    /// `(`, `[`, `{`
    kind: &'static str,
    /// the indentation of the line it is on
    line_indent: usize,
    /// a `{` of a struct or map literal (no spaces inside)
    literal: bool,
    /// the enclosing level's header flag, restored when it closes
    saved_header: bool,
}

/// Formats a whole source file. Like gofmt, only well-formed files are
/// formatted: the layout follows from their structure.
pub fn format(src: &str) -> Result<String, String> {
    let toks = lexer::lex(0, src).map_err(|d| d.msg)?;
    if let Err(d) = crate::parser::parse_file(0, toks) {
        let line = src[..(d.span.lo as usize).min(src.len())].matches('\n').count() + 1;
        return Err(format!("line {}: {} (only files without syntax errors are formatted)", line, d.msg));
    }
    layout(src)
}

/// Lays out any token sequence, well-formed or not.
fn layout(src: &str) -> Result<String, String> {
    let toks = lexer::lex(0, src).map_err(|d| d.msg)?;
    let entries = split_gaps(src, &toks);
    let text = |k: usize| &src[toks[k].span.lo as usize..toks[k].span.hi as usize];
    let mut out = String::new();
    let mut stack: Vec<Open> = vec![];
    // the current line
    let mut line_indent = 0usize;
    let mut at_line_start = true;
    let mut first = true;
    // what came last on the current line
    let mut prev: Option<Tok> = None;
    let mut prev_comment = false;
    let mut prev_binary = false;
    // the previous line ended with a binary operator: this one continues it
    let mut line_continues = false;
    // a header keyword since the statement began, at this bracket level
    let mut header = false;
    // on a `use` line: paths like `./geom` and `os.{exit}` keep their spacing
    let mut use_line = false;
    // the bracket depth where the current `use` began: its group's lines
    // (`use std/{` ... `}`) are part of it
    let mut use_depth: Option<usize> = None;
    for (ei, e) in entries.iter().enumerate() {
        let closer = matches!(&e.item, Item::Tok(k) if matches!(toks[*k].tok, Tok::P(")") | Tok::P("]") | Tok::P("}")));
        if e.newlines > 0 && !first {
            // a blank line at most, and none right inside brackets
            let after_open = matches!(prev, Some(Tok::P("{")) | Some(Tok::P("(")) | Some(Tok::P("["))) && !prev_comment;
            let n = if after_open || closer { 1 } else { e.newlines.min(2) };
            for _ in 0..n {
                out.push('\n');
            }
            line_continues = (prev_binary && !prev_comment) || matches!(&e.item, Item::Tok(k) if dot_continues(&toks, *k));
            at_line_start = true;
            // a new statement, unless inside parentheses or continuing
            if !line_continues && matches!(stack.last().map(|o| o.kind), None | Some("{")) {
                header = false;
            }
        }
        if at_line_start {
            let starts_use = matches!(&e.item, Item::Tok(k) if toks[*k].tok == Tok::Kw(Kw::Use));
            if starts_use {
                use_depth = Some(stack.len());
            } else if use_depth.is_some_and(|d| stack.len() <= d && !matches!(&e.item, Item::Tok(k) if toks[*k].tok == Tok::P("}"))) {
                use_depth = None;
            }
            use_line = use_depth.is_some();
            let base = match stack.last() {
                Some(o) => o.line_indent + 1,
                None => 0,
            };
            line_indent = if closer { stack.last().map(|o| o.line_indent).unwrap_or(0) } else { base + usize::from(line_continues) };
            for _ in 0..line_indent {
                out.push_str("    ");
            }
        }
        match &e.item {
            Item::Comment(c) => {
                if !at_line_start {
                    out.push(' ');
                }
                out.push_str(c);
                prev_comment = true;
                prev_binary = false;
            }
            Item::Tok(k) => {
                let t = &toks[*k].tok;
                let p_txt: Option<&str> = match &prev {
                    Some(Tok::P(p)) => Some(*p),
                    _ => None,
                };
                // Where only the parser could tell, the source's spacing decides:
                // whether there is space before this token, and after it.
                // (a line break counts as space; after one that ended a
                // statement, an operator can only be a prefix)
                let ended = *k > 0 && toks[*k - 1].tok == Tok::Newline;
                let space_before = *k > 0 && (ended || toks[*k].span.lo > toks[*k - 1].span.hi);
                // unary or binary? After a value it is binary (types follow `:`
                // or `->`, so `*u8` never comes right after a value)
                let binary = match t {
                    Tok::P(p) if BINARY.contains(p) => {
                        if matches!(*p, "-" | "&" | "*" | "!" | "<-") {
                            !ended && prev.as_ref().is_some_and(ends_value) && !prev_comment
                        } else {
                            true
                        }
                    }
                    _ => false,
                };
                let unary_prev = matches!(p_txt, Some("-" | "&" | "*" | "!" | "<-" | "~" | "?")) && !prev_binary;
                if !at_line_start {
                    let space = if prev_comment {
                        true
                    } else if use_line {
                        space_before
                    } else {
                        let pt = prev.as_ref().unwrap();
                        match (pt, t) {
                            (_, Tok::P(")" | "]" | "," | ";" | "?." | ":")) => false,
                            // member access after a value; `.Variant` elsewhere
                            (_, Tok::P(".")) if ends_value(pt) => false,
                            (Tok::P("(" | "[" | "." | "?." | ".." | "..="), _) => false,
                            (_, Tok::P(".." | "..=")) => false,
                            (Tok::P("," | ";" | ":"), _) => true,
                            _ if unary_prev => false,
                            // `if (`, but `g.go(` and `fn extend(`: keywords as names
                            (Tok::Kw(Kw::Func), Tok::P("(")) => false,
                            (Tok::Kw(_), Tok::P("(" | "[")) => space_before,
                            (_, Tok::P("(")) => !(matches!(pt, Tok::Ident(_) | Tok::P(")" | "]" | "}"))),
                            // `f()[0]`, `xs[1]`, `Vec[int]`
                            (Tok::P(")") | Tok::Str(_) | Tok::Interp(_) | Tok::Ident(_), Tok::P("[")) => false,
                            (_, Tok::P("[")) => !(matches!(pt, Tok::P(")" | "]"))),
                            (Tok::P("]"), Tok::Ident(_) | Tok::Kw(Kw::Func)) => false,
                            (_, Tok::P("{")) => !(matches!(pt, Tok::Ident(_) | Tok::P("]")) && !header),
                            (Tok::P("{"), Tok::P("}")) => false,
                            (_, Tok::P("}")) => !stack.last().is_some_and(|o| o.literal),
                            (Tok::P("{"), _) => !stack.last().is_some_and(|o| o.literal),
                            _ if binary => true,
                            (Tok::P(p), _) if BINARY.contains(p) && prev_binary => true,
                            // an element type: `[]?String`, `[3]?int`
                            (Tok::P("]"), Tok::P("?")) => false,
                            (_, Tok::P("?" | "~")) => true,
                            (Tok::P("?" | "~"), _) => false,
                            _ => is_word(pt) || is_word(t) || matches!(pt, Tok::P(")" | "]" | "}")),
                        }
                    };
                    // never glue two tokens into another one: `& &a` is not `&&a`
                    // (and `- -1` doesn't read as `--1`)
                    if space || (*k > 0 && (glues(text(*k - 1), text(*k)) || (text(*k - 1) == "-" && text(*k).starts_with('-')))) {
                        out.push(' ');
                    }
                }
                out.push_str(text(*k));
                match t {
                    Tok::P(p @ ("(" | "[" | "{")) => {
                        let literal = *p == "{" && ((matches!(prev, Some(Tok::Ident(_)) | Some(Tok::P("]"))) && !header) || prev == Some(Tok::P("."))) && !prev_comment;
                        stack.push(Open { kind: p, line_indent, literal, saved_header: header });
                        header = false;
                    }
                    Tok::P(")" | "]" | "}") => {
                        if let Some(o) = stack.pop() {
                            header = o.saved_header;
                        }
                    }
                    Tok::Kw(k) if header_kw(*k) => header = true,
                    Tok::P(";") if matches!(stack.last().map(|o| o.kind), None | Some("{")) => header = false,
                    _ => {}
                }
                prev = Some(t.clone());
                prev_comment = false;
                prev_binary = binary;
            }
        }
        at_line_start = false;
        first = false;
        let _ = ei;
    }
    let mut out: String = out.lines().map(|l| l.trim_end()).collect::<Vec<_>>().join("\n");
    out = out.trim_start_matches('\n').trim_end().to_string();
    out.push('\n');
    if same_tokens(src, &out) {
        Ok(out)
    } else {
        Err("internal error: formatting would change the meaning of this file; it was left alone (please report it)".into())
    }
}

/// The two texts have the same tokens (statement ends included).
/// Whether `a` and `b` written together lex as something other than these two
/// tokens (`&` `&` as `&&`, `<` `-` as `<-`, `-` `1` stays two).
fn glues(a: &str, b: &str) -> bool {
    let joined = format!("{}{}", a, b);
    match lexer::lex(0, &joined) {
        Ok(ts) => {
            let real: Vec<_> = ts.iter().filter(|t| !matches!(t.tok, Tok::Newline | Tok::Eof)).collect();
            real.len() != 2 || real[0].span.hi as usize != a.len()
        }
        Err(_) => true,
    }
}

pub fn same_tokens(a: &str, b: &str) -> bool {
    let norm = |s: &str| -> Option<Vec<Tok>> {
        let ts = lexer::lex(0, s).ok()?;
        let mut v: Vec<Tok> = vec![];
        for t in ts {
            if t.tok == Tok::Newline && v.last() == Some(&Tok::Newline) {
                continue;
            }
            v.push(t.tok);
        }
        Some(v)
    };
    norm(a).is_some() && norm(a) == norm(b)
}

#[cfg(test)]
mod tests {
    use super::{format, layout};

    #[track_caller]
    fn check(input: &str, want: &str) {
        let got = layout(input).unwrap();
        assert_eq!(got, want, "\n--- got\n{}--- want\n{}", got, want);
        assert_eq!(layout(&got).unwrap(), got, "not idempotent");
    }

    #[test]
    fn lays_out() {
        check("fn main(){\nlet x=1+2*3\n  println( x,-x )\n}", "fn main() {\n    let x = 1 + 2 * 3\n    println(x, -x)\n}\n");
        check("let p=P {x:1,y:2}\n", "let p = P{x: 1, y: 2}\n");
        check("if a==b{ x() }\n", "if a == b { x() }\n");
        check("for i in 0 .. n {}\n", "for i in 0..n {}\n");
        check("let v=xs[ 1 ]\nlet t:[ ]int=[1,2]\n", "let v = xs[1]\nlet t: []int = [1, 2]\n");
        check("x  // note\n\n\n\ny\n", "x // note\n\ny\n");
        check("f(a,\nb)\n", "f(a,\n    b)\n");
        check("let s = a +\nb\n", "let s = a +\n    b\n");
        check("match x {\n1=>a\n_=>{ b }\n}\n", "match x {\n    1 => a\n    _ => { b }\n}\n");
        check("let o:?int = none\nlet r = !ok && *p\n", "let o: ?int = none\nlet r = !ok && *p\n");
        check("ch <- v\nlet x = <-ch\n", "ch <- v\nlet x = <-ch\n");
        check("fn f()->int ! E {\n}\n", "fn f() -> int ! E {\n}\n");
        check("let m = Map[int, int]{1: 2}\nlet f = (x) => x * 2\n", "let m = Map[int, int]{1: 2}\nlet f = (x) => x * 2\n");
        check("{\n\n    x\n\n}\n", "{\n    x\n}\n");
        check("select {\n<-quit => x\n}\nmatch v {\n-1..=0 => y\n}\n", "select {\n    <-quit => x\n}\nmatch v {\n    -1..=0 => y\n}\n");
        check("let a = f(x\n- y)\n", "let a = f(x\n    - y)\n");
        check("use ./geom\nuse std/os.{exit, args}\nuse std/{fmt, os}\n", "use ./geom\nuse std/os.{exit, args}\nuse std/{fmt, os}\n");
        check("use std/{\nfmt as f,\nos.{args},\n}\nfn main() { let s = S{a: 1} }\n", "use std/{\n    fmt as f,\n    os.{args},\n}\nfn main() { let s = S{a: 1} }\n");
        check("let c = Chan[int]()\nlet s = \"hello\"[1..3]\n", "let c = Chan[int]()\nlet s = \"hello\"[1..3]\n");
        check("fn f(xs: []?String, m :Map[int, ?int]) -> ?int {}\nfn g(inout self, p: *u8) -> *u8 {}\n", "fn f(xs: []?String, m: Map[int, ?int]) -> ?int {}\nfn g(inout self, p: *u8) -> *u8 {}\n");
        check("swap(& &a)\nlet x = - -1\nlet y = !!ok\n", "swap(& &a)\nlet x = - -1\nlet y = !!ok\n");
    }

    #[test]
    fn whole_files() {
        assert_eq!(format("fn main(){\nprintln(1)\n}").unwrap(), "fn main() {\n    println(1)\n}\n");
        let e = format("fn main() {\n    let = 1\n}\n").unwrap_err();
        assert!(e.starts_with("line 2:"), "{}", e);
    }
}
