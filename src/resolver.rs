//! Name resolution pass.
//!
//! Two passes over the module:
//! 1. Declare top-level items (so order-independent forward references work).
//! 2. Resolve identifiers within item bodies against the scope chain.
//!
//! Outputs a [`Resolutions`] table:
//! - `path_to_sym` — each path expression's span → the symbol it refers to.
//! - `decl_to_sym` — each declaration ident's span → the symbol it declares.
//!
//! Built-in type names (`i64`, `bool`, ...) are pre-populated as symbols
//! with `SymbolKind::BuiltinType(Ty)`; the type checker reads the embedded
//! `Ty` when it needs to materialize a type from a path.

use std::collections::HashMap;
use std::fmt;

use crate::ast::*;
use crate::token::Span;
use crate::ty::{FloatTy, IntTy, SymbolId, Ty};

#[derive(Debug, Clone)]
pub struct Symbol {
    pub name: String,
    /// Span of the declaration (function name, struct name, pattern ident, ...).
    pub span: Span,
    pub kind: SymbolKind,
}

#[derive(Debug, Clone)]
pub enum SymbolKind {
    BuiltinType(Ty),
    /// Host-provided builtin function with a fixed signature.
    BuiltinFn(BuiltinFn),
    /// Polymorphic builtin — the type checker accepts a set of argument
    /// types and the lowerer dispatches to a concrete `BuiltinCall` based
    /// on what was passed. Used for `print`, which accepts both `i64` and
    /// `str`.
    PolyBuiltinFn(&'static str),
    Fn,
    Local { mutable: bool },
    Param,
    Struct,
    Enum,
    Const,
}

#[derive(Debug, Clone)]
pub struct BuiltinFn {
    pub name: &'static str,
    pub params: Vec<Ty>,
    pub ret: Ty,
}

pub struct Resolutions {
    pub symbols: Vec<Symbol>,
    pub path_to_sym: HashMap<Span, SymbolId>,
    pub decl_to_sym: HashMap<Span, SymbolId>,
}

impl Resolutions {
    pub fn symbol(&self, id: SymbolId) -> &Symbol {
        &self.symbols[id.0 as usize]
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveError {
    pub message: String,
    pub span: Span,
}

impl fmt::Display for ResolveError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "resolve error at {}..{}: {}",
            self.span.start, self.span.end, self.message
        )
    }
}

impl std::error::Error for ResolveError {}

pub struct Resolver {
    symbols: Vec<Symbol>,
    scopes: Vec<HashMap<String, SymbolId>>,
    path_to_sym: HashMap<Span, SymbolId>,
    decl_to_sym: HashMap<Span, SymbolId>,
    errors: Vec<ResolveError>,
}

impl Default for Resolver {
    fn default() -> Self { Self::new() }
}

impl Resolver {
    pub fn new() -> Self {
        let mut r = Self {
            symbols: Vec::new(),
            scopes: vec![HashMap::new()],
            path_to_sym: HashMap::new(),
            decl_to_sym: HashMap::new(),
            errors: Vec::new(),
        };
        r.insert_builtins();
        r
    }

    pub fn resolve_module(mut self, m: &Module) -> (Resolutions, Vec<ResolveError>) {
        for item in &m.items {
            self.declare_item(item);
        }
        for item in &m.items {
            self.resolve_item(item);
        }
        (
            Resolutions {
                symbols: self.symbols,
                path_to_sym: self.path_to_sym,
                decl_to_sym: self.decl_to_sym,
            },
            self.errors,
        )
    }

