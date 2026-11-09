// Source files, spans and error reporting.

use std::fmt::Write as _;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Span {
    pub file: u32,
    pub lo: u32,
    pub hi: u32,
}

impl Span {
    pub fn new(file: u32, lo: u32, hi: u32) -> Span {
        Span { file, lo, hi }
    }
    pub fn to(self, other: Span) -> Span {
        if self.file != other.file {
            return self;
        }
        Span { file: self.file, lo: self.lo.min(other.lo), hi: self.hi.max(other.hi) }
    }
}

pub struct SourceFile {
    pub path: String,
    pub text: String,
    line_starts: Vec<u32>,
}

impl SourceFile {
    pub fn new(path: String, text: String) -> SourceFile {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        SourceFile { path, text, line_starts }
    }

    /// 1-based (line, column).
    pub fn line_col(&self, pos: u32) -> (usize, usize) {
        let line = match self.line_starts.binary_search(&pos) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let start = self.line_starts[line] as usize;
        // (a position inside a multi-byte character counts as that character)
        let mut end = (pos as usize).min(self.text.len());
        while !self.text.is_char_boundary(end) {
            end -= 1;
        }
        let col = self.text[start..end.max(start)].chars().count() + 1;
        (line + 1, col)
    }

    pub fn line_text(&self, line: usize) -> &str {
        let start = self.line_starts[line - 1] as usize;
        let end = if line < self.line_starts.len() {
            self.line_starts[line] as usize - 1
        } else {
            self.text.len()
        };
        self.text[start..end].trim_end_matches('\r')
    }
}

#[derive(Default)]
pub struct SourceMap {
    pub files: Vec<SourceFile>,
}

impl SourceMap {
    pub fn add(&mut self, path: String, text: String) -> u32 {
        self.files.push(SourceFile::new(path, text));
        (self.files.len() - 1) as u32
    }
    pub fn file(&self, id: u32) -> &SourceFile {
        &self.files[id as usize]
    }
}

#[derive(Clone, Debug)]
pub struct Diag {
    pub span: Span,
    pub msg: String,
    pub notes: Vec<(Option<Span>, String)>,
    /// A warning: the program is right, and still builds and runs; the code
    /// could be better. Everything else is an error.
    pub warning: bool,
    /// Edits that fix it, when there is one obvious fix: replace each span
    /// with its text (an empty span inserts). `gojo check --json` gives
    /// them to tools; a coding agent can apply them as they are.
    pub fixes: Vec<(Span, String)>,
}

impl Diag {
    pub fn new(span: Span, msg: impl Into<String>) -> Diag {
        Diag { span, msg: msg.into(), notes: vec![], warning: false, fixes: vec![] }
    }
    pub fn warning(span: Span, msg: impl Into<String>) -> Diag {
        Diag { span, msg: msg.into(), notes: vec![], warning: true, fixes: vec![] }
    }
    pub fn note(mut self, span: Option<Span>, msg: impl Into<String>) -> Diag {
        self.notes.push((span, msg.into()));
        self
    }
    /// A machine-applicable fix: replace `span` with `text`.
    pub fn fix(mut self, span: Span, text: impl Into<String>) -> Diag {
        self.fixes.push((span, text.into()));
        self
    }
}

pub fn json_str(s: &str) -> String {
    let mut o = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            '\n' => o.push_str("\\n"),
            '\t' => o.push_str("\\t"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o.push('"');
    o
}

/// A place as JSON fields: the file, line and column of both ends (1-based,
/// the end exclusive), and the byte offsets.
fn json_span(sm: &SourceMap, sp: Span) -> String {
    let f = sm.file(sp.file);
    let (l, c) = f.line_col(sp.lo);
    let (el, ec) = f.line_col(sp.hi.max(sp.lo));
    format!("\"file\": {}, \"line\": {}, \"column\": {}, \"endLine\": {}, \"endColumn\": {}, \"start\": {}, \"end\": {}", json_str(&f.path), l, c, el, ec, sp.lo, sp.hi.max(sp.lo))
}

/// One diagnostic as a line of JSON (`gojo check --json`).
pub fn render_json(sm: &SourceMap, d: &Diag) -> String {
    let mut notes = vec![];
    for (sp, n) in &d.notes {
        match sp {
            Some(sp) => notes.push(format!("{{\"message\": {}, {}}}", json_str(&tuple_names(n)), json_span(sm, *sp))),
            None => notes.push(format!("{{\"message\": {}}}", json_str(&tuple_names(n)))),
        }
    }
    let fixes: Vec<String> = d.fixes.iter().map(|(sp, t)| format!("{{{}, \"replacement\": {}}}", json_span(sm, *sp), json_str(t))).collect();
    format!("{{\"severity\": {}, \"message\": {}, {}, \"notes\": [{}], \"fixes\": [{}]}}", json_str(if d.warning { "warning" } else { "error" }), json_str(&tuple_names(&d.msg)), json_span(sm, d.span), notes.join(", "), fixes.join(", "))
}

/// Tuple elements are fields `_0`, `_1`, ... inside; messages show `t.0` and
/// "element 0".
fn tuple_names(m: &str) -> String {
    let mut s = m.to_string();
    for i in 0..6 {
        s = s.replace(&format!("field `_{}`", i), &format!("element `{}`", i)).replace(&format!("._{}", i), &format!(".{}", i));
    }
    s
}

pub fn render(sm: &SourceMap, d: &Diag, color: bool) -> String {
    let (red, blue, bold, reset) = if color {
        (if d.warning { "\x1b[33;1m" } else { "\x1b[31;1m" }, "\x1b[34;1m", "\x1b[1m", "\x1b[0m")
    } else {
        ("", "", "", "")
    };
    let mut out = String::new();
    let f = sm.file(d.span.file);
    let (line, col) = f.line_col(d.span.lo);
    let _ = writeln!(out, "{red}{}{reset}{bold}: {}{reset}", if d.warning { "warning" } else { "error" }, tuple_names(&d.msg));
    let _ = writeln!(out, "  {blue}-->{reset} {}:{}:{}", f.path, line, col);
    snippet(&mut out, sm, d.span, blue, red, reset);
    for (sp, n) in &d.notes {
        let n = tuple_names(n);
        match sp {
            Some(sp) => {
                let nf = sm.file(sp.file);
                let (l, c) = nf.line_col(sp.lo);
                let _ = writeln!(out, "  {blue}note{reset}: {} ({}:{}:{})", n, nf.path, l, c);
                snippet(&mut out, sm, *sp, blue, blue, reset);
            }
            None => {
                let _ = writeln!(out, "  {blue}= note{reset}: {}", n);
            }
        }
    }
    out
}

fn snippet(out: &mut String, sm: &SourceMap, span: Span, blue: &str, mark: &str, reset: &str) {
    let f = sm.file(span.file);
    let (line, col) = f.line_col(span.lo);
    let (eline, ecol) = f.line_col(span.hi.max(span.lo));
    let text = f.line_text(line);
    let width = line.to_string().len();
    let _ = writeln!(out, "  {blue}{:>width$} |{reset} {}", line, text, width = width);
    let len = if eline == line { (ecol.saturating_sub(col)).max(1) } else { (text.chars().count() + 1).saturating_sub(col).max(1) };
    let pad: String = text.chars().take(col - 1).map(|c| if c == '\t' { '\t' } else { ' ' }).collect();
    let _ = writeln!(out, "  {blue}{:>width$} |{reset} {}{mark}{}{reset}", "", pad, "^".repeat(len), width = width);
}
