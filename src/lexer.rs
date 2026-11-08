// Lexer with Go-style automatic statement terminators.
//
// A newline becomes a `Newline` token when the previous token can end a
// statement and we are not inside `(` or `[` (inside `{` newlines count).

use crate::diag::{Diag, Span};

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(String),
    Int(u128),
    Float(f64),
    Str(String),
    /// A string with `${expr}` interpolations: literal text and the tokens of
    /// each embedded expression.
    Interp(Vec<IPart>),
    Char(u32),
    Kw(Kw),
    P(&'static str),
    Newline,
    Eof,
}

#[derive(Clone, Debug)]
pub enum IPart {
    Lit(String),
    /// `${expr}` or `${expr:spec}`; `toks` ends with `Eof`.
    Expr { toks: Vec<Token>, spec: Option<String>, span: Span },
}

impl PartialEq for IPart {
    fn eq(&self, o: &IPart) -> bool {
        match (self, o) {
            (IPart::Lit(a), IPart::Lit(b)) => a == b,
            (IPart::Expr { toks: a, spec: s, .. }, IPart::Expr { toks: b, spec: t, .. }) => a == b && s == t,
            _ => false,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kw {
    Use, Extend, Pub, Func, Struct, Enum, Interface, Type, Copy, Let, Var, If, Else, For,
    In, Match, Return, Break, Continue, Defer, ErrDefer, Go, Try, Catch, With, Yield, Unsafe, Extern,
    None, True, False, Inout, Sink, Where, Test, Const, Select, Scope, Isolate, While, Loop,
}

const KEYWORDS: &[&str] = &[
    "use", "extend", "pub", "fn", "struct", "enum", "interface", "type", "copy", "let", "var", "if", "else", "for", "in", "match", "return", "break",
    "continue", "defer", "errdefer", "go", "try", "catch", "with", "yield", "unsafe", "extern", "none", "true", "false", "inout", "sink", "where", "test",
    "const", "select", "scope", "isolate", "while", "loop",
];

impl Kw {
    /// The keyword as written in source.
    pub fn text(self) -> &'static str {
        KEYWORDS.iter().copied().find(|w| keyword(w) == Some(self)).unwrap_or("?")
    }
}

fn keyword(s: &str) -> Option<Kw> {
    use Kw::*;
    Some(match s {
        "use" => Use,
        "extend" => Extend,
        "pub" => Pub,
        "fn" => Func,
        "struct" => Struct,
        "enum" => Enum,
        "interface" => Interface,
        "type" => Type,
        "copy" => Copy,
        "let" => Let,
        "var" => Var,
        "if" => If,
        "else" => Else,
        "for" => For,
        "in" => In,
        "match" => Match,
        "return" => Return,
        "break" => Break,
        "continue" => Continue,
        "defer" => Defer,
        "errdefer" => ErrDefer,
        "go" => Go,
        "try" => Try,
        "catch" => Catch,
        "with" => With,
        "yield" => Yield,
        "unsafe" => Unsafe,
        "extern" => Extern,
        "none" => None,
        "true" => True,
        "false" => False,
        "inout" => Inout,
        "sink" => Sink,
        "where" => Where,
        "test" => Test,
        "const" => Const,
        "select" => Select,
        "scope" => Scope,
        "isolate" => Isolate,
        "while" => While,
        "loop" => Loop,
        _ => return Option::None,
    })
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub span: Span,
    /// True when a newline (or comment containing one) precedes this token.
    pub nl_before: bool,
}

/// Tokens compare by kind and text only (interpolated strings hold tokens,
/// and the formatter compares token streams of differently laid out files).
impl PartialEq for Token {
    fn eq(&self, o: &Token) -> bool {
        self.tok == o.tok
    }
}

const PUNCTS: &[&str] = &[
    "<<=", ">>=", "+%=", "-%=", "*%=", "..=", "...", "&&", "||", "==", "!=", "<=", ">=", "<<", ">>", "+=",
    "-=", "*=", "/=", "%=", "&=", "|=", "^=", "=>", "->", "??", "?.", "..", "<-", "+%", "-%", "*%",
    "(", ")", "[", "]", "{", "}", ",", ";", ":", ".", "?", "!", "=", "<", ">", "+", "-", "*", "/",
    "%", "&", "|", "^", "~", "@", "#",
];

/// `PUNCTS` by first byte, longest first (as `PUNCTS` lists them).
fn puncts() -> &'static [Vec<&'static str>] {
    static T: std::sync::OnceLock<Vec<Vec<&'static str>>> = std::sync::OnceLock::new();
    T.get_or_init(|| {
        let mut t = vec![vec![]; 256];
        for p in PUNCTS {
            t[p.as_bytes()[0] as usize].push(*p);
        }
        t
    })
}

pub fn lex(file: u32, src: &str) -> Result<Vec<Token>, Diag> {
    lex_from(file, src, 0, false).map(|(t, _)| t)
}

/// Lexes from byte `i`. In an interpolation (`interp`), stops at the `}` or
/// format `:` that ends it, at bracket level 0, and returns its position.
fn lex_from(file: u32, src: &str, mut i: usize, interp: bool) -> Result<(Vec<Token>, usize), Diag> {
    let b = src.as_bytes();
    let open = i;
    let mut out: Vec<Token> = Vec::new();
    let mut stack: Vec<u8> = Vec::new();
    let mut nl_before = false;
    let sp = |lo: usize, hi: usize| Span::new(file, lo as u32, hi as u32);

    fn ends_stmt(t: &Tok) -> bool {
        match t {
            Tok::Ident(_) | Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::Interp(_) | Tok::Char(_) => true,
            Tok::Kw(k) => matches!(k, Kw::Return | Kw::Break | Kw::Continue | Kw::True | Kw::False | Kw::None),
            Tok::P(p) => matches!(*p, ")" | "]" | "}"),
            _ => false,
        }
    }

    macro_rules! newline {
        ($pos:expr) => {{
            let in_braces = matches!(stack.last(), None | Some(b'{'));
            if in_braces {
                if let Some(last) = out.last() {
                    if ends_stmt(&last.tok) {
                        out.push(Token { tok: Tok::Newline, span: sp($pos, $pos + 1), nl_before: false });
                    }
                }
            }
            nl_before = true;
        }};
    }

    while i < b.len() {
        let c = b[i];
        if interp && stack.is_empty() && (c == b'}' || c == b':') {
            if out.is_empty() {
                return Err(Diag::new(sp(open, i + 1), "empty interpolation: write an expression inside `${...}`"));
            }
            out.push(Token { tok: Tok::Eof, span: sp(i, i), nl_before: false });
            return Ok((out, i));
        }
        if interp && c == b'\n' {
            return Err(Diag::new(sp(open.saturating_sub(2), i), "unterminated interpolation: `${` needs a `}` on the same line"));
        }
        if c == b'\n' {
            newline!(i);
            i += 1;
            continue;
        }
        if c == b' ' || c == b'\t' || c == b'\r' {
            i += 1;
            continue;
        }
        if c == b'/' && i + 1 < b.len() && b[i + 1] == b'/' {
            while i < b.len() && b[i] != b'\n' {
                i += 1;
            }
            continue;
        }
        if c == b'/' && i + 1 < b.len() && b[i + 1] == b'*' {
            let start = i;
            i += 2;
            let mut had_nl = false;
            while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                if b[i] == b'\n' {
                    had_nl = true;
                }
                i += 1;
            }
            if i + 1 >= b.len() {
                return Err(Diag::new(sp(start, start + 2), "unterminated block comment"));
            }
            i += 2;
            if had_nl {
                newline!(start);
            }
            continue;
        }
        let start = i;
        let tok = if c.is_ascii_alphabetic() || c == b'_' || c >= 0x80 {
            while i < b.len() && (b[i].is_ascii_alphanumeric() || b[i] == b'_' || b[i] >= 0x80) {
                i += 1;
            }
            let s = &src[start..i];
            match keyword(s) {
                Some(k) => Tok::Kw(k),
                None => Tok::Ident(s.to_string()),
            }
        } else if c.is_ascii_digit() && matches!(out.last().map(|t| &t.tok), Some(Tok::P("."))) {
            // `t.0.1`: tuple elements, not the float `0.1`
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            Tok::Int(src[start..i].parse::<u128>().map_err(|_| Diag::new(sp(start, i), "tuple index too large"))?)
        } else if c.is_ascii_digit() {
            lex_number(src, &mut i, file)?
        } else if c == b'"' {
            i += 1;
            let mut s = String::new();
            let mut parts: Vec<IPart> = vec![];
            loop {
                if i >= b.len() || b[i] == b'\n' {
                    return Err(Diag::new(sp(start, i), "unterminated string literal"));
                }
                if b[i] == b'"' {
                    i += 1;
                    break;
                }
                if b[i] == b'$' && i + 1 < b.len() && b[i + 1] == b'{' {
                    let lo = i;
                    let (toks, end) = lex_from(file, src, i + 2, true)?;
                    i = end;
                    let mut spec = None;
                    if b[i] == b':' {
                        let s0 = i + 1;
                        while i < b.len() && b[i] != b'}' && b[i] != b'"' && b[i] != b'\n' {
                            i += 1;
                        }
                        if i >= b.len() || b[i] != b'}' {
                            return Err(Diag::new(sp(lo, i), "unterminated interpolation: expected `}` after the format"));
                        }
                        spec = Some(src[s0..i].to_string());
                    }
                    i += 1;
                    if !s.is_empty() {
                        parts.push(IPart::Lit(std::mem::take(&mut s)));
                    }
                    parts.push(IPart::Expr { toks, spec, span: sp(lo, i) });
                    continue;
                }
                if b[i] == b'\\' {
                    let ch = lex_escape(src, &mut i, file)?;
                    s.push(ch);
                } else {
                    let ch = src[i..].chars().next().unwrap();
                    s.push(ch);
                    i += ch.len_utf8();
                }
            }
            if parts.is_empty() {
                Tok::Str(s)
            } else {
                if !s.is_empty() {
                    parts.push(IPart::Lit(s));
                }
                Tok::Interp(parts)
            }
        } else if c == b'`' {
            i += 1;
            let s0 = i;
            while i < b.len() && b[i] != b'`' {
                i += 1;
            }
            if i >= b.len() {
                return Err(Diag::new(sp(start, i), "unterminated raw string literal"));
            }
            let s = src[s0..i].to_string();
            i += 1;
            Tok::Str(s)
        } else if c == b'\'' {
            i += 1;
            let ch = if i < b.len() && b[i] == b'\\' {
                lex_escape(src, &mut i, file)?
            } else if i < b.len() {
                let ch = src[i..].chars().next().unwrap();
                i += ch.len_utf8();
                ch
            } else {
                return Err(Diag::new(sp(start, i), "unterminated rune literal"));
            };
            if i >= b.len() || b[i] != b'\'' {
                return Err(Diag::new(sp(start, i), "rune literal must contain exactly one character"));
            }
            i += 1;
            Tok::Char(ch as u32)
        } else {
            let mut found = None;
            for p in &puncts()[c as usize] {
                if b[i..].starts_with(p.as_bytes()) {
                    found = Some(*p);
                    break;
                }
            }
            match found {
                Some(p) => {
                    i += p.len();
                    match p {
                        "(" => stack.push(b'('),
                        "[" => stack.push(b'['),
                        "{" => stack.push(b'{'),
                        ")" | "]" | "}" => {
                            stack.pop();
                        }
                        _ => {}
                    }
                    // A closing brace also ends the last statement inside the block.
                    if p == "}" {
                        if let Some(last) = out.last() {
                            if ends_stmt(&last.tok) && matches!(stack.last(), None | Some(b'{')) {
                                // handled naturally by parser: `}` terminates a statement
                            }
                        }
                    }
                    Tok::P(p)
                }
                None => {
                    let ch = src[i..].chars().next().unwrap();
                    return Err(Diag::new(sp(i, i + ch.len_utf8()), format!("unexpected character `{}`", ch)));
                }
            }
        };
        out.push(Token { tok, span: sp(start, i), nl_before });
        nl_before = false;
    }
    if interp {
        return Err(Diag::new(sp(open.saturating_sub(2), b.len()), "unterminated interpolation: `${` needs a `}`"));
    }
    if let Some(last) = out.last() {
        if ends_stmt(&last.tok) {
            out.push(Token { tok: Tok::Newline, span: sp(b.len(), b.len()), nl_before: false });
        }
    }
    out.push(Token { tok: Tok::Eof, span: sp(b.len(), b.len()), nl_before: true });
    Ok((out, b.len()))
}

