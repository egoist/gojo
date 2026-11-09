// Recursive-descent parser.

use crate::ast::*;
use crate::diag::{Diag, Span};
use crate::lexer::{IPart, Kw, Tok, Token};

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
    file: u32,
    /// When false, `Name {` is not parsed as a struct literal (if/for/match headers).
    allow_struct: bool,
    /// Current recursion depth, and the deepest syntax tree level reached (see `nested`).
    nest: u32,
    peak: u32,
    /// Bracket depth of each token, computed on first use (only match guards,
    /// `select` arms and `x =>` closures need it).
    depth: Vec<u32>,
    /// While parsing a match guard or a `select` arm head, `x => ...` at this
    /// bracket depth is the arm's arrow, not a closure.
    arrow_depth: Option<u32>,
    /// Hidden temporaries made for `let (a, b) = e`.
    tuple_tmps: u32,
}

/// How deeply expressions, blocks, types and patterns may nest. Every later
/// phase walks the tree recursively, so this bounds the compiler's stack use;
/// it counts parser levels, a few per source level of nesting.
pub const MAX_NEST: u32 = 1000;

type PResult<T> = Result<T, Diag>;

pub fn parse_file(file: u32, toks: Vec<Token>) -> PResult<File> {
    let mut p = Parser::new(file, toks);
    p.file()
}

fn bracket_depths(toks: &[Token]) -> Vec<u32> {
    let mut d = 0u32;
    toks.iter()
        .map(|t| match t.tok {
            Tok::P("(" | "[" | "{") => {
                d += 1;
                d - 1
            }
            Tok::P(")" | "]" | "}") => {
                d = d.saturating_sub(1);
                d
            }
            _ => d,
        })
        .collect()
}

impl Parser {
    pub fn new(file: u32, toks: Vec<Token>) -> Parser {
        Parser { toks, pos: 0, file, allow_struct: true, nest: 0, peak: 0, depth: vec![], arrow_depth: None, tuple_tmps: 0 }
    }

