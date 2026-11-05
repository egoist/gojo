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
        let end = (pos as usize).min(self.text.len());
        let col = self.text[start..end].chars().count() + 1;
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
}

impl Diag {
    pub fn new(span: Span, msg: impl Into<String>) -> Diag {
        Diag { span, msg: msg.into(), notes: vec![] }
    }
    pub fn note(mut self, span: Option<Span>, msg: impl Into<String>) -> Diag {
        self.notes.push((span, msg.into()));
        self
    }
}

pub fn render(sm: &SourceMap, d: &Diag, color: bool) -> String {
    let (red, blue, bold, reset) = if color {
        ("\x1b[31;1m", "\x1b[34;1m", "\x1b[1m", "\x1b[0m")
    } else {
        ("", "", "", "")
    };
    let mut out = String::new();
    let f = sm.file(d.span.file);
    let (line, col) = f.line_col(d.span.lo);
    let _ = writeln!(out, "{red}error{reset}{bold}: {}{reset}", d.msg);
    let _ = writeln!(out, "  {blue}-->{reset} {}:{}:{}", f.path, line, col);
    snippet(&mut out, sm, d.span, blue, red, reset);
    for (sp, n) in &d.notes {
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