fn lex_escape(src: &str, i: &mut usize, file: u32) -> Result<char, Diag> {
    let b = src.as_bytes();
    let start = *i;
    *i += 1; // backslash
    if *i >= b.len() {
        return Err(Diag::new(Span::new(file, start as u32, *i as u32), "bad escape"));
    }
    let c = b[*i];
    *i += 1;
    Ok(match c {
        b'n' => '\n',
        b't' => '\t',
        b'r' => '\r',
        b'0' => '\0',
        b'\\' => '\\',
        b'"' => '"',
        b'\'' => '\'',
        b'$' => '$',
        b'x' => {
            let h = src.get(*i..*i + 2).ok_or_else(|| Diag::new(Span::new(file, start as u32, *i as u32), "bad \\x escape"))?;
            let v = u8::from_str_radix(h, 16).map_err(|_| Diag::new(Span::new(file, start as u32, *i as u32 + 2), "bad \\x escape"))?;
            *i += 2;
            v as char
        }
        b'u' => {
            if *i < b.len() && b[*i] == b'{' {
                let close = src[*i..].find('}').ok_or_else(|| Diag::new(Span::new(file, start as u32, *i as u32), "bad \\u escape"))?;
                let h = &src[*i + 1..*i + close];
                let v = u32::from_str_radix(h, 16).ok().and_then(char::from_u32);
                *i += close + 1;
                v.ok_or_else(|| Diag::new(Span::new(file, start as u32, *i as u32), "bad \\u escape"))?
            } else {
                return Err(Diag::new(Span::new(file, start as u32, *i as u32), "expected `\\u{...}`"));
            }
        }
        _ => return Err(Diag::new(Span::new(file, start as u32, *i as u32), format!("unknown escape `\\{}`", c as char))),
    })
}