    fn insert_builtins(&mut self) {
        let zero = Span::new(0, 0);
        let builtins: &[(&str, Ty)] = &[
            ("bool", Ty::Bool),
            ("char", Ty::Char),
            ("str", Ty::Str),
            ("i8", Ty::Int(IntTy::I8)),
            ("i16", Ty::Int(IntTy::I16)),
            ("i32", Ty::Int(IntTy::I32)),
            ("i64", Ty::Int(IntTy::I64)),
            ("isize", Ty::Int(IntTy::ISize)),
            ("u8", Ty::Int(IntTy::U8)),
            ("u16", Ty::Int(IntTy::U16)),
            ("u32", Ty::Int(IntTy::U32)),
            ("u64", Ty::Int(IntTy::U64)),
            ("usize", Ty::Int(IntTy::USize)),
            ("f32", Ty::Float(FloatTy::F32)),
            ("f64", Ty::Float(FloatTy::F64)),
        ];
        for (name, ty) in builtins {
            self.intern(name.to_string(), zero, SymbolKind::BuiltinType(ty.clone()));
        }
        // `print` dispatches by argument type at lowering time.
        self.intern(
            "print".to_string(),
            zero,
            SymbolKind::PolyBuiltinFn("print"),
        );
        // Explicit single-type variants stay available for users who want
        // them, and are the targets of `print`'s dispatch.
        let print_str = BuiltinFn {
            name: "print_str",
            params: vec![Ty::Str],
            ret: Ty::Unit,
        };
        self.intern(
            print_str.name.to_string(),
            zero,
            SymbolKind::BuiltinFn(print_str),
        );
        let print_i64 = BuiltinFn {
            name: "print_i64",
            params: vec![Ty::Int(IntTy::I64)],
            ret: Ty::Unit,
        };
        self.intern(
            print_i64.name.to_string(),
            zero,
            SymbolKind::BuiltinFn(print_i64),
        );
    }

    /// Insert a symbol into the current scope. Shadowing is allowed —
    /// existing entries with the same name in the same scope are overwritten
    /// in the lookup map (the old `Symbol` remains in `symbols` for span-keyed
    /// queries).
    fn intern(&mut self, name: String, span: Span, kind: SymbolKind) -> SymbolId {
        let id = SymbolId(self.symbols.len() as u32);
        self.symbols.push(Symbol { name: name.clone(), span, kind });
        self.scopes.last_mut().unwrap().insert(name, id);
        id
    }

    fn enter_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn exit_scope(&mut self) {
        self.scopes.pop();
    }

    fn lookup(&self, name: &str) -> Option<SymbolId> {
        for scope in self.scopes.iter().rev() {
            if let Some(&id) = scope.get(name) {
                return Some(id);
            }
        }
        None
    }

    fn error(&mut self, msg: impl Into<String>, span: Span) {
        self.errors.push(ResolveError { message: msg.into(), span });
    }

    // ---- pass 1: declare top-level items ----

    fn declare_item(&mut self, item: &Item) {
        let (name, kind) = match item {
            Item::Fn(f) => (&f.name, SymbolKind::Fn),
            Item::Struct(s) => (&s.name, SymbolKind::Struct),
            Item::Enum(e) => (&e.name, SymbolKind::Enum),
            Item::Const(c) => (&c.name, SymbolKind::Const),
        };
        let id = self.intern(name.name.clone(), name.span, kind);
        self.decl_to_sym.insert(name.span, id);
    }

    // ---- pass 2: resolve bodies ----

    fn resolve_item(&mut self, item: &Item) {
        match item {
            Item::Fn(f) => self.resolve_fn(f),
            Item::Struct(s) => self.resolve_struct(s),
            Item::Enum(e) => self.resolve_enum(e),
            Item::Const(c) => self.resolve_const(c),
        }
    }

    fn resolve_fn(&mut self, f: &FnDecl) {
        self.enter_scope();
        for p in &f.params {
            self.resolve_type(&p.ty);
            let id = self.intern(p.name.name.clone(), p.name.span, SymbolKind::Param);
            self.decl_to_sym.insert(p.name.span, id);
        }
        if let Some(rt) = &f.return_type {
            self.resolve_type(rt);
        }
        self.resolve_block(&f.body);
        self.exit_scope();
    }

    fn resolve_struct(&mut self, s: &StructDecl) {
        for f in &s.fields {
            self.resolve_type(&f.ty);
        }
    }

    fn resolve_enum(&mut self, e: &EnumDecl) {
        for v in &e.variants {
            match &v.fields {
                VariantFields::Unit => {}
                VariantFields::Tuple(types) => {
                    for t in types {
                        self.resolve_type(t);
                    }
                }
                VariantFields::Named(fields) => {
                    for f in fields {
                        self.resolve_type(&f.ty);
                    }
                }
            }
        }
    }

    fn resolve_const(&mut self, c: &ConstDecl) {
        self.resolve_type(&c.ty);
        self.resolve_expr(&c.value);
    }

    fn resolve_block(&mut self, b: &Block) {
        self.enter_scope();
        for stmt in &b.stmts {
            self.resolve_stmt(stmt);
        }
        self.exit_scope();
    }

