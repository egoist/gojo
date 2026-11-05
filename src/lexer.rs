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
    Char(u32),
    Kw(Kw),
    P(&'static str),
    Newline,
    Eof,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kw {
    Use, Extend, Pub, Func, Struct, Enum, Interface, Type, Copy, Let, Var, If, Else, For,
    In, Match, Return, Break, Continue, Defer, ErrDefer, Go, Try, Catch, With, Yield, Unsafe, Extern,
    None, True, False, Inout, Sink, Where, Test, Const, Chan, Select,
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
        "chan" => Chan,
        "select" => Select,
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

const PUNCTS: &[&str] = &[
    "<<=", ">>=", "+%=", "-%=", "*%=", "..=", "...", "&&", "||", "==", "!=", "<=", ">=", "<<", ">>", "+=",
    "-=", "*=", "/=", "%=", "&=", "|=", "^=", "=>", "->", "??", "?.", "..", "<-", "+%", "-%", "*%",
    "(", ")", "[", "]", "{", "}", ",", ";", ":", ".", "?", "!", "=", "<", ">", "+", "-", "*", "/",
    "%", "&", "|", "^", "~", "@", "#",
];

pub fn lex(file: u32, src: &str) -> Result<Vec<Token>, Diag> {
    let b = src.as_bytes();
    let mut i = 0usize;
    let mut out: Vec<Token> = Vec::new();
    let mut stack: Vec<u8> = Vec::new();
    let mut nl_before = false;
    let sp = |lo: usize, hi: usize| Span::new(file, lo as u32, hi as u32);

    fn ends_stmt(t: &Tok) -> bool {
        match t {
            Tok::Ident(_) | Tok::Int(_) | Tok::Float(_) | Tok::Str(_) | Tok::Char(_) => true,
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
        } else if c.is_ascii_digit() {
            lex_number(src, &mut i, file)?
        } else if c == b'"' {
            i += 1;
            let mut s = String::new();
            loop {
                if i >= b.len() || b[i] == b'\n' {
                    return Err(Diag::new(sp(start, i), "unterminated string literal"));
                }
                if b[i] == b'"' {
                    i += 1;
                    break;
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
            Tok::Str(s)
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
            for p in PUNCTS {
                if src[i..].starts_with(p) {
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
    if let Some(last) = out.last() {
        if ends_stmt(&last.tok) {
            out.push(Token { tok: Tok::Newline, span: sp(b.len(), b.len()), nl_before: false });
        }
    }
    out.push(Token { tok: Tok::Eof, span: sp(b.len(), b.len()), nl_before: true });
    Ok(out)
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
