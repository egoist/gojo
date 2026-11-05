// Recursive-descent parser.

use crate::ast::*;
use crate::diag::{Diag, Span};
use crate::lexer::{Kw, Tok, Token};

pub struct Parser {
    toks: Vec<Token>,
    pos: usize,
    file: u32,
    /// When false, `Name {` is not parsed as a struct literal (if/for/match headers).
    allow_struct: bool,
}

type PResult<T> = Result<T, Diag>;

pub fn parse_file(file: u32, toks: Vec<Token>) -> PResult<File> {
    let mut p = Parser { toks, pos: 0, file, allow_struct: true };
    p.file()
}

impl Parser {
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
            Tok::Str(_) => "string".into(),
            Tok::Char(_) => "rune literal".into(),
            Tok::Kw(k) => format!("keyword `{}`", format!("{:?}", k).to_lowercase()),
            Tok::P(p) => format!("`{}`", p),
            Tok::Newline => "newline".into(),
            Tok::Eof => "end of file".into(),
        }
    }
    fn expect_p(&mut self, p: &str) -> PResult<Span> {
        if self.is_p(p) {
            Ok(self.bump().span)
        } else {
            self.err(format!("expected `{}`, found {}", p, self.describe()))
        }
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
                imports.push(self.use_spec()?);
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

    /// `fmt`, `./geom`, `../shared/util`, `./geom.{Vec2, Shape}`, `./net/http as h`
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
        while self.is_p("/") {
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
            // `extend Type { func ... }`
            self.bump();
            let owner = self.ty()?;
            self.expect_p("{")?;
            self.skip_nl();
            let mut out = vec![];
            while !self.is_p("}") {
                let mlo = self.span();
                let is_pub = self.eat_kw(Kw::Pub);
                if !self.is_kw(Kw::Func) {
                    return self.err(format!("expected `fn` in `extend` block, found {}", self.describe()));
                }
                let f = self.func_decl(Some(&owner))?;
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
        if self.is_kw(Kw::Unsafe) && matches!(self.peek_at(1), Tok::Ident(s) if s == "send") {
            self.bump();
            self.bump();
            unsafe_send = true;
        }
        if self.is_kw(Kw::Copy) {
            self.bump();
            is_copy = true;
        }
        let kind = match self.peek().clone() {
            Tok::Kw(Kw::Func) => ItemKind::Func(self.func_decl(None)?),
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
                ItemKind::Interface(self.iface_body(name, tparams)?)
            }
            Tok::Kw(Kw::Type) => {
                self.bump();
                let (name, _) = self.ident()?;
                let tparams = self.tparams_opt()?;
                let newtype = !self.eat_p("=");
                let ty = self.ty()?;
                ItemKind::TypeAlias { name, tparams, ty, newtype }
            }
            Tok::Kw(Kw::Const) => {
                self.bump();
                let (name, _) = self.ident()?;
                let ty = if self.is_p("=") { None } else { Some(self.ty()?) };
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
                        if let Tok::Str(s) = self.peek().clone() {
                            self.bump();
                            lib = Some(if w == "static" { format!("static:{}", s) } else { s });
                        }
                    }
                }
                if self.is_kw(Kw::Func) {
                    // `extern "C" fn name(...) {...}`: a Kite function callable from C.
                    let mut f = self.func_decl(None)?;
                    f.is_extern_c = true;
                    ItemKind::Func(f)
                } else {
                    self.expect_p("{")?;
                    self.skip_nl();
                    let mut funcs = vec![];
                    while !self.is_p("}") {
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
        let is_func = self.is_kw(Kw::Func) || (self.is_kw(Kw::Pub) && matches!(self.peek_at(1), Tok::Kw(Kw::Func)));
        if !is_func {
            return Ok(false);
        }
        let lo = self.span();
        let is_pub = self.eat_kw(Kw::Pub);
        let f = self.func_decl(Some(owner))?;
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
            let mut names = vec![self.ident()?];
            while self.eat_p(",") {
                names.push(self.ident()?);
            }
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
                    // named `r f64` vs unnamed `os.Error`
                    let named = matches!(self.peek(), Tok::Ident(_))
                        && matches!(self.peek_at(1), Tok::Ident(_) | Tok::P("[") | Tok::P("?") | Tok::P("*") | Tok::Kw(Kw::Func) | Tok::Kw(Kw::Chan));
                    if named {
                        let (n, _) = self.ident()?;
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
                    None => return Err(Diag::new(self.sp_from(lo), "interface methods take `self` as their first parameter: `fn name(self) T`")),
                };
                let ret = self.ret_type()?;
                methods.push(IfaceMethod { name: mname, recv_mode, params, ret, span: self.sp_from(lo) });
            }
            self.end_stmt()?;
        }
        self.bump();
        Ok(IfaceDecl { name, tparams, methods, embeds, type_set })
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
            owner: assoc_owner,
            span: self.sp_from(lo),
        })
    }

    /// Parameters, where the first may be `self [inout|sink]` inside a type body.
    fn params_with_self(&mut self, owner: Option<&TypeExpr>) -> PResult<(Option<Param>, Vec<Param>)> {
        let is_self = self.is_p("(") && matches!(self.peek_at(1), Tok::Ident(s) if s == "self");
        if !is_self {
            let ps = self.params()?;
            return Ok((None, ps));
        }
        let owner = match owner {
            Some(o) => o.clone(),
            None => return Err(Diag::new(self.toks[self.pos + 1].span, "`self` is only allowed in methods (functions declared inside a type or an `extend` block)")),
        };
        self.bump();
        let span = self.bump().span;
        let mode = self.mode();
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
        let mut pending: Vec<(String, Span)> = vec![];
        while !self.is_p(")") {
            let (name, span) = self.ident()?;
            if self.is_p(",") {
                // `x, y int`
                self.bump();
                pending.push((name, span));
                continue;
            }
            let mode = self.mode();
            let ty = self.ty()?;
            for (n, s) in pending.drain(..) {
                out.push(Param { name: n, mode, ty: ty.clone(), span: s });
            }
            out.push(Param { name, mode, ty, span });
            if !self.eat_p(",") {
                break;
            }
        }
        if !pending.is_empty() {
            return Err(Diag::new(pending[0].1, "parameter is missing a type"));
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
        if self.is_p("{") || matches!(self.peek(), Tok::Newline | Tok::Eof) || self.is_kw(Kw::Where) || self.is_p("}") || self.is_p(";") || self.is_p(")") || self.is_p(",") || self.is_p("=>") {
            return Ok(None);
        }
        Ok(Some(self.ty()?))
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
            Tok::Kw(Kw::Func) => {
                self.bump();
                self.expect_p("(")?;
                let mut params = vec![];
                while !self.is_p(")") {
                    // allow optional parameter names: `func(x int)`
                    if matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Kw(Kw::Inout) | Tok::Kw(Kw::Sink) | Tok::P("[") | Tok::P("?") | Tok::Kw(Kw::Func)) {
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
                TypeExprKind::Func { params, ret }
            }
            Tok::Kw(Kw::Chan) => {
                self.bump();
                TypeExprKind::Chan(Box::new(self.ty_atom()?))
            }
            Tok::P("(") => {
                self.bump();
                if self.eat_p(")") {
                    TypeExprKind::Unit
                } else {
                    let t = self.ty()?;
                    self.expect_p(")")?;
                    return Ok(t);
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
        let lo = self.expect_p("{")?;
        let save = self.allow_struct;
        self.allow_struct = true;
        self.skip_nl();
        let mut stmts = vec![];
        while !self.is_p("}") {
            if matches!(self.peek(), Tok::Eof) {
                return self.err("unexpected end of file; missing `}`");
            }
            stmts.push(self.stmt()?);
            self.end_stmt()?;
        }
        self.bump();
        self.allow_struct = save;
        Ok(Block { stmts, span: self.sp_from(lo) })
    }

    fn stmt(&mut self) -> PResult<Stmt> {
        let lo = self.span();
        let kind = match self.peek().clone() {
            Tok::Kw(Kw::Let) | Tok::Kw(Kw::Var) => {
                let mutable = self.is_kw(Kw::Var);
                self.bump();
                let (name, name_span) = self.ident()?;
                let ty = if self.is_p("=") || matches!(self.peek(), Tok::Newline) || self.is_p("}") || self.is_p(";") { None } else { Some(self.ty()?) };
                let init = if self.eat_p("=") { Some(self.expr()?) } else { None };
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
            Tok::Kw(Kw::Break) => {
                self.bump();
                StmtKind::Break
            }
            Tok::Kw(Kw::Continue) => {
                self.bump();
                StmtKind::Continue
            }
            Tok::Kw(Kw::Defer) => {
                self.bump();
                StmtKind::Defer(Box::new(self.stmt()?))
            }
            Tok::Kw(Kw::ErrDefer) => {
                self.bump();
                StmtKind::ErrDefer(Box::new(self.stmt()?))
            }
            Tok::Kw(Kw::Go) => {
                self.bump();
                StmtKind::Go(self.expr()?)
            }
            Tok::Kw(Kw::With) => {
                self.bump();
                let (name, name_span) = self.ident()?;
                self.expect_p("=")?;
                let save = self.allow_struct;
                self.allow_struct = false;
                let expr = self.expr()?;
                self.allow_struct = save;
                let body = self.block()?;
                StmtKind::With { name, name_span, expr, body }
            }
            Tok::Kw(Kw::For) => {
                self.bump();
                StmtKind::For(self.for_loop()?)
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
                if !self.is_p("<-") {
                    return self.err(format!("expected a channel operation (`let x = <-ch`, `<-ch`, `ch <- v`) or `_`, found {}", self.describe()));
                }
                self.bump();
                let value = self.expr()?;
                SelectKind::Send { chan, value }
            };
            self.expect_p("=>")?;
            let body = self.expr_or_assign()?;
            let bspan = body.span;
            let block = match body.kind {
                ExprKind::Block(b) => b,
                _ => Block { stmts: vec![Stmt { kind: StmtKind::Expr(body), span: bspan }], span: bspan },
            };
            arms.push(SelectArm { kind, body: block, span: self.sp_from(lo) });
            self.eat_p(",");
            self.skip_nl();
        }
        self.bump();
        self.allow_struct = save;
        Ok(arms)
    }

    fn for_loop(&mut self) -> PResult<ForLoop> {
        if self.is_p("{") {
            return Ok(ForLoop::Infinite(self.block()?));
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
            let cond = self.expr()?;
            self.allow_struct = save;
            let body = self.block()?;
            ForLoop::Cond(cond, body)
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
        let lo = self.span();
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
            lhs = if p == "??" {
                Expr { kind: ExprKind::Coalesce(Box::new(lhs), Box::new(rhs)), span }
            } else {
                Expr { kind: ExprKind::Binary(p, Box::new(lhs), Box::new(rhs)), span }
            };
        }
        Ok(lhs)
    }

    fn unary(&mut self) -> PResult<Expr> {
        let lo = self.span();
        let op = match self.peek() {
            Tok::P(p) if matches!(*p, "-" | "!" | "~" | "&" | "<-" | "*") => *p,
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
        let lo = self.span();
        let mut e = self.primary()?;
        loop {
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
                self.allow_struct = save;
                self.expect_p(")")?;
                return Ok(Expr { kind: e.kind, span: self.sp_from(lo) });
            }
            Tok::P("[") => {
                let type_follows = matches!(self.peek_at(2), Tok::Ident(_) | Tok::P("?") | Tok::P("[") | Tok::P("*") | Tok::Kw(Kw::Func) | Tok::Kw(Kw::Chan)) && !self.toks[(self.pos + 2).min(self.toks.len() - 1)].nl_before;
                if matches!(self.peek_at(1), Tok::P("]")) && !type_follows {
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
                    while !self.is_p("]") {
                        xs.push(self.expr()?);
                        self.skip_nl();
                        if !self.eat_p(",") {
                            break;
                        }
                        self.skip_nl();
                    }
                    self.allow_struct = save;
                    self.expect_p("]")?;
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
            Tok::Kw(Kw::Chan) => {
                self.bump();
                self.expect_p("[")?;
                let t = self.ty()?;
                self.expect_p("]")?;
                ExprKind::TypeExpr(TypeExpr { kind: TypeExprKind::Chan(Box::new(t)), span: self.sp_from(lo) })
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
            Tok::P(".") => {
                self.bump();
                let (n, _) = self.ident()?;
                ExprKind::DotVariant(n)
            }
            Tok::Kw(Kw::If) => return self.if_expr(),
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
                    let guard = if self.eat_kw(Kw::If) { Some(self.expr()?) } else { None };
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
                // Go-style function literal: func(x int) int { ... }
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
            let mode = self.mode();
            let ty = if self.is_p(",") || self.is_p(")") { None } else { Some(self.ty()?) };
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

    /// An expression, or an assignment wrapped in a block (closure and match-arm bodies).
    fn expr_or_assign(&mut self) -> PResult<Expr> {
        let blo = self.span();
        let mut body = self.expr()?;
        if let Tok::P(p) = self.peek().clone() {
            if matches!(p, "=" | "+=" | "-=" | "*=" | "/=" | "%=" | "+%=" | "-%=" | "*%=" | "&=" | "|=" | "^=" | "<<=" | ">>=") {
                self.bump();
                let value = self.expr()?;
                let span = self.sp_from(blo);
                let st = Stmt { kind: StmtKind::Assign { place: body, op: p, value }, span };
                body = Expr { kind: ExprKind::Block(Block { stmts: vec![st], span }), span };
            }
        }
        Ok(body)
    }

    fn if_expr(&mut self) -> PResult<Expr> {
        let lo = self.expect_kw(Kw::If)?;
        let save = self.allow_struct;
        self.allow_struct = false;
        let r = if self.eat_kw(Kw::Let) {
            let pat = self.pattern()?;
            self.expect_p("=")?;
            let e = self.expr()?;
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
            self.bump();
            let v = n as i128;
            return Ok(Some(if neg { -v } else { v }));
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
                // typed pattern `f File`
                if matches!(self.peek(), Tok::Ident(_)) {
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