    fn resolve_stmt(&mut self, s: &Stmt) {
        match s {
            Stmt::Let(l) => {
                if let Some(ty) = &l.ty {
                    self.resolve_type(ty);
                }
                if let Some(init) = &l.init {
                    self.resolve_expr(init);
                }
                self.declare_pattern(&l.pat, l.mutable);
            }
            Stmt::Expr(e, _) => self.resolve_expr(e),
            Stmt::Item(item) => {
                self.declare_item(item);
                self.resolve_item(item);
            }
        }
    }

    fn declare_pattern(&mut self, p: &Pattern, mutable_let: bool) {
        match p {
            Pattern::Wildcard(_) => {}
            Pattern::Ident { name, mutable: pat_mut, .. } => {
                let mutable = mutable_let || *pat_mut;
                let id = self.intern(
                    name.name.clone(),
                    name.span,
                    SymbolKind::Local { mutable },
                );
                self.decl_to_sym.insert(name.span, id);
            }
            Pattern::Literal { .. } => {}
        }
    }

    fn resolve_type(&mut self, t: &Type) {
        match t {
            Type::Path(p) => self.resolve_path(p),
        }
    }

    fn resolve_path(&mut self, p: &Path) {
        let first = &p.segments[0];
        if let Some(id) = self.lookup(&first.name) {
            self.path_to_sym.insert(p.span, id);
        } else {
            self.error(format!("unresolved name `{}`", first.name), p.span);
        }
        // Multi-segment paths (`std::io::println`) aren't resolved deeper.
        // The type checker will flag use of an unsupported path shape.
    }

    fn resolve_expr(&mut self, e: &Expr) {
        match e {
            Expr::Lit { .. } => {}
            Expr::Path(p) => self.resolve_path(p),
            Expr::Unary { expr, .. } => self.resolve_expr(expr),
            Expr::Binary { lhs, rhs, .. }
            | Expr::Assign { lhs, rhs, .. }
            | Expr::AssignOp { lhs, rhs, .. } => {
                self.resolve_expr(lhs);
                self.resolve_expr(rhs);
            }
            Expr::Call { callee, args, .. } => {
                self.resolve_expr(callee);
                for a in args {
                    self.resolve_expr(a);
                }
            }
            Expr::MethodCall { receiver, args, .. } => {
                self.resolve_expr(receiver);
                for a in args {
                    self.resolve_expr(a);
                }
            }
            Expr::Field { receiver, .. } => self.resolve_expr(receiver),
            Expr::Index { receiver, index, .. } => {
                self.resolve_expr(receiver);
                self.resolve_expr(index);
            }
            Expr::Try { expr, .. } => self.resolve_expr(expr),
            Expr::Cast { expr, ty, .. } => {
                self.resolve_expr(expr);
                self.resolve_type(ty);
            }
            Expr::Array { elems, .. } => {
                for e in elems {
                    self.resolve_expr(e);
                }
            }
            Expr::Block(b) => self.resolve_block(b),
            Expr::If { cond, then_branch, else_branch, .. } => {
                self.resolve_expr(cond);
                self.resolve_block(then_branch);
                if let Some(e) = else_branch {
                    self.resolve_expr(e);
                }
            }
            Expr::While { cond, body, .. } => {
                self.resolve_expr(cond);
                self.resolve_block(body);
            }
            Expr::For { pat, iter, body, .. } => {
                self.resolve_expr(iter);
                self.enter_scope();
                self.declare_pattern(pat, false);
                for stmt in &body.stmts {
                    self.resolve_stmt(stmt);
                }
                self.exit_scope();
            }
            Expr::Match { scrutinee, arms, .. } => {
                self.resolve_expr(scrutinee);
                for arm in arms {
                    self.enter_scope();
                    self.declare_pattern(&arm.pat, false);
                    if let Some(g) = &arm.guard {
                        self.resolve_expr(g);
                    }
                    self.resolve_expr(&arm.body);
                    self.exit_scope();
                }
            }
            Expr::Range { start, end, .. } => {
                if let Some(s) = start.as_deref() {
                    self.resolve_expr(s);
                }
                if let Some(e) = end.as_deref() {
                    self.resolve_expr(e);
                }
            }
            Expr::Return { value, .. } => {
                if let Some(v) = value {
                    self.resolve_expr(v);
                }
            }
            Expr::Break(_) | Expr::Continue(_) => {}
        }
    }
}