fn lex_number(src: &str, i: &mut usize, file: u32) -> Result<Tok, Diag> {
    let b = src.as_bytes();
    let start = *i;
    let err = |lo: usize, hi: usize, m: &str| Diag::new(Span::new(file, lo as u32, hi as u32), m.to_string());
    if b[*i] == b'0' && *i + 1 < b.len() && matches!(b[*i + 1], b'x' | b'b' | b'o') {
        let radix = match b[*i + 1] {
            b'x' => 16,
            b'b' => 2,
            _ => 8,
        };
        *i += 2;
        let s0 = *i;
        while *i < b.len() && (b[*i].is_ascii_alphanumeric() || b[*i] == b'_') {
            *i += 1;
        }
        let digits: String = src[s0..*i].chars().filter(|c| *c != '_').collect();
        return u128::from_str_radix(&digits, radix).map(Tok::Int).map_err(|_| err(start, *i, "invalid integer literal"));
    }
    while *i < b.len() && (b[*i].is_ascii_digit() || b[*i] == b'_') {
        *i += 1;
    }
    let mut is_float = false;
    // `1..2` is a range, not a float.
    if *i + 1 < b.len() && b[*i] == b'.' && b[*i + 1].is_ascii_digit() {
        is_float = true;
        *i += 1;
        while *i < b.len() && (b[*i].is_ascii_digit() || b[*i] == b'_') {
            *i += 1;
        }
    }
    if *i < b.len() && (b[*i] == b'e' || b[*i] == b'E') {
        let save = *i;
        *i += 1;
        if *i < b.len() && (b[*i] == b'+' || b[*i] == b'-') {
            *i += 1;
        }
        if *i < b.len() && b[*i].is_ascii_digit() {
            is_float = true;
            while *i < b.len() && b[*i].is_ascii_digit() {
                *i += 1;
            }
        } else {
            *i = save;
        }
    }
    let text: String = src[start..*i].chars().filter(|c| *c != '_').collect();
    if is_float {
        text.parse::<f64>().map(Tok::Float).map_err(|_| err(start, *i, "invalid float literal"))
    } else {
        text.parse::<u128>().map(Tok::Int).map_err(|_| err(start, *i, "integer literal too large"))
    }
}
