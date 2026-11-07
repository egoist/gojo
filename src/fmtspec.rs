// Templates of `fmt.format` and `fmt.printf`: text with `{}` placeholders.
//
//   {}        the value, as `print` shows it
//   {:8}      at least 8 characters wide (numbers right-aligned, the rest left)
//   {:<8} {:>8} {:^8}   left, right, centered;   {:*^8}   with `*` as the fill
//   {:08}     numbers padded with zeros after the sign
//   {:.2}     a float with 2 digits after the point
//   {:x} {:X} {:o} {:b}   an integer in hex, octal or binary
//   {{ }}     literal braces
//
// The template must be a string literal; the compiler checks it against the
// arguments.

#[derive(Clone, Debug, PartialEq)]
pub struct Spec {
    pub fill: char,
    pub align: Option<char>,
    pub zero: bool,
    pub width: usize,
    pub precision: Option<usize>,
    pub radix: Option<char>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Piece {
    Lit(String),
    Arg(Spec),
}

/// Widths and precisions above this are surely mistakes.
const MAX_WIDTH: usize = 1000;

pub fn parse(t: &str) -> Result<Vec<Piece>, String> {
    let mut out = vec![];
    let mut lit = String::new();
    let cs: Vec<char> = t.chars().collect();
    let mut i = 0;
    while i < cs.len() {
        let c = cs[i];
        if c == '{' && cs.get(i + 1) == Some(&'{') {
            lit.push('{');
            i += 2;
            continue;
        }
        if c == '}' && cs.get(i + 1) == Some(&'}') {
            lit.push('}');
            i += 2;
            continue;
        }
        if c == '}' {
            return Err("unmatched `}` in the template; write `}}` for a literal brace".into());
        }
        if c != '{' {
            lit.push(c);
            i += 1;
            continue;
        }
        let close = match cs[i..].iter().position(|&x| x == '}') {
            Some(p) => i + p,
            None => return Err("unclosed `{` in the template; write `{{` for a literal brace".into()),
        };
        let inner: String = cs[i + 1..close].iter().collect();
        if !lit.is_empty() {
            out.push(Piece::Lit(std::mem::take(&mut lit)));
        }
        out.push(Piece::Arg(spec(&inner)?));
        i = close + 1;
    }
    if !lit.is_empty() {
        out.push(Piece::Lit(lit));
    }
    Ok(out)
}

fn spec(inner: &str) -> Result<Spec, String> {
    let mut s = Spec { fill: ' ', align: None, zero: false, width: 0, precision: None, radix: None };
    if inner.is_empty() {
        return Ok(s);
    }
    let Some(rest) = inner.strip_prefix(':') else {
        return Err(format!("`{{{}}}`: placeholders are `{{}}` or `{{:spec}}`, without names or positions", inner));
    };
    let cs: Vec<char> = rest.chars().collect();
    let mut i = 0;
    let is_align = |c: char| matches!(c, '<' | '>' | '^');
    if cs.len() >= 2 && is_align(cs[1]) {
        s.fill = cs[0];
        s.align = Some(cs[1]);
        i = 2;
    } else if !cs.is_empty() && is_align(cs[0]) {
        s.align = Some(cs[0]);
        i = 1;
    }
    if cs.get(i) == Some(&'0') {
        s.zero = true;
        i += 1;
    }
    let num = |i: &mut usize| -> Option<usize> {
        let start = *i;
        while *i < cs.len() && cs[*i].is_ascii_digit() {
            *i += 1;
        }
        if *i == start {
            None
        } else {
            cs[start..*i].iter().collect::<String>().parse().ok()
        }
    };
    if let Some(w) = num(&mut i) {
        s.width = w;
    }
    if cs.get(i) == Some(&'.') {
        i += 1;
        s.precision = Some(num(&mut i).ok_or("expected digits after `.` in a placeholder")?);
    }
    if let Some(&c) = cs.get(i) {
        if matches!(c, 'x' | 'X' | 'o' | 'b') {
            s.radix = Some(c);
            i += 1;
        }
    }
    if i != cs.len() {
        return Err(format!("`{{:{}}}` is not a valid placeholder (see `fmt.format`: like `{{:>8}}`, `{{:08.3}}` or `{{:x}}`)", rest));
    }
    if s.width > MAX_WIDTH || s.precision.is_some_and(|p| p > MAX_WIDTH) {
        return Err(format!("width or precision above {} in a placeholder", MAX_WIDTH));
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn templates() {
        assert_eq!(parse("a{}b").unwrap().len(), 3);
        assert_eq!(parse("{{}}").unwrap(), vec![Piece::Lit("{}".into())]);
        let p = parse("{:*^10.2x}").unwrap();
        assert_eq!(p, vec![Piece::Arg(Spec { fill: '*', align: Some('^'), zero: false, width: 10, precision: Some(2), radix: Some('x') })]);
        assert!(parse("{").is_err());
        assert!(parse("}").is_err());
        assert!(parse("{name}").is_err());
        assert!(parse("{:q}").is_err());
        assert!(parse("{:99999}").is_err());
    }
}