    /// Runs `f` with `x => ...` at the current bracket depth read as an arm's
    /// arrow (match guards, `select` arm heads).
    fn arm_head<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        let d = self.depth_at(self.pos);
        let save = self.arrow_depth.replace(d);
        let r = f(self);
        self.arrow_depth = save;
        r
    }

    /// At a line break before `.name`: does the next line continue the
    /// expression (`xs\n    .map(...)`)? Not when that line is a match arm
    /// (`.Circle(r) => ...`): an `=>` on its bracket level.
    fn dot_continues(&self) -> bool {
        if !matches!(self.peek(), Tok::Newline) || !matches!(self.peek_at(1), Tok::P(".")) || !matches!(self.peek_at(2), Tok::Ident(_)) {
            return false;
        }
        let mut depth = 0i32;
        for t in &self.toks[self.pos + 1..] {
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

    fn depth_at(&mut self, i: usize) -> u32 {
        if self.depth.is_empty() {
            self.depth = bracket_depths(&self.toks);
        }
        self.depth[i]
    }

    /// The `:` before a type annotation (`x: int`).
    fn colon(&mut self, kind: &str, name: &str) -> PResult<()> {
        if self.eat_p(":") {
            return Ok(());
        }
        let what = if name.is_empty() { kind.to_string() } else { format!("{}`{}`", kind, name) };
        let d = Diag::new(self.span(), format!("expected `:` before the type of {}, found {}", what, self.describe())).note(None, "annotations are written `name: Type`, as in `fn f(x: int) -> int` and `let n: u8 = 1`");
        Err(d)
    }

    /// Does a type start here (old syntax, without `:` or `->`)?
    fn type_starts(&self) -> bool {
        matches!(self.peek(), Tok::Ident(_) | Tok::P("?") | Tok::P("[") | Tok::P("*") | Tok::P("(") | Tok::Kw(Kw::Func))
    }

    // ---------- nesting ----------
    /// Runs `f` one level deeper. Recursion is bounded by `MAX_NEST`; trees that
    /// loops build (`a + b + c`, `x.f().g()`) are bounded through `grown`.
    fn nested<T>(&mut self, f: impl FnOnce(&mut Self) -> PResult<T>) -> PResult<T> {
        if self.nest >= MAX_NEST {
            return self.too_deep();
        }
        self.nest += 1;
        self.peak = self.peak.max(self.nest);
        let r = f(self);
        self.nest -= 1;
        r
    }

    /// Starts measuring the height of a tree built at the current level.
    fn measure(&mut self) -> u32 {
        std::mem::replace(&mut self.peak, self.nest)
    }

    /// The node built at this level now wraps everything parsed since `measure`
    /// once more: one more level for the later phases to recurse through.
    fn grown(&mut self) -> PResult<()> {
        self.peak += 1;
        if self.peak > MAX_NEST {
            return self.too_deep();
        }
        Ok(())
    }

    fn measured(&mut self, saved: u32) {
        self.peak = self.peak.max(saved);
    }

    fn too_deep<T>(&self) -> PResult<T> {
        self.err("this is nested too deeply for the compiler; split it up with `let` bindings or helper functions")
    }

    // ---------- token helpers ----------
    fn peek(&self) -> &Tok {
        &self.toks[self.pos].tok
    }
    fn peek_at(&self, n: usize) -> &Tok {
        let i = (self.pos + n).min(self.toks.len() - 1);
        &self.toks[i].tok
    }
    fn span(&self) -> Span {
        self.toks[self.pos].span
    }
    fn prev_span(&self) -> Span {
        if self.pos == 0 {
            self.span()
        } else {
            self.toks[self.pos - 1].span
        }
    }
    fn bump(&mut self) -> Token {
        let t = self.toks[self.pos].clone();
        if self.pos < self.toks.len() - 1 {
            self.pos += 1;
        }
        t
    }
    fn is_p(&self, p: &str) -> bool {
        matches!(self.peek(), Tok::P(q) if *q == p)
    }
    fn is_kw(&self, k: Kw) -> bool {
        matches!(self.peek(), Tok::Kw(q) if *q == k)
    }
    fn eat_p(&mut self, p: &str) -> bool {
        if self.is_p(p) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn eat_kw(&mut self, k: Kw) -> bool {
        if self.is_kw(k) {
            self.bump();
            true
        } else {
            false
        }
    }
    fn err<T>(&self, msg: impl Into<String>) -> PResult<T> {
        Err(Diag::new(self.span(), msg))
    }
    fn describe(&self) -> String {
        match self.peek() {
            Tok::Ident(s) => format!("identifier `{}`", s),
            Tok::Int(_) | Tok::Float(_) => "number".into(),
            Tok::Str(_) | Tok::Interp(_) => "string".into(),
            Tok::Char(_) => "rune literal".into(),
            Tok::Kw(k) => format!("keyword `{}`", k.text()),
            Tok::P(p) => format!("`{}`", p),
            Tok::Newline => "newline".into(),
            Tok::Eof => "end of file".into(),
        }
    }
    fn expect_p(&mut self, p: &str) -> PResult<Span> {
        if self.is_p(p) {
            Ok(self.bump().span)
        } else {
            Err(self.with_opener(Diag::new(self.span(), format!("expected `{}`, found {}", p, self.describe())), p))
        }
    }

    /// For a missing `)`, `]` or `}`: points at the bracket it should close.
    fn with_opener(&self, d: Diag, close: &str) -> Diag {
        let open = match close {
            ")" => "(",
            "]" => "[",
            "}" => "{",
            _ => return d,
        };
        let mut depth = 0;
        for t in self.toks[..self.pos.min(self.toks.len())].iter().rev() {
            match &t.tok {
                Tok::P(x) if *x == close => depth += 1,
                Tok::P(x) if *x == open => {
                    if depth == 0 {
                        return d.note(Some(t.span), format!("this `{}` is not closed", open));
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        d
    }
    fn expect_kw(&mut self, k: Kw) -> PResult<Span> {
        if self.is_kw(k) {
            Ok(self.bump().span)
        } else {
            self.err(format!("expected `{}`, found {}", format!("{:?}", k).to_lowercase(), self.describe()))
        }
    }
    fn ident(&mut self) -> PResult<(String, Span)> {
        match self.peek().clone() {
            Tok::Ident(s) => {
                let sp = self.bump().span;
                Ok((s, sp))
            }
            _ => self.err(format!("expected identifier, found {}", self.describe())),
        }
    }
    /// Identifiers, plus keywords used as member names (`g.go(...)`).
    fn member_name(&mut self) -> PResult<(String, Span)> {
        if let Tok::Int(n) = self.peek().clone() {
            // `t.0`: a tuple element
            let sp = self.bump().span;
            return Ok((format!("_{}", n), sp));
        }
        if let Tok::Kw(k) = self.peek().clone() {
            let sp = self.bump().span;
            return Ok((format!("{:?}", k).to_lowercase(), sp));
        }
        self.ident()
    }
    fn skip_nl(&mut self) {
        while matches!(self.peek(), Tok::Newline) || self.is_p(";") {
            self.bump();
        }
    }
    fn end_stmt(&mut self) -> PResult<()> {
        if matches!(self.peek(), Tok::Newline) || self.is_p(";") {
            self.skip_nl();
            Ok(())
        } else if self.is_p("}") || matches!(self.peek(), Tok::Eof) {
            Ok(())
        } else {
            self.err(format!("expected end of statement, found {}", self.describe()))
        }
    }
    fn sp_from(&self, lo: Span) -> Span {
        lo.to(self.prev_span())
    }

    // ---------- file & items ----------
    fn file(&mut self) -> PResult<File> {
        self.skip_nl();
        let mut imports = vec![];
        while self.is_kw(Kw::Use) {
            self.bump();
            loop {
                imports.extend(self.use_specs()?);
                if !self.eat_p(",") {
                    break;
                }
            }
            self.end_stmt()?;
        }
        let mut items = vec![];
        while !matches!(self.peek(), Tok::Eof) {
            if self.is_kw(Kw::Use) {
                return self.err("`use` declarations must come before everything else in the file");
            }
            items.extend(self.item()?);
            self.skip_nl();
        }
        Ok(File { package: String::new(), imports, items, file_id: self.file })
    }

    /// One `use` item, or a group of them: `std/{fmt, os as o}` is
    /// `std/fmt, std/os as o`.
    fn use_specs(&mut self) -> PResult<Vec<Import>> {
        let first = self.use_spec()?;
        if !(self.is_p("/") && matches!(self.peek_at(1), Tok::P("{"))) {
            return Ok(vec![first]);
        }
        if first.names.is_some() || first.alias.is_some() {
            return self.err("a group `/{...}` follows the path directly: `std/{fmt, os}`");
        }
        self.bump();
        self.bump();
        self.skip_nl();
        let mut out = vec![];
        while !self.is_p("}") {
            let mut item = self.use_spec()?;
            if item.path.starts_with("./") || item.path.starts_with("../") {
                return self.err("the paths in a group are inside it: `std/{fmt, os}`, not `./`");
            }
            if self.is_p("/") && matches!(self.peek_at(1), Tok::P("{")) {
                return self.err("groups can't be nested");
            }
            item.path = format!("{}/{}", first.path, item.path);
            item.span = Span { lo: first.span.lo, ..item.span };
            out.push(item);
            self.skip_nl();
            if !self.eat_p(",") {
                break;
            }
            self.skip_nl();
        }
        self.expect_p("}")?;
        if out.is_empty() {
            return self.err("an empty group imports nothing");
        }
        Ok(out)
    }

    /// `std/fmt`, `./geom`, `../shared/util`, `./geom.{Vec2, Shape}`, `./net/http as h`
    fn use_spec(&mut self) -> PResult<Import> {
        let lo = self.span();
        let mut path = String::new();
        loop {
            if self.is_p(".") && matches!(self.peek_at(1), Tok::P("/")) {
                self.bump();
                self.bump();
                path.push_str("./");
            } else if self.is_p("..") && matches!(self.peek_at(1), Tok::P("/")) {
                self.bump();
                self.bump();
                path.push_str("../");
            } else {
                break;
            }
        }
        let (first, _) = self.ident()?;
        path.push_str(&first);
        while self.is_p("/") && !matches!(self.peek_at(1), Tok::P("{")) {
            self.bump();
            let (seg, _) = self.ident()?;
            path.push('/');
            path.push_str(&seg);
        }
        let mut names = None;
        if self.is_p(".") && matches!(self.peek_at(1), Tok::P("{")) {
            self.bump();
            self.bump();
            self.skip_nl();
            let mut ns = vec![];
            while !self.is_p("}") {
                ns.push(self.ident()?);
                self.skip_nl();
                if !self.eat_p(",") {
                    break;
                }
                self.skip_nl();
            }
            self.skip_nl();
            self.expect_p("}")?;
            names = Some(ns);
        }
        let mut alias = None;
        if matches!(self.peek(), Tok::Ident(s) if s == "as") {
            self.bump();
            if names.is_some() {
                return self.err("`as` renames a package; it can't be combined with `.{...}`");
            }
            alias = Some(self.ident()?.0);
        }
        Ok(Import { path, alias, names, span: self.sp_from(lo) })
    }

    fn item(&mut self) -> PResult<Vec<Item>> {
        let lo = self.span();
        if self.is_kw(Kw::Extend) {
            // `extend Type { func ... }`, or `extend T: Integer { ... }` for
            // every built-in type in a type set
            self.bump();
            let mut ext = vec![];
            let owner = if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::P(":")) {
                let (name, span) = self.ident()?;
                self.bump();
                let bounds = self.bound_list()?;
                ext.push(TParam { name: name.clone(), bounds, is_int: false, span });
                TypeExpr { kind: TypeExprKind::Named { path: vec![name], args: vec![] }, span }
            } else {
                self.ty()?
            };
            self.expect_p("{")?;
            self.skip_nl();
            let mut out = vec![];
            while !self.is_p("}") {
                let mlo = self.span();
                let is_pub = self.eat_kw(Kw::Pub);
                let is_unsafe = self.is_kw(Kw::Unsafe) && matches!(self.peek_at(1), Tok::Kw(Kw::Func));
                if is_unsafe {
                    self.bump();
                }
                if !self.is_kw(Kw::Func) {
                    return self.err(format!("expected `fn` in `extend` block, found {}", self.describe()));
                }
                let mut f = self.func_decl(Some(&owner))?;
                f.is_unsafe = is_unsafe;
                f.ext_tparams = ext.clone();
                out.push(Item { kind: ItemKind::Func(f), is_pub, span: self.sp_from(mlo) });
                self.skip_nl();
            }
            self.bump();
            return Ok(out);
        }
        let mut methods: Vec<Item> = vec![];
        let is_pub = self.eat_kw(Kw::Pub);
        let mut is_copy = false;
        let mut unsafe_send = false;
        let mut unsafe_fn = false;
        if self.is_kw(Kw::Unsafe) && matches!(self.peek_at(1), Tok::Ident(s) if s == "send") {
            self.bump();
            self.bump();
            unsafe_send = true;
        } else if self.is_kw(Kw::Unsafe) && matches!(self.peek_at(1), Tok::Kw(Kw::Func)) {
            self.bump();
            unsafe_fn = true;
        } else if self.is_kw(Kw::Unsafe) {
            self.bump();
            return self.err(format!("expected `fn` or `send` after `unsafe` (`unsafe fn name()` or `unsafe send struct T`), found {}", self.describe()));
        }
        if self.is_kw(Kw::Copy) {
            self.bump();
            is_copy = true;
        }
        // `send interface Handler { ... }`
        let mut send_iface = false;
        if matches!(self.peek(), Tok::Ident(s) if s == "send") && matches!(self.peek_at(1), Tok::Kw(Kw::Interface)) {
            self.bump();
            send_iface = true;
        }
        let kind = match self.peek().clone() {
            Tok::Kw(Kw::Func) => {
                let mut f = self.func_decl(None)?;
                f.is_unsafe = unsafe_fn;
                ItemKind::Func(f)
            }
            Tok::Kw(Kw::Struct) => {
                self.bump();
                let (name, nsp) = self.ident()?;
                let tparams = self.tparams_opt()?;
                let owner = Self::owner_type(&name, &tparams, nsp);
                let fields = self.struct_fields(&owner, &mut methods)?;
                ItemKind::Struct(StructDecl { name, tparams, fields, is_copy, unsafe_send })
            }
            Tok::Kw(Kw::Enum) => {
                self.bump();
                let (name, nsp) = self.ident()?;
                let tparams = self.tparams_opt()?;
                let owner = Self::owner_type(&name, &tparams, nsp);
                let variants = self.enum_variants(&owner, &mut methods)?;
                ItemKind::Enum(EnumDecl { name, tparams, variants, is_copy })
            }
            Tok::Kw(Kw::Interface) => {
                self.bump();
                let (name, _) = self.ident()?;
                let tparams = self.tparams_opt()?;
                let mut d = self.iface_body(name, tparams)?;
                d.is_send = send_iface;
                ItemKind::Interface(d)
            }
            Tok::Kw(Kw::Type) => {
                self.bump();
                let (name, _) = self.ident()?;
                let tparams = self.tparams_opt()?;
                let newtype = !self.eat_p("=");
                let ty = self.ty()?;
                ItemKind::TypeAlias { name, tparams, ty, newtype }
            }
            Tok::Kw(Kw::Let) => {
                self.bump();
                let (name, name_span) = self.ident()?;
                let ty = if self.is_p("=") {
                    None
                } else {
                    self.colon("", &name)?;
                    Some(self.ty()?)
                };
                self.expect_p("=")?;
                let value = self.expr()?;
                ItemKind::Global { name, name_span, ty, value }
            }
            Tok::Kw(Kw::Var) => {
                return self.err("package-level values can't change: write `let` (and share changing state through a `Mutex` or an `Atomic`)");
            }
            Tok::Kw(Kw::Const) => {
                self.bump();
                let (name, _) = self.ident()?;
                let ty = if self.is_p("=") {
                    None
                } else {
                    self.colon("", &name)?;
                    Some(self.ty()?)
                };
                self.expect_p("=")?;
                let value = self.expr()?;
                ItemKind::Const { name, ty, value }
            }
            Tok::Kw(Kw::Extern) => {
                self.bump();
                let abi = match self.peek().clone() {
                    Tok::Str(s) => {
                        self.bump();
                        s
                    }
                    _ => "C".to_string(),
                };
                let mut lib = None;
                if let Tok::Ident(w) = self.peek().clone() {
                    if w == "lib" || w == "static" {
                        self.bump();
                        let Tok::Str(s) = self.peek().clone() else {
                            return self.err(format!("expected the library's name after `{}`: `extern \"C\" {} \"z\"`", w, w));
                        };
                        // The name reaches the C compiler's command line (`-lz`)
                        // or `dlopen`: only plain names, so that it can't add
                        // other options or code. (`lib` may also be a file name
                        // with a directory: `lib "vendor/libfoo.so"`.)
                        let ok_char = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '+' | '-') || (w == "lib" && c == '/');
                        if s.is_empty() || s.starts_with('-') || !s.chars().all(ok_char) {
                            return self.err(format!("`{}` is not a library name: use letters, digits, `_`, `.`, `+` and `-`{}, not starting with `-`", s.escape_default(), if w == "lib" { " (and `/`)" } else { "" }));
                        }
                        self.bump();
                        lib = Some(if w == "static" { format!("static:{}", s) } else { s });
                    }
                }
                if self.is_kw(Kw::Unsafe) {
                    return self.err("a function exported to C can't be `unsafe`: Gojo code calls it like any other function, and C never sees the marker");
                }
                if self.is_kw(Kw::Func) {
                    // `extern "C" fn name(...) {...}`: a Gojo function callable from C.
                    let mut f = self.func_decl(None)?;
                    f.is_extern_c = true;
                    ItemKind::Func(f)
                } else {
                    self.expect_p("{")?;
                    self.skip_nl();
                    let mut funcs = vec![];
                    while !self.is_p("}") {
                        if self.is_kw(Kw::Unsafe) {
                            return self.err("functions in an `extern` block are already unsafe to call: write `fn`, not `unsafe fn`");
                        }
                        let mut f = self.func_decl(None)?;
                        f.is_extern_c = true;
                        funcs.push(f);
                        self.skip_nl();
                    }
                    self.bump();
                    ItemKind::Extern(ExternBlock { abi, lib, funcs })
                }
            }
            Tok::Kw(Kw::Test) => {
                self.bump();
                let name = match self.peek().clone() {
                    Tok::Str(s) => {
                        self.bump();
                        s
                    }
                    _ => return self.err("expected test name string"),
                };
                let body = self.block()?;
                ItemKind::Test { name, body }
            }
            _ => return self.err(format!("expected declaration, found {}", self.describe())),
        };
        let mut out = vec![Item { kind, is_pub, span: self.sp_from(lo) }];
        out.extend(methods);
        Ok(out)
    }

    fn tparams_opt(&mut self) -> PResult<Vec<TParam>> {
        let mut out = vec![];
        if !self.is_p("[") {
            return Ok(out);
        }
        self.bump();
        while !self.is_p("]") {
            let (name, span) = self.ident()?;
            let mut bounds = vec![];
            let mut is_int = false;
            if !self.is_p(",") && !self.is_p("]") {
                // `[T: ordered + copy]`, `[N: int]`
                if !self.eat_p(":") {
                    return self.err(format!("expected `:` before the constraints of `{}`: `[{}: ordered]`, or `[{}]` for any type", name, name, name));
                }
                if matches!(self.peek(), Tok::Ident(s) if s == "int") && (matches!(self.peek_at(1), Tok::P(",") | Tok::P("]"))) {
                    self.bump();
                    is_int = true;
                } else {
                    bounds = self.bound_list()?;
                }
            }
            out.push(TParam { name, bounds, is_int, span });
            if !self.eat_p(",") {
                break;
            }
        }
        self.expect_p("]")?;
        Ok(out)
    }

    fn bound_list(&mut self) -> PResult<Vec<TypeExpr>> {
        let mut bounds = vec![self.bound()?];
        while self.eat_p("+") {
            bounds.push(self.bound()?);
        }
        Ok(bounds)
    }

    fn bound(&mut self) -> PResult<TypeExpr> {
        if self.is_kw(Kw::Copy) {
            let sp = self.bump().span;
            return Ok(TypeExpr { kind: TypeExprKind::Named { path: vec!["copy".into()], args: vec![] }, span: sp });
        }
        self.ty()
    }

    /// The type a method body belongs to: `Stack[T]` for `struct Stack[T any]`.
    fn owner_type(name: &str, tparams: &[TParam], span: Span) -> TypeExpr {
        let args = tparams.iter().map(|tp| TypeExpr { kind: TypeExprKind::Named { path: vec![tp.name.clone()], args: vec![] }, span: tp.span }).collect();
        TypeExpr { kind: TypeExprKind::Named { path: vec![name.to_string()], args }, span }
    }

    /// `[pub] func ...` inside a type body.
    fn body_method(&mut self, owner: &TypeExpr, methods: &mut Vec<Item>) -> PResult<bool> {
        let at = if self.is_kw(Kw::Pub) { 1 } else { 0 };
        let is_unsafe = matches!(self.peek_at(at), Tok::Kw(Kw::Unsafe)) && matches!(self.peek_at(at + 1), Tok::Kw(Kw::Func));
        let is_func = matches!(self.peek_at(at), Tok::Kw(Kw::Func)) || is_unsafe;
        if !is_func {
            return Ok(false);
        }
        let lo = self.span();
        let is_pub = self.eat_kw(Kw::Pub);
        if is_unsafe {
            self.bump();
        }
        let mut f = self.func_decl(Some(owner))?;
        f.is_unsafe = is_unsafe;
        methods.push(Item { kind: ItemKind::Func(f), is_pub, span: self.sp_from(lo) });
        self.skip_nl();
        Ok(true)
    }

    fn struct_fields(&mut self, owner: &TypeExpr, methods: &mut Vec<Item>) -> PResult<Vec<FieldDecl>> {
        self.expect_p("{")?;
        self.skip_nl();
        let mut fields = vec![];
        while !self.is_p("}") {
            if self.body_method(owner, methods)? {
                continue;
            }
            let lo = self.span();
            let is_pub = self.eat_kw(Kw::Pub);
            // Embedded field: a single type name on its own line.
            if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Newline | Tok::P("}") | Tok::P(";")) {
                let ty = self.ty()?;
                let name = match &ty.kind {
                    TypeExprKind::Named { path, .. } => path.last().unwrap().clone(),
                    _ => unreachable!(),
                };
                fields.push(FieldDecl { name, ty, default: None, is_pub, embedded: true, span: self.sp_from(lo) });
                self.end_stmt()?;
                continue;
            }
            let names = vec![self.ident()?];
            self.colon("a field", "")?;
            let ty = self.ty()?;
            let default = if self.eat_p("=") { Some(self.expr()?) } else { None };
            for (n, sp) in names {
                fields.push(FieldDecl { name: n, ty: ty.clone(), default: default.clone(), is_pub, embedded: false, span: sp });
            }
            if !self.eat_p(",") {
                self.end_stmt()?;
            }
            self.skip_nl();
        }
        self.bump();
        Ok(fields)
    }

    fn enum_variants(&mut self, owner: &TypeExpr, methods: &mut Vec<Item>) -> PResult<Vec<VariantDecl>> {
        self.expect_p("{")?;
        self.skip_nl();
        let mut vs = vec![];
        while !self.is_p("}") {
            if self.body_method(owner, methods)? {
                continue;
            }
            let (name, span) = self.ident()?;
            let mut fields = vec![];
            if self.eat_p("(") {
                let mut idx = 0;
                while !self.is_p(")") {
                    // named `r: f64` vs unnamed `os.Error`
                    let named = matches!(self.peek(), Tok::Ident(_))
                        && matches!(self.peek_at(1), Tok::P(":"));
                    if named {
                        let (n, _) = self.ident()?;
                        self.colon("a variant field", "")?;
                        let t = self.ty()?;
                        fields.push((n, t));
                    } else {
                        let t = self.ty()?;
                        fields.push((format!("_{}", idx), t));
                    }
                    idx += 1;
                    if !self.eat_p(",") {
                        break;
                    }
                }
                self.expect_p(")")?;
            }
            vs.push(VariantDecl { name, fields, span });
            if !self.eat_p(",") {
                self.end_stmt()?;
            }
            self.skip_nl();
        }
        self.bump();
        Ok(vs)
    }

    fn iface_body(&mut self, name: String, tparams: Vec<TParam>) -> PResult<IfaceDecl> {
        self.expect_p("{")?;
        self.skip_nl();
        let mut methods = vec![];
        let mut embeds = vec![];
        let mut type_set = vec![];
        while !self.is_p("}") {
            let lo = self.span();
            if self.is_kw(Kw::Unsafe) {
                return self.err("an interface method can't be `unsafe`: code calling it through the interface doesn't know which implementation it gets");
            }
            if !self.is_kw(Kw::Func) {
                // type set or embedded interface
                let mut elems = vec![];
                loop {
                    let approx = self.eat_p("~");
                    let t = self.ty()?;
                    elems.push((approx, t));
                    if !self.eat_p("|") {
                        break;
                    }
                }
                if elems.len() == 1 && !elems[0].0 {
                    let t = elems.pop().unwrap().1;
                    embeds.push(t);
                } else {
                    type_set.extend(elems);
                }
            } else {
                self.bump();
                let (mname, _) = self.ident()?;
                let dummy = TypeExpr { kind: TypeExprKind::Unit, span: lo };
                let (recv, params) = self.params_with_self(Some(&dummy))?;
                let recv_mode = match recv {
                    Some(r) => r.mode,
                    None => return Err(Diag::new(self.sp_from(lo), "interface methods take `self` as their first parameter: `fn name(self) -> T`")),
                };
                let ret = self.ret_type()?;
                methods.push(IfaceMethod { name: mname, recv_mode, params, ret, span: self.sp_from(lo) });
            }
            self.end_stmt()?;
        }
        self.bump();
        Ok(IfaceDecl { name, tparams, methods, embeds, type_set, is_send: false })
    }

    fn func_decl(&mut self, owner: Option<&TypeExpr>) -> PResult<FuncDecl> {
        let lo = self.expect_kw(Kw::Func)?;
        let (name, name_span) = self.member_name()?;
        let tparams = self.tparams_opt()?;
        let (recv, params) = self.params_with_self(owner)?;
        let assoc_owner = if recv.is_none() { owner.cloned() } else { None };
        let mut yields = None;
        let mut yield_opt = false;
        let ret = if self.eat_kw(Kw::Yield) {
            if self.eat_p("?") {
                yield_opt = true;
            }
            if self.eat_p("&") {
                yields = Some(YieldKind::Ref);
            } else if self.eat_kw(Kw::Inout) {
                yields = Some(YieldKind::Mut);
            } else {
                yields = Some(YieldKind::Ref);
            }
            Some(self.ty()?)
        } else {
            self.ret_type()?
        };
        let mut wheres = vec![];
        if self.eat_kw(Kw::Where) {
            loop {
                let (n, sp) = self.ident()?;
                self.expect_p(":")?;
                let b = self.bound_list()?;
                wheres.push((n, b, sp));
                if !self.eat_p(",") {
                    break;
                }
            }
        }
        let body = if self.is_p("{") { Some(self.block()?) } else { None };
        Ok(FuncDecl {
            name,
            name_span,
            recv,
            tparams,
            params,
            ret,
            yields,
            yield_opt,
            wheres,
            body,
            is_extern_c: false,
            is_unsafe: false,
            owner: assoc_owner,
            ext_tparams: vec![],
            span: self.sp_from(lo),
        })
    }

    /// Parameters, where the first may be `self [inout|sink]` inside a type body.
    fn params_with_self(&mut self, owner: Option<&TypeExpr>) -> PResult<(Option<Param>, Vec<Param>)> {
        let new_self = self.is_p("(") && matches!(self.peek_at(1), Tok::Kw(Kw::Inout | Kw::Sink)) && matches!(self.peek_at(2), Tok::Ident(s) if s == "self");
        let is_self = new_self || (self.is_p("(") && matches!(self.peek_at(1), Tok::Ident(s) if s == "self"));
        if !is_self {
            let ps = self.params()?;
            return Ok((None, ps));
        }
        let owner = match owner {
            Some(o) => o.clone(),
            None => return Err(Diag::new(self.toks[self.pos + 1].span, "`self` is only allowed in methods (functions declared inside a type or an `extend` block)")),
        };
        self.bump();
        let (mode, span) = if new_self {
            let m = self.mode();
            (m, self.bump().span)
        } else {
            let span = self.bump().span;
            let mlo = self.span();
            let m = self.mode();
            if m != Mode::Borrow {
                let kw = if m == Mode::Inout { "inout" } else { "sink" };
                return Err(Diag::new(mlo, format!("the mode goes before `self`: `{} self`", kw)));
            }
            (m, span)
        };
        let recv = Param { name: "self".into(), mode, ty: owner, span };
        if self.eat_p(")") {
            return Ok((Some(recv), vec![]));
        }
        self.expect_p(",")?;
        let ps = self.params_rest()?;
        Ok((Some(recv), ps))
    }

    fn mode(&mut self) -> Mode {
        if self.eat_kw(Kw::Inout) {
            Mode::Inout
        } else if self.eat_kw(Kw::Sink) {
            Mode::Sink
        } else {
            Mode::Borrow
        }
    }

    fn params(&mut self) -> PResult<Vec<Param>> {
        self.expect_p("(")?;
        self.params_rest()
    }

    /// Parameters after the opening `(`, through the closing `)`.
    fn params_rest(&mut self) -> PResult<Vec<Param>> {
        let mut out: Vec<Param> = vec![];
        while !self.is_p(")") {
            let (name, span) = self.ident()?;
            self.colon("parameter ", &name)?;
            let mode = self.mode();
            let ty = self.ty()?;
            out.push(Param { name, mode, ty, span });
            if !self.eat_p(",") {
                break;
            }
        }
        self.expect_p(")")?;
        Ok(out)
    }

    fn ret_type(&mut self) -> PResult<Option<TypeExpr>> {
        if self.is_p("!") {
            let lo = self.bump().span;
            let e = self.ty_atom()?;
            let unit = TypeExpr { kind: TypeExprKind::Unit, span: lo };
            return Ok(Some(TypeExpr { kind: TypeExprKind::Result(Box::new(unit), Box::new(e)), span: self.sp_from(lo) }));
        }
        if self.eat_p("->") {
            return Ok(Some(self.ty()?));
        }
        if self.is_p("{") || matches!(self.peek(), Tok::Newline | Tok::Eof) || self.is_kw(Kw::Where) || self.is_p("}") || self.is_p(";") || self.is_p(")") || self.is_p(",") || self.is_p("=>") || self.is_p("]") || self.is_p("=") {
            return Ok(None);
        }
        if self.type_starts() {
            return self.err(format!("expected `->` before the result type, found {}", self.describe()));
        }
        Ok(None)
    }

    // ---------- types ----------
    pub fn ty(&mut self) -> PResult<TypeExpr> {
        let lo = self.span();
        let t = self.ty_atom()?;
        if self.is_p("!") {
            self.bump();
            let e = self.ty_atom()?;
            return Ok(TypeExpr { kind: TypeExprKind::Result(Box::new(t), Box::new(e)), span: self.sp_from(lo) });
        }
        Ok(t)
    }

    fn ty_atom(&mut self) -> PResult<TypeExpr> {
        self.nested(|p| p.ty_atom_inner())
    }

    fn ty_atom_inner(&mut self) -> PResult<TypeExpr> {
        let lo = self.span();
        let kind = match self.peek().clone() {
            Tok::P("?") => {
                self.bump();
                TypeExprKind::Option(Box::new(self.ty_atom()?))
            }
            Tok::P("*") => {
                self.bump();
                TypeExprKind::Ptr(Box::new(self.ty_atom()?))
            }
            Tok::P("&") => {
                self.bump();
                return self.ty_atom();
            }
            Tok::P("[") => {
                self.bump();
                if self.eat_p("]") {
                    TypeExprKind::Slice(Box::new(self.ty_atom()?))
                } else {
                    let n = self.expr()?;
                    self.expect_p("]")?;
                    TypeExprKind::Array(Box::new(n), Box::new(self.ty_atom()?))
                }
            }
            Tok::Ident(ref s) if s == "send" && matches!(self.peek_at(1), Tok::Kw(Kw::Func)) => {
                self.bump();
                let t = self.ty_atom()?;
                return Ok(match t.kind {
                    TypeExprKind::Func { params, ret, .. } => TypeExpr { kind: TypeExprKind::Func { params, ret, send: true }, span: self.sp_from(lo) },
                    k => TypeExpr { kind: k, span: t.span },
                });
            }
            Tok::Kw(Kw::Func) => {
                self.bump();
                self.expect_p("(")?;
                let mut params = vec![];
                while !self.is_p(")") {
                    // optional parameter names: `fn(x: int)`
                    if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::P(":")) {
                        self.bump();
                        self.bump();
                    }
                    let m = if self.is_p("&") {
                        Mode::Borrow
                    } else {
                        self.mode()
                    };
                    params.push((m, self.ty()?));
                    if !self.eat_p(",") {
                        break;
                    }
                }
                self.expect_p(")")?;
                let ret = self.ret_type()?.map(Box::new);
                TypeExprKind::Func { params, ret, send: false }
            }
            Tok::P("(") => {
                self.bump();
                if self.eat_p(")") {
                    TypeExprKind::Unit
                } else {
                    let t = self.ty()?;
                    if self.eat_p(",") {
                        let mut ts = vec![t];
                        while !self.is_p(")") {
                            ts.push(self.ty()?);
                            if !self.eat_p(",") {
                                break;
                            }
                        }
                        self.expect_p(")")?;
                        TypeExprKind::Tuple(ts)
                    } else {
                        self.expect_p(")")?;
                        return Ok(t);
                    }
                }
            }
            Tok::Int(n) => {
                self.bump();
                TypeExprKind::IntLit(n)
            }
            Tok::Ident(_) => {
                let mut path = vec![self.ident()?.0];
                while self.is_p(".") && matches!(self.peek_at(1), Tok::Ident(_)) {
                    self.bump();
                    path.push(self.ident()?.0);
                }
                let mut args = vec![];
                if self.is_p("[") && !self.toks[self.pos].nl_before {
                    self.bump();
                    while !self.is_p("]") {
                        args.push(self.ty()?);
                        if !self.eat_p(",") {
                            break;
                        }
                    }
                    self.expect_p("]")?;
                }
                TypeExprKind::Named { path, args }
            }
            _ => return self.err(format!("expected type, found {}", self.describe())),
        };
        Ok(TypeExpr { kind, span: self.sp_from(lo) })
    }

    // ---------- blocks & statements ----------
    pub fn block(&mut self) -> PResult<Block> {
        self.nested(|p| p.block_inner())
    }

    fn block_inner(&mut self) -> PResult<Block> {
        let lo = self.expect_p("{")?;
        let save = self.allow_struct;
        self.allow_struct = true;
        self.skip_nl();
        let stmts = self.stmts_until_close()?;
        self.bump();
        self.allow_struct = save;
        Ok(Block { stmts, span: self.sp_from(lo) })
    }

    /// The statements of a block, up to (not including) its `}`. After a
    /// `let ... else`, the rest of the block becomes its success branch.
    fn stmts_until_close(&mut self) -> PResult<Vec<Stmt>> {
        let mut stmts = vec![];
        while !self.is_p("}") {
            if matches!(self.peek(), Tok::Eof) {
                return Err(self.with_opener(Diag::new(self.span(), "unexpected end of file; missing `}`"), "}"));
            }
            if matches!(self.peek(), Tok::Kw(Kw::Let | Kw::Var)) && self.peek_at(1) == &Tok::P("(") {
                self.let_tuple(&mut stmts)?;
            } else {
                stmts.push(self.stmt()?);
            }
            self.end_stmt()?;
            if matches!(stmts.last().map(|s| &s.kind), Some(StmtKind::LetElse { .. })) {
                let st = stmts.pop().unwrap();
                let StmtKind::LetElse { pat, expr, else_ } = st.kind else { unreachable!() };
                let rlo = self.span();
                let rest = self.nested(|p| p.stmts_until_close())?;
                let rest = Block { stmts: rest, span: self.sp_from(rlo) };
                let span = st.span;
                let e = Expr { kind: ExprKind::LetElse { pat, expr: Box::new(expr), else_, rest }, span };
                stmts.push(Stmt { kind: StmtKind::Expr(e), span });
                break;
            }
        }
        Ok(stmts)
    }

    /// `let (a, (b, _)) = e`: a hidden variable holding `e`, then one `let` per
    /// name, taking its element (`let a = t.0`).
    fn let_tuple(&mut self, out: &mut Vec<Stmt>) -> PResult<()> {
        let lo = self.span();
        let mutable = self.is_kw(Kw::Var);
        self.bump();
        let pat = self.pattern()?;
        let ty = if self.eat_p(":") { Some(self.ty()?) } else { None };
        self.expect_p("=")?;
        let init = self.expr()?;
        if self.is_kw(Kw::Else) && !mutable && ty.is_none() {
            // `let (x, 0) = pair else { ... }`
            self.bump();
            let else_ = self.block()?;
            out.push(Stmt { kind: StmtKind::LetElse { pat, expr: init, else_ }, span: self.sp_from(lo) });
            return Ok(());
        }
        let sp = self.sp_from(lo);
        self.destructure(pat, ty, init, mutable, sp, out)
    }

    fn destructure(&mut self, pat: Pattern, ty: Option<TypeExpr>, init: Expr, mutable: bool, sp: Span, out: &mut Vec<Stmt>) -> PResult<()> {
        match pat.kind {
            PatternKind::Ident(name) => {
                out.push(Stmt { kind: StmtKind::Let { name, name_span: pat.span, ty, init: Some(init), mutable }, span: sp });
                Ok(())
            }
            PatternKind::Wild => {
                // `_` still evaluates its part, like `_ = e`
                self.tuple_tmps += 1;
                let name = format!("__tup{}", self.tuple_tmps);
                out.push(Stmt { kind: StmtKind::Let { name, name_span: pat.span, ty, init: Some(init), mutable: false }, span: sp });
                Ok(())
            }
            PatternKind::Tuple(ps) => {
                self.tuple_tmps += 1;
                let tmp = format!("__tup{}", self.tuple_tmps);
                out.push(Stmt { kind: StmtKind::Let { name: tmp.clone(), name_span: pat.span, ty, init: Some(init), mutable }, span: sp });
                for (i, p) in ps.into_iter().enumerate() {
                    if matches!(p.kind, PatternKind::Wild) {
                        continue;
                    }
                    let base = Expr { kind: ExprKind::Ident(tmp.clone()), span: p.span };
                    let elem = Expr { kind: ExprKind::Field(Box::new(base), format!("_{}", i), p.span), span: p.span };
                    self.destructure(p, None, elem, mutable, sp, out)?;
                }
                Ok(())
            }
            _ => Err(Diag::new(pat.span, "`let` takes a name or a tuple of names here: `let (a, b) = pair`; use `match` or `if let` for other patterns")),
        }
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let st = self.stmt_inner()?;
        Ok(match st.kind {
            StmtKind::With { .. } | StmtKind::WithLocks { .. } => {
                let span = st.span;
                Stmt { kind: StmtKind::Expr(Expr { kind: ExprKind::With(Box::new(st)), span }), span }
            }
            _ => st,
        })
    }

    fn stmt_inner(&mut self) -> PResult<Stmt> {
        let lo = self.span();
        let kind = match self.peek().clone() {
            Tok::Kw(Kw::Let) | Tok::Kw(Kw::Var) => {
                let mutable = self.is_kw(Kw::Var);
                self.bump();
                let simple = matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::P(":" | "=" | "}" | ";") | Tok::Newline | Tok::Eof);
                if !simple && !mutable {
                    // `let .Circle(r) = s else { ... }`
                    let pat = self.pattern()?;
                    self.expect_p("=")?;
                    let expr = self.expr()?;
                    if !self.is_kw(Kw::Else) {
                        return Err(Diag::new(pat.span, "`let` with a pattern that may not match needs an `else` that leaves: `let .Circle(r) = s else { return }`"));
                    }
                    self.bump();
                    let else_ = self.block()?;
                    return Ok(Stmt { kind: StmtKind::LetElse { pat, expr, else_ }, span: self.sp_from(lo) });
                }
                let (name, name_span) = self.ident()?;
                let ty = if self.is_p("=") || matches!(self.peek(), Tok::Newline) || self.is_p("}") || self.is_p(";") {
                    None
                } else {
                    self.colon("", &name)?;
                    Some(self.ty()?)
                };
                let init = if self.eat_p("=") { Some(self.expr()?) } else { None };
                if self.is_kw(Kw::Else) && !mutable && ty.is_none() {
                    if let Some(expr) = init {
                        // `let x = opt else { ... }`: unwraps an optional
                        self.bump();
                        let else_ = self.block()?;
                        let inner = Pattern { kind: PatternKind::Ident(name), span: name_span };
                        let pat = Pattern { kind: PatternKind::Path(vec!["some".into()], Some(vec![inner])), span: name_span };
                        return Ok(Stmt { kind: StmtKind::LetElse { pat, expr, else_ }, span: self.sp_from(lo) });
                    }
                }
                StmtKind::Let { name, name_span, ty, init, mutable }
            }
            Tok::Kw(Kw::Return) => {
                self.bump();
                if matches!(self.peek(), Tok::Newline | Tok::Eof) || self.is_p("}") || self.is_p(";") {
                    StmtKind::Return(None)
                } else {
                    StmtKind::Return(Some(self.expr()?))
                }
            }
            Tok::Kw(Kw::Yield) => {
                self.bump();
                if matches!(self.peek(), Tok::Newline | Tok::Eof) || self.is_p("}") {
                    StmtKind::Yield(None)
                } else {
                    StmtKind::Yield(Some(self.expr()?))
                }
            }
            Tok::Kw(Kw::Break) | Tok::Kw(Kw::Continue) => {
                let is_break = self.is_kw(Kw::Break);
                self.bump();
                let label = match self.peek().clone() {
                    Tok::Ident(l) if !self.toks[self.pos].nl_before => {
                        let sp = self.bump().span;
                        Some((l, sp))
                    }
                    _ => None,
                };
                if is_break {
                    StmtKind::Break(label)
                } else {
                    StmtKind::Continue(label)
                }
            }
            Tok::Ident(l) if matches!(self.peek_at(1), Tok::P(":")) && matches!(self.peek_at(2), Tok::Kw(Kw::For | Kw::While | Kw::Loop)) => {
                let sp = self.bump().span;
                self.bump();
                let k = self.bump();
                StmtKind::For(self.loop_body(&k.tok)?, Some((l, sp)))
            }
            Tok::Kw(Kw::Defer) => {
                self.bump();
                StmtKind::Defer(Box::new(self.stmt()?))
            }
            Tok::Kw(Kw::ErrDefer) => {
                self.bump();
                StmtKind::ErrDefer(Box::new(self.stmt()?))
            }
            Tok::Kw(Kw::With) => {
                self.bump();
                let (name, name_span) = self.ident()?;
                if self.is_p(",") {
                    let mut names = vec![(name, name_span)];
                    while self.eat_p(",") {
                        names.push(self.ident()?);
                    }
                    self.expect_p("=")?;
                    match self.peek().clone() {
                        Tok::Ident(l) if l == "lock" => {
                            self.bump();
                        }
                        _ => return self.err("`with a, b = ...` takes several locks at once: `with a, b = lock(m1, m2) { }`"),
                    }
                    self.expect_p("(")?;
                    let mut mutexes = vec![];
                    while !self.is_p(")") {
                        mutexes.push(self.expr()?);
                        if !self.eat_p(",") {
                            break;
                        }
                    }
                    self.expect_p(")")?;
                    let body = self.block()?;
                    return Ok(Stmt { kind: StmtKind::WithLocks { names, mutexes, body }, span: self.sp_from(lo) });
                }
                self.expect_p("=")?;
                let save = self.allow_struct;
                self.allow_struct = false;
                let expr = self.expr()?;
                self.allow_struct = save;
                let body = self.block()?;
                StmtKind::With { name, name_span, expr, body }
            }
            Tok::Kw(Kw::For | Kw::While | Kw::Loop) => {
                let k = self.bump();
                StmtKind::For(self.loop_body(&k.tok)?, None)
            }
            Tok::Ident(ref p) if p == "par" && matches!(self.peek_at(1), Tok::Kw(Kw::For)) => {
                self.bump();
                self.bump();
                match self.for_loop()? {
                    ForLoop::In { first, second, iter, by_ref, body } => StmtKind::ParFor { first, second, iter, by_ref, body },
                    _ => return self.err("`par for` needs `in`: `par for x in &v { }`, `par for i in 0..n { }`"),
                }
            }
            Tok::Kw(Kw::Select) => {
                self.bump();
                StmtKind::Select(self.select_arms()?)
            }
            Tok::Ident(ref s) if s == "_" && matches!(self.peek_at(1), Tok::P("=")) => {
                self.bump();
                self.bump();
                StmtKind::Discard(self.expr()?)
            }
            _ => {
                let e = self.expr()?;
                if let Tok::P(p) = self.peek().clone() {
                    match p {
                        "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "+%=" | "-%=" | "*%=" | "&=" | "|=" | "^=" | "<<=" | ">>=" => {
                            self.bump();
                            let value = self.expr()?;
                            return Ok(Stmt { kind: StmtKind::Assign { place: e, op: p, value }, span: self.sp_from(lo) });
                        }
                        "<-" => {
                            self.bump();
                            let value = self.expr()?;
                            return Ok(Stmt { kind: StmtKind::Send { chan: e, value }, span: self.sp_from(lo) });
                        }
                        _ => {}
                    }
                }
                StmtKind::Expr(e)
            }
        };
        Ok(Stmt { kind, span: self.sp_from(lo) })
    }

    /// `select { let x = <-a => ...  <-b => ...  ch <- v => ...  _ => ... }`
    fn select_arms(&mut self) -> PResult<Vec<SelectArm>> {
        self.expect_p("{")?;
        let save = self.allow_struct;
        self.allow_struct = true;
        self.skip_nl();
        let mut arms = vec![];
        while !self.is_p("}") {
            let lo = self.span();
            let d = self.depth_at(self.pos);
            let save_arrow = self.arrow_depth.replace(d);
            let kind = if self.eat_kw(Kw::Let) {
                let (n, sp) = self.ident()?;
                self.expect_p("=")?;
                self.expect_p("<-")?;
                let chan = self.unary()?;
                SelectKind::Recv { bind: Some((n, sp)), chan }
            } else if self.eat_p("<-") {
                let chan = self.unary()?;
                SelectKind::Recv { bind: None, chan }
            } else if matches!(self.peek(), Tok::Ident(s) if s == "_") && matches!(self.peek_at(1), Tok::P("=>")) {
                self.bump();
                SelectKind::Default
            } else {
                let chan = self.unary()?;
                if self.is_p("<-") {
                    self.bump();
                    let value = self.expr()?;
                    SelectKind::Send { chan, value }
                } else if self.is_p("=>") || self.is_kw(Kw::If) {
                    SelectKind::Event(chan)
                } else {
                    return self.err(format!("expected a `select` arm (`let x = <-rx`, `<-rx`, `tx <- v`, `time.after(d)`, `cancelled` or `_`), found {}", self.describe()));
                }
            };
            let guard = if self.is_kw(Kw::If) {
                self.bump();
                let save = self.allow_struct;
                self.allow_struct = false;
                let g = self.expr()?;
                self.allow_struct = save;
                Some(g)
            } else {
                None
            };
            self.arrow_depth = save_arrow;
            self.expect_p("=>")?;
            let body = self.expr_or_assign()?;
            let bspan = body.span;
            let block = match body.kind {
                ExprKind::Block(b) => b,
                _ => Block { stmts: vec![Stmt { kind: StmtKind::Expr(body), span: bspan }], span: bspan },
            };
            arms.push(SelectArm { kind, guard, body: block, span: self.sp_from(lo) });
            self.eat_p(",");
            self.skip_nl();
        }
        self.bump();
        self.allow_struct = save;
        Ok(arms)
    }

    /// After `for`, `while` or `loop`.
    fn loop_body(&mut self, kw: &Tok) -> PResult<ForLoop> {
        match kw {
            Tok::Kw(Kw::Loop) => Ok(ForLoop::Infinite(self.block()?)),
            Tok::Kw(Kw::While) => {
                let save = self.allow_struct;
                self.allow_struct = false;
                let cond = self.expr()?;
                self.allow_struct = save;
                Ok(ForLoop::Cond(cond, self.block()?))
            }
            _ => self.for_loop(),
        }
    }

    fn for_loop(&mut self) -> PResult<ForLoop> {
        if self.is_p("{") {
            return self.err("`for` iterates over something: `for x in xs { }`; a loop without a condition is `loop { }`");
        }
        let is_in = matches!(self.peek(), Tok::Ident(_))
            && (matches!(self.peek_at(1), Tok::Kw(Kw::In)) || (matches!(self.peek_at(1), Tok::P(",")) && matches!(self.peek_at(2), Tok::Ident(_)) && matches!(self.peek_at(3), Tok::Kw(Kw::In))));
        let save = self.allow_struct;
        self.allow_struct = false;
        let r = if is_in {
            let first = self.ident()?;
            let second = if self.eat_p(",") { Some(self.ident()?) } else { None };
            self.expect_kw(Kw::In)?;
            let by_ref = self.eat_p("&");
            let iter = self.expr()?;
            self.allow_struct = save;
            let body = self.block()?;
            ForLoop::In { first, second, iter, by_ref, body }
        } else {
            return self.err("`for` iterates over something: `for x in xs { }`; a loop with a condition is `while cond { }`");
        };
        self.allow_struct = save;
        Ok(r)
    }

    // ---------- expressions ----------
    pub fn expr(&mut self) -> PResult<Expr> {
        let lo = self.span();
        let e = self.range_expr()?;
        if self.is_kw(Kw::Catch) {
            self.bump();
            let bind = if let Tok::Ident(n) = self.peek().clone() {
                let sp = self.bump().span;
                Some((n, sp))
            } else {
                None
            };
            let body = self.block()?;
            return Ok(Expr { kind: ExprKind::Catch { expr: Box::new(e), bind, body }, span: self.sp_from(lo) });
        }
        Ok(e)
    }

    fn range_expr(&mut self) -> PResult<Expr> {
        let lo = self.span();
        let e = self.binary(0)?;
        if self.is_p("..") || self.is_p("..=") {
            let inclusive = self.is_p("..=");
            self.bump();
            let hi = self.binary(0)?;
            return Ok(Expr { kind: ExprKind::Range { lo: Box::new(e), hi: Box::new(hi), inclusive }, span: self.sp_from(lo) });
        }
        Ok(e)
    }

    fn binop_prec(p: &str) -> Option<(u8, bool)> {
        // (precedence, right-assoc)
        Some(match p {
            "??" => (1, true),
            "||" => (2, false),
            "&&" => (3, false),
            "==" | "!=" | "<" | "<=" | ">" | ">=" => (4, false),
            "|" => (5, false),
            "^" => (6, false),
            "&" => (7, false),
            "<<" | ">>" => (8, false),
            "+" | "-" | "+%" | "-%" => (9, false),
            "*" | "/" | "%" | "*%" => (10, false),
            _ => return None,
        })
    }

    fn binary(&mut self, min: u8) -> PResult<Expr> {
        self.nested(|p| p.binary_inner(min))
    }

    fn binary_inner(&mut self, min: u8) -> PResult<Expr> {
        let lo = self.span();
        let saved = self.measure();
        let r = self.binary_chain(min, lo);
        self.measured(saved);
        r
    }

    fn binary_chain(&mut self, min: u8, lo: Span) -> PResult<Expr> {
        let mut lhs = self.unary()?;
        loop {
            let p = match self.peek() {
                Tok::P(p) => *p,
                _ => break,
            };
            let (prec, right) = match Self::binop_prec(p) {
                Some(x) => x,
                None => break,
            };
            if prec < min || prec == 0 {
                break;
            }
            self.bump();
            // allow line break after a binary operator
            while matches!(self.peek(), Tok::Newline) {
                self.bump();
            }
            let rhs = self.binary(if right { prec } else { prec + 1 })?;
            let span = self.sp_from(lo);
            self.grown()?;
            lhs = if p == "??" {
                Expr { kind: ExprKind::Coalesce(Box::new(lhs), Box::new(rhs)), span }
            } else {
                Expr { kind: ExprKind::Binary(p, Box::new(lhs), Box::new(rhs)), span }
            };
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> PResult<Expr> {
        self.nested(|p| p.unary_inner())
    }

    fn unary_inner(&mut self) -> PResult<Expr> {
        let lo = self.span();
        let op = match self.peek() {
            Tok::P(p) if matches!(*p, "-" | "!" | "~" | "&" | "<-" | "*") => *p,
            Tok::Kw(Kw::Go) => {
                self.bump();
                let e = self.unary()?;
                return Ok(Expr { kind: ExprKind::Go(Box::new(e)), span: self.sp_from(lo) });
            }
            Tok::Kw(Kw::Try) => {
                self.bump();
                let e = self.unary()?;
                let wrap = if self.is_kw(Kw::Else) {
                    self.bump();
                    Some(Box::new(self.postfix()?))
                } else {
                    None
                };
                return Ok(Expr { kind: ExprKind::Try { expr: Box::new(e), wrap }, span: self.sp_from(lo) });
            }
            _ => return self.postfix(),
        };
        self.bump();
        let e = self.unary()?;
        let span = self.sp_from(lo);
        Ok(match op {
            "&" => Expr { kind: ExprKind::Ref(Box::new(e)), span },
            "<-" => Expr { kind: ExprKind::Recv(Box::new(e)), span },
            "-" => match e.kind {
                ExprKind::Float(f) => Expr { kind: ExprKind::Float(-f), span },
                _ => Expr { kind: ExprKind::Unary("-", Box::new(e)), span },
            },
            _ => Expr { kind: ExprKind::Unary(op, Box::new(e)), span },
        })
    }

    fn looks_like_type_path(e: &Expr) -> bool {
        match &e.kind {
            ExprKind::Ident(_) => true,
            ExprKind::Field(b, _, _) => Self::looks_like_type_path(b),
            ExprKind::Index(b, _) | ExprKind::TypeArgs(b, _) => Self::looks_like_type_path(b),
            ExprKind::TypeExpr(_) => true,
            _ => false,
        }
    }

    fn postfix(&mut self) -> PResult<Expr> {
        let saved = self.measure();
        let r = self.postfix_chain();
        self.measured(saved);
        r
    }

    fn postfix_chain(&mut self) -> PResult<Expr> {
        let lo = self.span();
        let mut e = self.primary()?;
        let mut first = true;
        loop {
            // every iteration after the first follows a wrap of `e`
            if !first {
                self.grown()?;
            }
            first = false;
            if self.dot_continues() {
                self.bump();
            }
            let nl = self.toks[self.pos].nl_before;
            if self.is_p(".") {
                self.bump();
                let (name, nsp) = self.member_name()?;
                e = Expr { kind: ExprKind::Field(Box::new(e), name, nsp), span: self.sp_from(lo) };
            } else if self.is_p("?.") {
                self.bump();
                let (name, _) = self.ident()?;
                e = Expr { kind: ExprKind::OptField(Box::new(e), name), span: self.sp_from(lo) };
            } else if self.is_p("(") && !nl {
                self.bump();
                let save = self.allow_struct;
                self.allow_struct = true;
                let mut args = vec![];
                while !self.is_p(")") {
                    args.push(self.expr()?);
                    if !self.eat_p(",") {
                        break;
                    }
                }
                self.allow_struct = save;
                self.expect_p(")")?;
                e = Expr { kind: ExprKind::Call(Box::new(e), args), span: self.sp_from(lo) };
            } else if self.is_p("[") && !nl {
                self.bump();
                let save_struct = self.allow_struct;
                self.allow_struct = true;
                // slice `a[lo..hi]`, `a[..hi]`, `a[lo..]`
                if self.is_p("..") {
                    self.bump();
                    let hi = if self.is_p("]") { None } else { Some(Box::new(self.binary(0)?)) };
                    self.expect_p("]")?;
                    self.allow_struct = save_struct;
                    e = Expr { kind: ExprKind::Slice(Box::new(e), None, hi), span: self.sp_from(lo) };
                    continue;
                }
                let save = self.pos;
                let attempt: PResult<Vec<Expr>> = (|| {
                    let mut xs = vec![];
                    while !self.is_p("]") {
                        let x = self.binary(0)?;
                        if self.is_p("..") {
                            // slice
                            self.bump();
                            let hi = if self.is_p("]") { None } else { Some(Box::new(self.binary(0)?)) };
                            let span = x.span;
                            xs.push(Expr { kind: ExprKind::Range { lo: Box::new(x), hi: Box::new(hi.map(|h| *h).unwrap_or(Expr { kind: ExprKind::Ident("\0open".into()), span })), inclusive: false }, span });
                            break;
                        }
                        xs.push(x);
                        if !self.eat_p(",") {
                            break;
                        }
                    }
                    if !self.is_p("]") {
                        return self.err("expected `]`");
                    }
                    Ok(xs)
                })();
                match attempt {
                    Ok(xs) => {
                        self.bump();
                        if xs.len() == 1 {
                            if let ExprKind::Range { .. } = &xs[0].kind {
                                let x = xs.into_iter().next().unwrap();
                                if let ExprKind::Range { lo: rlo, hi: rhi, .. } = x.kind {
                                    let hi = match &rhi.kind {
                                        ExprKind::Ident(s) if s == "\0open" => None,
                                        _ => Some(rhi),
                                    };
                                    e = Expr { kind: ExprKind::Slice(Box::new(e), Some(rlo), hi), span: self.sp_from(lo) };
                                }
                                self.allow_struct = save_struct;
                                continue;
                            }
                        }
                        e = Expr { kind: ExprKind::Index(Box::new(e), xs), span: self.sp_from(lo) };
                    }
                    Err(_) => {
                        self.pos = save;
                        let mut ts = vec![];
                        while !self.is_p("]") {
                            ts.push(self.ty()?);
                            if !self.eat_p(",") {
                                break;
                            }
                        }
                        self.expect_p("]")?;
                        e = Expr { kind: ExprKind::TypeArgs(Box::new(e), ts), span: self.sp_from(lo) };
                    }
                }
                self.allow_struct = save_struct;
            } else if self.is_p("{") && self.allow_struct && Self::looks_like_type_path(&e) && !nl {
                e = self.struct_lit(e, lo)?;
            } else {
                break;
            }
        }
        Ok(e)
    }

    fn struct_lit(&mut self, ty: Expr, lo: Span) -> PResult<Expr> {
        self.expect_p("{")?;
        let save = self.allow_struct;
        self.allow_struct = true;
        self.skip_nl();
        let mut fields = vec![];
        let mut positional = vec![];
        let mut kv = vec![];
        while !self.is_p("}") {
            if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::P(":")) {
                let (n, sp) = self.ident()?;
                self.bump();
                self.skip_nl();
                let v = self.expr()?;
                fields.push((n, sp, v));
            } else {
                let k = self.expr()?;
                if self.eat_p(":") {
                    self.skip_nl();
                    let v = self.expr()?;
                    kv.push((k, v));
                } else {
                    positional.push(k);
                }
            }
            self.skip_nl();
            if !self.eat_p(",") {
                self.skip_nl();
                break;
            }
            self.skip_nl();
        }
        self.expect_p("}")?;
        self.allow_struct = save;
        Ok(Expr { kind: ExprKind::StructLit { ty: Box::new(ty), fields, positional, kv }, span: self.sp_from(lo) })
    }

    fn is_closure_start(&self) -> bool {
        // at `(`: find matching `)`, then `=>` (optionally after a return type)
        let mut depth = 0i32;
        let mut i = self.pos;
        while i < self.toks.len() {
            match &self.toks[i].tok {
                Tok::P("(") => depth += 1,
                Tok::P(")") => {
                    depth -= 1;
                    if depth == 0 {
                        return matches!(self.toks.get(i + 1).map(|t| &t.tok), Some(Tok::P("=>")));
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
            i += 1;
        }
        false
    }

    fn primary(&mut self) -> PResult<Expr> {
        let lo = self.span();
        let kind = match self.peek().clone() {
            Tok::Int(n) => {
                self.bump();
                ExprKind::Int(n)
            }
            Tok::Float(f) => {
                self.bump();
                ExprKind::Float(f)
            }
            Tok::Str(s) => {
                self.bump();
                ExprKind::Str(s)
            }
            Tok::Interp(parts) => {
                self.bump();
                let mut out = vec![];
                for p in parts {
                    match p {
                        IPart::Lit(s) => out.push(InterpPart::Lit(s)),
                        IPart::Expr { toks, spec, span } => {
                            let mut sub = Parser::new(self.file, toks);
                            sub.nest = self.nest;
                            let e = sub.expr()?;
                            if !matches!(sub.peek(), Tok::Eof) {
                                return Err(Diag::new(sub.span(), format!("expected `}}` to end the interpolation, found {}", sub.describe())));
                            }
                            out.push(InterpPart::Expr(Box::new(e), spec, span));
                        }
                    }
                }
                ExprKind::Interp(out)
            }
            Tok::Char(c) => {
                self.bump();
                ExprKind::Char(c)
            }
            Tok::Kw(Kw::True) => {
                self.bump();
                ExprKind::Bool(true)
            }
            Tok::Kw(Kw::False) => {
                self.bump();
                ExprKind::Bool(false)
            }
            Tok::Kw(Kw::None) => {
                self.bump();
                ExprKind::None
            }
            Tok::Ident(s) => {
                let arrow = self.arrow_depth;
                if matches!(self.peek_at(1), Tok::P("=>")) && !self.toks[self.pos + 1].nl_before && (arrow.is_none() || arrow != Some(self.depth_at(self.pos))) {
                    return self.bare_closure();
                }
                self.bump();
                ExprKind::Ident(s)
            }
            Tok::P("(") => {
                if self.is_closure_start() {
                    return self.closure();
                }
                self.bump();
                let save = self.allow_struct;
                self.allow_struct = true;
                let e = self.expr()?;
                if self.eat_p(",") {
                    // `(a, b)`
                    let mut xs = vec![e];
                    while !self.is_p(")") {
                        xs.push(self.expr()?);
                        if !self.eat_p(",") {
                            break;
                        }
                    }
                    self.allow_struct = save;
                    self.expect_p(")")?;
                    return Ok(Expr { kind: ExprKind::Tuple(xs), span: self.sp_from(lo) });
                }
                self.allow_struct = save;
                self.expect_p(")")?;
                return Ok(Expr { kind: e.kind, span: self.sp_from(lo) });
            }
            Tok::P("[") => {
                let type_follows = matches!(self.peek_at(2), Tok::Ident(_) | Tok::P("?") | Tok::P("[") | Tok::P("*") | Tok::Kw(Kw::Func)) && !self.toks[(self.pos + 2).min(self.toks.len() - 1)].nl_before;
                if matches!(self.peek_at(1), Tok::P(":")) && matches!(self.peek_at(2), Tok::P("]")) {
                    // `[:]`: an empty map
                    self.bump();
                    self.bump();
                    self.bump();
                    ExprKind::MapLit(vec![])
                } else if matches!(self.peek_at(1), Tok::P("]")) && !type_follows {
                    self.bump();
                    self.bump();
                    ExprKind::VecLit(vec![])
                } else if matches!(self.peek_at(1), Tok::P("]")) {
                    let t = self.ty()?;
                    ExprKind::TypeExpr(t)
                } else {
                    self.bump();
                    let save = self.allow_struct;
                    self.allow_struct = true;
                    self.skip_nl();
                    let mut xs = vec![];
                    let mut kvs = vec![];
                    // `if` / `for` / `..` elements make it a ListComp
                    let mut elems: Vec<ListElem> = vec![];
                    let mut comp = false;
                    while !self.is_p("]") {
                        if kvs.is_empty() && (self.is_p("..") || self.is_kw(Kw::For)) {
                            if self.eat_p("..") {
                                elems.push(ListElem::Spread(self.expr()?));
                            } else {
                                self.bump();
                                elems.push(ListElem::For(self.for_loop()?));
                            }
                            comp = true;
                            self.skip_nl();
                            if !self.eat_p(",") {
                                break;
                            }
                            self.skip_nl();
                            continue;
                        }
                        let x = self.expr()?;
                        if matches!(x.kind, ExprKind::If { else_: None, .. } | ExprKind::IfLet { else_: None, .. }) {
                            comp = true;
                        }
                        if !comp && (xs.is_empty() && kvs.is_empty() || !kvs.is_empty()) && self.eat_p(":") {
                            // `["a": 1]`
                            self.skip_nl();
                            kvs.push((x, self.expr()?));
                        } else if !kvs.is_empty() {
                            return self.err("every entry of a map literal is `key: value`");
                        } else {
                            elems.push(ListElem::Expr(x.clone()));
                            xs.push(x);
                        }
                        self.skip_nl();
                        if !self.eat_p(",") {
                            break;
                        }
                        self.skip_nl();
                    }
                    self.allow_struct = save;
                    self.expect_p("]")?;
                    if !kvs.is_empty() {
                        return Ok(Expr { kind: ExprKind::MapLit(kvs), span: self.sp_from(lo) });
                    }
                    if comp {
                        return Ok(Expr { kind: ExprKind::ListComp(elems), span: self.sp_from(lo) });
                    }
                    // `[N]T` array type
                    if xs.len() == 1 && !self.toks[self.pos].nl_before && matches!(self.peek(), Tok::Ident(_) | Tok::P("?") | Tok::Kw(Kw::Func)) {
                        let elem = self.ty_atom()?;
                        let n = xs.pop().unwrap();
                        ExprKind::TypeExpr(TypeExpr { kind: TypeExprKind::Array(Box::new(n), Box::new(elem)), span: self.sp_from(lo) })
                    } else {
                        ExprKind::VecLit(xs)
                    }
                }
            }
            Tok::P("?") => {
                let t = self.ty()?;
                ExprKind::TypeExpr(t)
            }
            Tok::P("{") => ExprKind::Block(self.block()?),
            Tok::Kw(Kw::Return) | Tok::Kw(Kw::Break) | Tok::Kw(Kw::Continue) => {
                // `match x { none => return 0 }`
                let st = self.stmt()?;
                let span = st.span;
                ExprKind::Block(Block { stmts: vec![st], span })
            }
            Tok::Kw(Kw::Unsafe) => {
                self.bump();
                ExprKind::Unsafe(self.block()?)
            }
            Tok::Kw(Kw::Isolate) => {
                self.bump();
                ExprKind::Isolate(self.block()?)
            }
            Tok::Kw(Kw::Scope) => {
                self.bump();
                let name = match self.peek().clone() {
                    Tok::Ident(n) => {
                        let sp = self.bump().span;
                        Some((n, sp))
                    }
                    _ => None,
                };
                ExprKind::Scope { name, body: self.block()?, implicit: false }
            }
            Tok::P(".") if matches!(self.peek_at(1), Tok::P("{")) => {
                // `.{x: 1}`: a struct literal whose type comes from the context
                let ty = Expr { kind: ExprKind::Ident("_".into()), span: self.span() };
                self.bump();
                return self.struct_lit(ty, lo);
            }
            Tok::P(".") => {
                self.bump();
                let (n, _) = self.ident()?;
                ExprKind::DotVariant(n)
            }
            Tok::Kw(Kw::If) => return self.if_expr(),
            Tok::Kw(Kw::With) => match self.stmt()?.kind {
                StmtKind::Expr(e) => return Ok(e),
                _ => return self.err("expected a `with` block"),
            },
            Tok::Kw(Kw::Match) => {
                self.bump();
                let save = self.allow_struct;
                self.allow_struct = false;
                let scrut = self.expr()?;
                self.allow_struct = save;
                self.expect_p("{")?;
                let save = self.allow_struct;
                self.allow_struct = true;
                self.skip_nl();
                let mut arms = vec![];
                while !self.is_p("}") {
                    let alo = self.span();
                    let pat = self.pattern()?;
                    let guard = if self.eat_kw(Kw::If) { Some(self.arm_head(|p| p.expr())?) } else { None };
                    self.expect_p("=>")?;
                    let body = self.expr_or_assign()?;
                    arms.push(Arm { pat, guard, body, span: self.sp_from(alo) });
                    self.eat_p(",");
                    self.skip_nl();
                }
                self.bump();
                self.allow_struct = save;
                ExprKind::Match { scrut: Box::new(scrut), arms }
            }
            Tok::Kw(Kw::Func) => {
                // `fn name` inside a function: most likely a `}` is missing above
                if matches!(self.peek_at(1), Tok::Ident(_)) {
                    let d = Diag::new(self.span(), "functions can't be declared inside other functions").note(None, "if this is meant to be a separate function, a `}` is missing above it; for a local function, use a closure: `let name = (x) => ...`");
                    return Err(self.with_opener(d, "}"));
                }
                // function literal: fn(x int) int { ... }
                self.bump();
                let params = self.params()?;
                let ret = self.ret_type()?;
                let body = self.block()?;
                let bspan = body.span;
                ExprKind::Closure {
                    params: params.into_iter().map(|p| ClosureParam { name: p.name, mode: p.mode, ty: Some(p.ty), span: p.span }).collect(),
                    ret,
                    body: Box::new(Expr { kind: ExprKind::Block(body), span: bspan }),
                }
            }
            _ => return self.err(format!("expected expression, found {}", self.describe())),
        };
        Ok(Expr { kind, span: self.sp_from(lo) })
    }

    fn closure(&mut self) -> PResult<Expr> {
        let lo = self.expect_p("(")?;
        let mut params = vec![];
        while !self.is_p(")") {
            let (name, span) = self.ident()?;
            let (mode, ty) = if self.is_p(",") || self.is_p(")") {
                (Mode::Borrow, None)
            } else {
                self.colon("parameter ", &name)?;
                (self.mode(), Some(self.ty()?))
            };
            params.push(ClosureParam { name, mode, ty, span });
            if !self.eat_p(",") {
                break;
            }
        }
        self.expect_p(")")?;
        self.expect_p("=>")?;
        let save = self.allow_struct;
        self.allow_struct = true;
        let body = self.expr_or_assign()?;
        self.allow_struct = save;
        Ok(Expr { kind: ExprKind::Closure { params, ret: None, body: Box::new(body) }, span: self.sp_from(lo) })
    }

    /// `x => body`: a closure with one parameter, its type inferred.
    fn bare_closure(&mut self) -> PResult<Expr> {
        let lo = self.span();
        let (name, span) = self.ident()?;
        self.expect_p("=>")?;
        let save = self.allow_struct;
        self.allow_struct = true;
        let body = self.expr_or_assign()?;
        self.allow_struct = save;
        let params = vec![ClosureParam { name, mode: Mode::Borrow, ty: None, span }];
        Ok(Expr { kind: ExprKind::Closure { params, ret: None, body: Box::new(body) }, span: self.sp_from(lo) })
    }

    /// An expression, or an assignment wrapped in a block (closure and match-arm bodies).
    fn expr_or_assign(&mut self) -> PResult<Expr> {
        let blo = self.span();
        let mut body = self.expr()?;
        if let Tok::P(p) = self.peek().clone() {
            if matches!(p, "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "+%=" | "-%=" | "*%=" | "&=" | "|=" | "^=" | "<<=" | ">>=") {
                self.bump();
                let value = self.expr()?;
                let span = self.sp_from(blo);
                let st = if p == "=" && matches!(&body.kind, ExprKind::Ident(n) if n == "_") {
                    Stmt { kind: StmtKind::Discard(value), span }
                } else {
                    Stmt { kind: StmtKind::Assign { place: body, op: p, value }, span }
                };
                body = Expr { kind: ExprKind::Block(Block { stmts: vec![st], span }), span };
            }
        }
        Ok(body)
    }

    fn if_expr(&mut self) -> PResult<Expr> {
        self.nested(|p| p.if_expr_inner())
    }

    fn if_expr_inner(&mut self) -> PResult<Expr> {
        let lo = self.expect_kw(Kw::If)?;
        let save = self.allow_struct;
        self.allow_struct = false;
        let r = if self.eat_kw(Kw::Let) {
            let pat = self.pattern()?;
            // `if let x { }`: unwrap the optional variable `x` in place
            let e = match (&pat.kind, self.is_p("{")) {
                (PatternKind::Ident(n), true) => Expr { kind: ExprKind::Ident(n.clone()), span: pat.span },
                _ => {
                    self.expect_p("=")?;
                    self.expr()?
                }
            };
            self.allow_struct = save;
            let then = self.block()?;
            let else_ = self.else_opt()?;
            ExprKind::IfLet { pat, expr: Box::new(e), then, else_ }
        } else {
            let cond = self.expr()?;
            self.allow_struct = save;
            let then = self.block()?;
            let else_ = self.else_opt()?;
            ExprKind::If { cond: Box::new(cond), then, else_ }
        };
        self.allow_struct = save;
        Ok(Expr { kind: r, span: self.sp_from(lo) })
    }

    fn else_opt(&mut self) -> PResult<Option<Box<Expr>>> {
        // allow `}\nelse {` as well as `} else {`
        if matches!(self.peek(), Tok::Newline) && matches!(self.peek_at(1), Tok::Kw(Kw::Else)) {
            self.bump();
        }
        if !self.eat_kw(Kw::Else) {
            return Ok(None);
        }
        if self.is_kw(Kw::If) {
            return Ok(Some(Box::new(self.if_expr()?)));
        }
        let lo = self.span();
        let b = self.block()?;
        Ok(Some(Box::new(Expr { kind: ExprKind::Block(b), span: self.sp_from(lo) })))
    }

    // ---------- patterns ----------
    fn pattern(&mut self) -> PResult<Pattern> {
        let lo = self.span();
        let first = self.pattern_atom()?;
        if self.is_p("|") {
            let mut alts = vec![first];
            while self.eat_p("|") {
                self.skip_nl();
                alts.push(self.pattern_atom()?);
            }
            return Ok(Pattern { kind: PatternKind::Or(alts), span: self.sp_from(lo) });
        }
        Ok(first)
    }

    fn sub_patterns(&mut self) -> PResult<Option<Vec<Pattern>>> {
        if !self.eat_p("(") {
            return Ok(None);
        }
        let mut ps = vec![];
        while !self.is_p(")") {
            ps.push(self.pattern()?);
            if !self.eat_p(",") {
                break;
            }
        }
        self.expect_p(")")?;
        Ok(Some(ps))
    }

    fn int_pat(&mut self) -> PResult<Option<i128>> {
        let neg = self.is_p("-") && matches!(self.peek_at(1), Tok::Int(_));
        if neg {
            self.bump();
        }
        if let Tok::Int(n) = self.peek().clone() {
            // no integer type holds more than 64 bits; this only keeps the
            // value exact until the checker compares it with the type
            let v = match (neg, i128::try_from(n)) {
                (false, Ok(v)) => v,
                (true, Ok(v)) => -v,
                (true, Err(_)) if n == 1u128 << 127 => i128::MIN,
                _ => return self.err("integer pattern is too large for any integer type"),
            };
            self.bump();
            return Ok(Some(v));
        }
        if let Tok::Char(c) = self.peek().clone() {
            if !neg {
                self.bump();
                return Ok(Some(c as i128));
            }
        }
        Ok(None)
    }

    fn pattern_atom(&mut self) -> PResult<Pattern> {
        self.nested(|p| p.pattern_atom_inner())
    }

    fn pattern_atom_inner(&mut self) -> PResult<Pattern> {
        let lo = self.span();
        let is_char = matches!(self.peek(), Tok::Char(_));
        if let Some(v) = self.int_pat()? {
            if self.is_p("..") || self.is_p("..=") {
                let inclusive = self.is_p("..=");
                self.bump();
                let hi = self.int_pat()?.ok_or_else(|| Diag::new(self.span(), "expected range end"))?;
                return Ok(Pattern { kind: PatternKind::Range(v, hi, inclusive), span: self.sp_from(lo) });
            }
            let kind = if is_char { PatternKind::Char(v as u32) } else { PatternKind::Int(v) };
            return Ok(Pattern { kind, span: self.sp_from(lo) });
        }
        let kind = match self.peek().clone() {
            Tok::P("(") => {
                self.bump();
                let mut ps = vec![];
                while !self.is_p(")") {
                    ps.push(self.pattern()?);
                    if !self.eat_p(",") {
                        break;
                    }
                }
                self.expect_p(")")?;
                if ps.len() < 2 {
                    return Err(Diag::new(self.sp_from(lo), "a tuple pattern has two or more elements: `(a, b)`"));
                }
                PatternKind::Tuple(ps)
            }
            Tok::Str(s) => {
                self.bump();
                PatternKind::Str(s)
            }
            Tok::Kw(Kw::True) => {
                self.bump();
                PatternKind::Bool(true)
            }
            Tok::Kw(Kw::False) => {
                self.bump();
                PatternKind::Bool(false)
            }
            Tok::Kw(Kw::None) => {
                self.bump();
                PatternKind::None
            }
            Tok::P(".") => {
                self.bump();
                let (n, _) = self.ident()?;
                let subs = self.sub_patterns()?;
                PatternKind::DotVariant(n, subs)
            }
            Tok::Ident(s) if s == "_" => {
                self.bump();
                PatternKind::Wild
            }
            Tok::Ident(_) => {
                let (first, _) = self.ident()?;
                // typed pattern `f: File`
                if self.is_p(":") {
                    self.colon("the pattern", "")?;
                    let t = self.ty()?;
                    return Ok(Pattern { kind: PatternKind::Typed(first, t), span: self.sp_from(lo) });
                }
                let mut path = vec![first];
                while self.is_p(".") && matches!(self.peek_at(1), Tok::Ident(_)) {
                    self.bump();
                    path.push(self.ident()?.0);
                }
                let subs = self.sub_patterns()?;
                if path.len() == 1 && subs.is_none() {
                    PatternKind::Ident(path.pop().unwrap())
                } else {
                    PatternKind::Path(path, subs)
                }
            }
            _ => return self.err(format!("expected pattern, found {}", self.describe())),
        };
        Ok(Pattern { kind, span: self.sp_from(lo) })
    }
}
