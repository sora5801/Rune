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
    /// A variant of an enum. Carries its parent enum's symbol and its
    /// numeric discriminant. Unit variants have no payload; tuple
    /// variants carry one or more value types.
    EnumVariant { enum_sym: SymbolId, discriminant: u32 },
    Const,
    /// A generic type parameter declared on an item (`<T>` in
    /// `fn id<T>(x: T) -> T`). The body refers to it via this symbol.
    /// Codegen rejects functions whose body still mentions any
    /// `TypeParam`; that's what makes "generics step 1" parser-only.
    TypeParam,
    /// A trait declaration. Carries no codegen weight — traits are a
    /// compile-time-only construct; method dispatch is resolved
    /// statically via monomorphization.
    Trait,
    /// An inline module (`mod name { ... }`). Compile-time-only — a
    /// module is a namespace, not a runtime value.
    Module,
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
    /// User-defined inherent methods: `(struct_sym, method_name) → method's Fn symbol`.
    /// Populated from `impl` blocks. Builtin methods (`str.len()`, etc.) are
    /// resolved via the checker's hardcoded table instead.
    pub impl_methods: HashMap<(SymbolId, String), SymbolId>,
    /// For each enum symbol, a map from variant name to the variant's
    /// symbol. Variants aren't in the global scope — they're addressed
    /// as `EnumName::VariantName`.
    pub enum_variants: HashMap<SymbolId, HashMap<String, SymbolId>>,
    /// Payload types per variant (variant_sym → AST Types). Empty for
    /// unit variants. For tuple variants the payloads appear in
    /// declaration order; for named-field variants the types appear
    /// in declaration order too, with names tracked separately in
    /// `enum_variant_field_names`.
    pub enum_variant_payloads: HashMap<SymbolId, Vec<crate::ast::Type>>,
    /// Field names per **named** variant, in declaration order.
    /// Tuple and unit variants don't appear in this map. Used to
    /// validate `Variant { name: val }` construction and
    /// destructure patterns.
    pub enum_variant_field_names: HashMap<SymbolId, Vec<String>>,
    /// Generic type-parameter symbols per generic struct, in
    /// declaration order. Lets users of `Ty::Struct(sym, args)`
    /// build a substitution mapping for the struct's fields.
    pub struct_generics: HashMap<SymbolId, Vec<SymbolId>>,
    /// Same for enums.
    pub enum_generics: HashMap<SymbolId, Vec<SymbolId>>,
    /// Declared method signatures per trait — keyed by trait sym.
    /// The checker uses these for impl conformance + bounded-generic
    /// method-call resolution.
    pub trait_methods: HashMap<SymbolId, Vec<crate::ast::TraitMethodSig>>,
    /// Direct supertraits of each trait — the names after `:` in
    /// `trait Sub: A + B { .. }`, resolved to their trait symbols.
    /// Transitive walk is the caller's job; an empty entry (or no
    /// entry) means a free-standing trait.
    pub trait_supertraits: HashMap<SymbolId, Vec<SymbolId>>,
    /// Generic type-parameter symbols per generic trait, in
    /// declaration order. `trait Fn1<A, R>` records `[A_sym,
    /// R_sym]`. The checker substitutes through these at use sites
    /// like `dyn Fn1<i64, str>` and at impl declarations.
    pub trait_generics: HashMap<SymbolId, Vec<SymbolId>>,
    /// For a path `T::Item` where `T` is a TypeParam, the path's
    /// span maps to `T`'s symbol. The checker reads this to build
    /// `Ty::Assoc(TypeVar(T), name)` projections — kept separate
    /// from `path_to_sym` so the "this is a projection, not a bare
    /// TypeParam path" distinction is unambiguous.
    pub assoc_proj_bases: HashMap<Span, SymbolId>,
    /// For each struct, the set of traits explicitly implemented by
    /// an `impl Trait for Struct` block. Strict — inherent methods
    /// shadowing a trait method do *not* count as implementing the
    /// trait. The checker reads this for supertrait conformance.
    pub impls_for: HashMap<SymbolId, std::collections::HashSet<SymbolId>>,
    /// For each closure expression, the synthetic fn `SymbolId` the
    /// resolver minted. The checker uses this to register the
    /// closure's signature; the lowerer uses it as the `HirExprKind::Fn(sym)`
    /// payload at the closure's source position.
    pub closure_fn_sym: HashMap<Span, SymbolId>,
    /// Closures' parameter symbols, in declaration order. The lowerer
    /// needs these to build the synthesized `HirFn`'s params; the
    /// checker reads them to bind types under contextual inference.
    pub closure_params: HashMap<Span, Vec<SymbolId>>,
    /// Captures detected per closure span — Local/Param symbols
    /// referenced inside the body whose declaration lies outside
    /// the closure's source range. The lowerer materializes one
    /// struct field per capture; the checker registers the synth
    /// struct's layout from this list.
    pub closure_captures: HashMap<Span, Vec<SymbolId>>,
    /// Per-closure synthesized struct sym — the type of the
    /// closure value at runtime (when capturing). Pure
    /// fn-pointer-shaped (non-capturing) closures keep
    /// `closure_fn_sym` and synthesize via session 057's path.
    pub closure_struct_sym: HashMap<Span, SymbolId>,
    /// Per-closure synthesized `call` method sym. Inserted into
    /// `impl_methods[(closure_struct_sym, "call")]` so method
    /// lookup resolves uniformly.
    pub closure_call_method_sym: HashMap<Span, SymbolId>,
    /// Associated-type names each trait declares, in source order.
    pub trait_assoc_types: HashMap<SymbolId, Vec<String>>,
    /// `(struct sym, associated-type name) → bound type`, from an
    /// `impl`'s `type Item = Concrete;` binding.
    pub impl_assoc_bindings: HashMap<(SymbolId, String), crate::ast::Type>,
    /// Generic-param symbol → trait-bound symbols. `<T: Display>`
    /// records `T_sym → [Display_sym]`.
    pub generic_bounds: HashMap<SymbolId, Vec<SymbolId>>,
    /// Session 071: `(trait_sym, method_name) → default-fn sym` for
    /// trait methods that declared a default body. The default fn
    /// is a synthesized generic function whose `Self` param is
    /// bounded by the trait — when an impl doesn't override the
    /// method, method dispatch falls through to this fn. The lowerer
    /// emits a HirFn per entry; the monomorphizer specializes per
    /// Self at each call site.
    pub trait_defaults: HashMap<(SymbolId, String), SymbolId>,
    /// Session 072: `into_impls[source_struct_sym]` lists every
    /// `impl Into<Target> for SourceStruct` block as
    /// `(target_ast_type, into_fn_sym)`. The target is kept as an
    /// AST `Type` (not a `Ty`) because the checker resolves AST
    /// types — it walks this list, resolves each target, and
    /// matches against the surrounding fn's err type to pick the
    /// right `into` method when the source struct has more than
    /// one Into impl. Pre-072 the `into` method was looked up via
    /// `impl_methods[(source, "into")]`, which silently
    /// overwrote with the last impl declared. This field fixes
    /// that.
    pub into_impls: HashMap<SymbolId, Vec<(crate::ast::Type, SymbolId)>>,
    /// Session 071: synthesized self-type-param sym for each
    /// default fn. The default fn's `self: Self` resolves through
    /// `generic_bounds[this_sym] = [trait_sym]`, so `self.next()`
    /// inside the default body routes via trait_bound_method_sig
    /// (session 051's machinery).
    pub default_self_syms: HashMap<SymbolId, SymbolId>,
    /// Generic-param symbol + trait-bound symbol → spans of the
    /// trait's generic args at the bound site. `<F: Fn1<I::Item,
    /// U>>` records `(F, Fn1) → [span_of_I::Item, span_of_U]`. The
    /// checker resolves these spans through `type_resolutions` to
    /// recover the Ty values, then uses them to propagate
    /// inference: when F is unified with a concrete `fn(P) -> R`,
    /// the bound says `A = P, R_arg = R` (positional), so any
    /// TypeVars among the bound args get pinned. This is the only
    /// way `let m = Map { iter: ..., f: |x| x * 2 }` can pin Map's
    /// `U` — there's no field that mentions U directly. */
    pub generic_bound_args: HashMap<(SymbolId, SymbolId), Vec<Span>>,
    /// Impl-side generic param sym → struct-side generic param sym
    /// (positional). `impl<I, F, U> Iterator for Map<I, F, U>`
    /// declares its own `[I_impl, F_impl, U_impl]` separate from
    /// the struct's `[I_struct, F_struct, U_struct]` (different
    /// spans). The checker keeps subst keyed by struct-side syms;
    /// `generic_bound_args` entries on impl-side syms (which is
    /// where bounds live) need this map to find the corresponding
    /// struct-side sym, AND its TypeVars in the bound's arg types
    /// need translation. Positional mapping suffices for v0.x
    /// (impls always have impl's generics positionally aligned with
    /// the for-type's args).
    pub impl_to_struct_generic: HashMap<SymbolId, SymbolId>,
    /// Enums that have at least one payload-bearing variant. These use
    /// a heap-allocated `{ tag, payload, rc }` descriptor at runtime
    /// instead of the plain i64 discriminant used by tag-only enums.
    pub enum_has_payload: std::collections::HashSet<SymbolId>,
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
    impl_methods: HashMap<(SymbolId, String), SymbolId>,
    enum_variants: HashMap<SymbolId, HashMap<String, SymbolId>>,
    enum_variant_payloads: HashMap<SymbolId, Vec<crate::ast::Type>>,
    enum_variant_field_names: HashMap<SymbolId, Vec<String>>,
    enum_has_payload: std::collections::HashSet<SymbolId>,
    struct_generics: HashMap<SymbolId, Vec<SymbolId>>,
    enum_generics: HashMap<SymbolId, Vec<SymbolId>>,
    trait_methods: HashMap<SymbolId, Vec<crate::ast::TraitMethodSig>>,
    trait_supertraits: HashMap<SymbolId, Vec<SymbolId>>,
    trait_generics: HashMap<SymbolId, Vec<SymbolId>>,
    impls_for: HashMap<SymbolId, std::collections::HashSet<SymbolId>>,
    closure_fn_sym: HashMap<Span, SymbolId>,
    closure_params: HashMap<Span, Vec<SymbolId>>,
    closure_captures: HashMap<Span, Vec<SymbolId>>,
    closure_struct_sym: HashMap<Span, SymbolId>,
    closure_call_method_sym: HashMap<Span, SymbolId>,
    /// Stack of currently-open closure-body spans, outermost first.
    /// Inside a closure body, a Local/Param path that resolves to a
    /// sym whose declaration span lies *outside* the innermost
    /// stack entry is a capture — rejected in v0.x. The stack
    /// supports nested closures (the outer one still rejects
    /// captures from its caller's frame).
    open_closure_spans: Vec<Span>,
    /// Per-module counter for mangling synthetic lambda fn names
    /// (`__lambda_0`, `__lambda_1`, ...). Reset to 0 nowhere — the
    /// counter is global to the compilation; collisions across
    /// modules avoided by module-path prefix mangling.
    lambda_counter: u32,
    assoc_proj_bases: HashMap<Span, SymbolId>,
    trait_assoc_types: HashMap<SymbolId, Vec<String>>,
    impl_assoc_bindings: HashMap<(SymbolId, String), crate::ast::Type>,
    generic_bounds: HashMap<SymbolId, Vec<SymbolId>>,
    generic_bound_args: HashMap<(SymbolId, SymbolId), Vec<Span>>,
    trait_defaults: HashMap<(SymbolId, String), SymbolId>,
    default_self_syms: HashMap<SymbolId, SymbolId>,
    into_impls: HashMap<SymbolId, Vec<(crate::ast::Type, SymbolId)>>,
    impl_to_struct_generic: HashMap<SymbolId, SymbolId>,
    /// Names of the modules currently being declared / resolved,
    /// outermost first. Empty means the root module. Item lookups
    /// qualify against this path; functions mangle with it.
    current_path: Vec<String>,
    /// Per-item visibility: symbol → (declaring module path, is_pub).
    /// A non-`pub` item is visible only from its own module and
    /// descendants. Symbols absent here (builtins, locals, type
    /// params, enum variants resolved off-table) are always visible.
    item_vis: HashMap<SymbolId, (Vec<String>, bool)>,
    /// Global-namespace keys created by a `pub use` — these alias
    /// keys are publicly visible re-exports, so a path resolving to
    /// one (or through one) skips the privacy check.
    pub_reexport_keys: std::collections::HashSet<String>,
    errors: Vec<ResolveError>,
}

impl Default for Resolver {
    fn default() -> Self { Self::new() }
}

/// Whether a top-level item is declared `pub`. Impl blocks and `use`
/// statements carry no visibility of their own.
fn item_is_pub(item: &Item) -> bool {
    let vis = match item {
        Item::Fn(f) => &f.vis,
        Item::Struct(s) => &s.vis,
        Item::Enum(e) => &e.vis,
        Item::Const(c) => &c.vis,
        Item::Trait(t) => &t.vis,
        Item::Mod(m) => &m.vis,
        Item::Impl(_) | Item::Use(_) => return false,
    };
    matches!(vis, Visibility::Pub)
}

impl Resolver {
    pub fn new() -> Self {
        let mut r = Self {
            symbols: Vec::new(),
            scopes: vec![HashMap::new()],
            path_to_sym: HashMap::new(),
            decl_to_sym: HashMap::new(),
            impl_methods: HashMap::new(),
            enum_variants: HashMap::new(),
            enum_variant_payloads: HashMap::new(),
            enum_variant_field_names: HashMap::new(),
            enum_has_payload: std::collections::HashSet::new(),
            struct_generics: HashMap::new(),
            enum_generics: HashMap::new(),
            trait_methods: HashMap::new(),
            trait_supertraits: HashMap::new(),
            trait_generics: HashMap::new(),
            impls_for: HashMap::new(),
            closure_fn_sym: HashMap::new(),
            closure_params: HashMap::new(),
            closure_captures: HashMap::new(),
            closure_struct_sym: HashMap::new(),
            closure_call_method_sym: HashMap::new(),
            open_closure_spans: Vec::new(),
            lambda_counter: 0,
            assoc_proj_bases: HashMap::new(),
            trait_assoc_types: HashMap::new(),
            impl_assoc_bindings: HashMap::new(),
            generic_bounds: HashMap::new(),
            generic_bound_args: HashMap::new(),
            trait_defaults: HashMap::new(),
            default_self_syms: HashMap::new(),
            into_impls: HashMap::new(),
            impl_to_struct_generic: HashMap::new(),
            current_path: Vec::new(),
            item_vis: HashMap::new(),
            pub_reexport_keys: std::collections::HashSet::new(),
            errors: Vec::new(),
        };
        r.insert_builtins();
        r
    }

    pub fn resolve_module(mut self, m: &Module) -> (Resolutions, Vec<ResolveError>) {
        // Pass 1: declare non-impl items (recursing into modules) so
        // impl blocks and `use` can resolve their targets.
        self.declare_items(&m.items);
        // Pass 1.5: declare impl methods.
        self.declare_impls(&m.items);
        // Pass 1.7: resolve `use` aliases now that every item exists.
        self.resolve_uses(&m.items);
        // Pass 2: resolve all bodies.
        self.resolve_items(&m.items);
        // After every trait's supertrait list is resolved, surface
        // cycles as a clear diagnostic.
        self.validate_supertrait_cycles();
        (
            Resolutions {
                symbols: self.symbols,
                path_to_sym: self.path_to_sym,
                decl_to_sym: self.decl_to_sym,
                impl_methods: self.impl_methods,
                enum_variants: self.enum_variants,
                enum_variant_payloads: self.enum_variant_payloads,
                enum_variant_field_names: self.enum_variant_field_names,
                enum_has_payload: self.enum_has_payload,
                struct_generics: self.struct_generics,
                enum_generics: self.enum_generics,
                trait_methods: self.trait_methods,
                trait_supertraits: self.trait_supertraits,
                trait_generics: self.trait_generics,
                impls_for: self.impls_for,
                closure_fn_sym: self.closure_fn_sym,
                closure_params: self.closure_params,
                closure_captures: self.closure_captures,
                closure_struct_sym: self.closure_struct_sym,
                closure_call_method_sym: self.closure_call_method_sym,
                assoc_proj_bases: self.assoc_proj_bases,
                trait_assoc_types: self.trait_assoc_types,
                impl_assoc_bindings: self.impl_assoc_bindings,
                generic_bounds: self.generic_bounds,
                generic_bound_args: self.generic_bound_args,
                trait_defaults: self.trait_defaults,
                default_self_syms: self.default_self_syms,
                into_impls: self.into_impls,
                impl_to_struct_generic: self.impl_to_struct_generic,
            },
            self.errors,
        )
    }

    /// Pass 1.5 — declare impl methods, recursing into modules so an
    /// `impl` block inside `mod m` resolves its target type relative
    /// to `m`.
    fn declare_impls(&mut self, items: &[Item]) {
        for item in items {
            match item {
                Item::Impl(i) => self.declare_impl(i),
                Item::Mod(md) => {
                    self.current_path.push(md.name.name.clone());
                    self.declare_impls(&md.items);
                    self.current_path.pop();
                }
                _ => {}
            }
        }
    }

    /// Pass 1.7 — resolve `use a::b::c;` declarations into aliases in
    /// the using module's namespace.
    fn resolve_uses(&mut self, items: &[Item]) {
        for item in items {
            match item {
                Item::Use(u) => {
                    if u.glob {
                        self.resolve_use_glob(u);
                    } else if let Some((id, path_key)) =
                        self.lookup_path(&u.path.segments)
                    {
                        // You can only `use` what you can see — the
                        // whole path, intermediate modules included.
                        self.check_path_visibility(&path_key, u.path.span);
                        // `use x as y;` binds under `y`; a plain `use`
                        // binds under the path's final segment.
                        let alias = u
                            .alias
                            .as_ref()
                            .map(|a| a.name.clone())
                            .unwrap_or_else(|| {
                                u.path
                                    .segments
                                    .last()
                                    .map(|s| s.name.clone())
                                    .unwrap_or_default()
                            });
                        let alias_key = if self.current_path.is_empty() {
                            alias
                        } else {
                            format!("{}::{}", self.current_path.join("::"), alias)
                        };
                        // `pub use` — the alias is a public re-export.
                        if matches!(u.vis, Visibility::Pub) {
                            self.pub_reexport_keys.insert(alias_key.clone());
                        }
                        self.scopes[0].insert(alias_key, id);
                        self.path_to_sym.insert(u.path.span, id);
                    } else {
                        let shown: Vec<&str> = u
                            .path
                            .segments
                            .iter()
                            .map(|s| s.name.as_str())
                            .collect();
                        self.error(
                            format!("unresolved import `{}`", shown.join("::")),
                            u.path.span,
                        );
                    }
                }
                Item::Mod(md) => {
                    self.current_path.push(md.name.name.clone());
                    self.resolve_uses(&md.items);
                    self.current_path.pop();
                }
                _ => {}
            }
        }
    }

    /// Resolve `use m::*;` — alias every direct item of module `m`
    /// into the using module's namespace. Existing entries (local
    /// items, explicit `use`s) are not overwritten, and items not
    /// visible from here are skipped.
    fn resolve_use_glob(&mut self, u: &UseDecl) {
        let joined = u
            .path
            .segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("::");
        // Find the module's qualified key — absolute, then relative
        // to each enclosing module prefix.
        let mut candidates = vec![joined.clone()];
        for depth in (1..=self.current_path.len()).rev() {
            candidates.push(format!(
                "{}::{}",
                self.current_path[..depth].join("::"),
                joined
            ));
        }
        let mut module_key: Option<String> = None;
        for cand in candidates {
            if let Some(&id) = self.scopes[0].get(&cand) {
                if matches!(self.symbols[id.0 as usize].kind, SymbolKind::Module) {
                    module_key = Some(cand);
                    break;
                }
            }
        }
        let Some(module_key) = module_key else {
            self.error(
                format!("unresolved glob import `{}::*` — no such module", joined),
                u.path.span,
            );
            return;
        };
        // Direct children: keys `module_key::<name>` with no `::` left.
        let prefix = format!("{}::", module_key);
        let children: Vec<(String, SymbolId)> = self.scopes[0]
            .iter()
            .filter_map(|(k, &id)| {
                let rest = k.strip_prefix(&prefix)?;
                if rest.contains("::") {
                    None // a grandchild, not a direct item
                } else {
                    Some((rest.to_string(), id))
                }
            })
            .collect();
        for (name, id) in children {
            if !self.is_visible(id) {
                continue;
            }
            let key = if self.current_path.is_empty() {
                name
            } else {
                format!("{}::{}", self.current_path.join("::"), name)
            };
            // `pub use m::*;` re-exports each imported item publicly.
            if matches!(u.vis, Visibility::Pub) {
                self.pub_reexport_keys.insert(key.clone());
            }
            // An explicit `use` or a local item of the same name wins.
            self.scopes[0].entry(key).or_insert(id);
        }
    }

    fn declare_impl(&mut self, i: &ImplBlock) {
        // The impl's `<T>` must be in scope while resolving the type
        // path — `resolve_path` recurses into generic args, so
        // `Foo<T>` would otherwise report `T` unresolved. Use
        // `decl_to_sym`-keyed interning so every later read of this
        // impl's `T` agrees on a single `SymbolId`: the second scope
        // entry below (for assoc-type bindings), `resolve_fn`'s
        // merged-method-generics walk, and the checker's
        // pre-resolution of `type Item = T;`. Without this, the
        // impl's `T` is interned afresh in each scope and the
        // resulting four-way mismatch (struct-T, impl-T-aux1,
        // impl-T-aux2, method-T) breaks every substitution.
        self.enter_scope();
        for g in &i.generics {
            self.intern_generic_param(g);
        }
        self.resolve_path(&i.type_path);
        self.exit_scope();
        let Some(&struct_sym) = self.path_to_sym.get(&i.type_path.span) else {
            return;
        };
        if !matches!(self.symbols[struct_sym.0 as usize].kind, SymbolKind::Struct) {
            let name = self.symbols[struct_sym.0 as usize].name.clone();
            self.error(
                format!(
                    "`{}` is not a struct; `impl` can only be applied to structs (for now)",
                    name
                ),
                i.type_path.span,
            );
            return;
        }
        // For a trait impl, resolve the trait path so the checker can
        // validate the impl against the trait's declared signatures.
        if let Some(trait_path) = &i.trait_path {
            self.resolve_path(trait_path);
            if let Some(&tsym) = self.path_to_sym.get(&trait_path.span) {
                if !matches!(self.symbols[tsym.0 as usize].kind, SymbolKind::Trait) {
                    self.error(
                        format!(
                            "`{}` is not a trait",
                            self.symbols[tsym.0 as usize].name
                        ),
                        trait_path.span,
                    );
                } else {
                    // Record the explicit `impl Trait for Struct`
                    // for the supertrait conformance check.
                    self.impls_for.entry(struct_sym).or_default().insert(tsym);
                }
            }
        }
        let struct_name = self.symbols[struct_sym.0 as usize].name.clone();
        // Module-prefix the impl method's codegen name so two structs
        // named the same in different modules don't collide.
        let mod_prefix = if self.current_path.is_empty() {
            String::new()
        } else {
            format!("{}__", self.current_path.join("__"))
        };
        // Session 072: when this impl is for `Into<T>`, the same
        // struct may have multiple Into impls (one per target T).
        // Tolerate the duplicate-method-name in impl_methods for
        // that case — into_impls (populated below) is the
        // authoritative per-target lookup, and check_try reads it
        // by target instead of relying on impl_methods.
        let is_into_impl = i
            .trait_path
            .as_ref()
            .and_then(|tp| self.path_to_sym.get(&tp.span))
            .map(|&ts| self.symbols[ts.0 as usize].name == "Into")
            .unwrap_or(false);
        for method in &i.methods {
            // Session 072: Into impls disambiguate by appending
            // the impl's span-start so two `impl Into<X> for S`
            // blocks don't collide at the Cranelift symbol level.
            // Non-Into impls keep the clean mangling for codegen
            // readability.
            let mangled = if is_into_impl {
                format!(
                    "{}{}__{}__{}",
                    mod_prefix, struct_name, method.name.name, i.span.start
                )
            } else {
                format!("{}{}__{}", mod_prefix, struct_name, method.name.name)
            };
            let id = SymbolId(self.symbols.len() as u32);
            self.symbols.push(Symbol {
                name: mangled,
                span: method.name.span,
                kind: SymbolKind::Fn,
            });
            self.decl_to_sym.insert(method.name.span, id);
            let key = (struct_sym, method.name.name.clone());
            if self.impl_methods.contains_key(&key) && !is_into_impl {
                self.error(
                    format!(
                        "method `{}` already defined on `{}`",
                        method.name.name, struct_name
                    ),
                    method.name.span,
                );
            }
            self.impl_methods.insert(key, id);
        }
        // Session 072: record per-impl Into target so the checker
        // can disambiguate multiple Into impls on the same source
        // struct. The trait-path's first generic arg is the
        // target type; the method's sym is impl_methods[(struct,
        // "into")] (set just above in the methods loop, possibly
        // overwritten by a later impl — but here we keep ALL
        // entries via the into_impls vec).
        if let Some(trait_path) = &i.trait_path {
            if let Some(&trait_sym) = self.path_to_sym.get(&trait_path.span) {
                let is_into = self.symbols[trait_sym.0 as usize].name == "Into";
                if is_into {
                    if let Some(target_ast) = trait_path.generic_args.first().cloned() {
                        if let Some(into_method) = i
                            .methods
                            .iter()
                            .find(|m| m.name.name == "into")
                        {
                            if let Some(&fn_sym) =
                                self.decl_to_sym.get(&into_method.name.span)
                            {
                                self.into_impls
                                    .entry(struct_sym)
                                    .or_default()
                                    .push((target_ast, fn_sym));
                            }
                        }
                    }
                }
            }
        }
        // Session 071: for `impl Trait for Struct` blocks, point
        // any not-overridden trait-default method at the synth
        // default-fn sym. Method dispatch (resolve_method_calls in
        // monomorphize) reads impl_methods uniformly — the default
        // looks like a regular impl method, and the monomorphizer
        // specializes per Self at the call site. Requires the
        // trait's defaults to have been registered in pass 1 first
        // — works because declare_items runs before declare_impls
        // (traits get their default-fn syms minted before any
        // impl tries to look them up).
        if let Some(trait_path) = &i.trait_path {
            if let Some(&trait_sym) = self.path_to_sym.get(&trait_path.span) {
                let method_names: Vec<String> = i
                    .methods
                    .iter()
                    .map(|m| m.name.name.clone())
                    .collect();
                let defaults: Vec<(String, SymbolId)> = self
                    .trait_defaults
                    .iter()
                    .filter_map(|((ts, mname), &fn_sym)| {
                        if *ts == trait_sym && !method_names.contains(mname) {
                            Some((mname.clone(), fn_sym))
                        } else {
                            None
                        }
                    })
                    .collect();
                for (mname, fn_sym) in defaults {
                    let key = (struct_sym, mname);
                    self.impl_methods.entry(key).or_insert(fn_sym);
                }
            }
        }
        // Record this impl's associated-type bindings. The generic
        // scope is re-entered so a `type Item = T` form resolves.
        if !i.assoc_types.is_empty() {
            self.enter_scope();
            for g in &i.generics {
                self.intern_generic_param(g);
            }
            for binding in &i.assoc_types {
                self.resolve_type(&binding.value);
                self.impl_assoc_bindings.insert(
                    (struct_sym, binding.name.name.clone()),
                    binding.value.clone(),
                );
            }
            self.exit_scope();
        }
    }

    fn insert_builtins(&mut self) {
        let zero = Span::new(0, 0);
        // Builtin sentinel for `Weak<T>` — the checker special-cases
        // this name and reads the path's generic args to build
        // `Ty::Weak(args[0])`. v0.x: only Weak<Vec> has runtime
        // support; the checker rejects other inner types.
        self.intern(
            "Weak".to_string(),
            zero,
            SymbolKind::BuiltinType(Ty::Weak(Box::new(Ty::Error))),
        );
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
            ("Vec", Ty::Vec(Box::new(Ty::Error))),
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
        // Weak<T> primitives. Polymorphic over T; the lowerer
        // dispatches to the per-type runtime helper. v0.x supports
        // only Vec as the inner type.
        self.intern(
            "weak".to_string(),
            zero,
            SymbolKind::PolyBuiltinFn("weak"),
        );
        self.intern(
            "upgrade_or".to_string(),
            zero,
            SymbolKind::PolyBuiltinFn("upgrade_or"),
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
        self.intern(
            "vec_new".to_string(),
            zero,
            SymbolKind::BuiltinFn(BuiltinFn {
                name: "vec_new",
                params: vec![],
                ret: Ty::Vec(Box::new(Ty::Int(IntTy::I64))),
            }),
        );
        // Also expose Vec under the `std` namespace — `std::Vec<T>`
        // and `std::vec_new()`. The bare `Vec` / `vec_new` stay
        // available (the existing test corpus uses the bare forms).
        self.intern(
            "std::Vec".to_string(),
            zero,
            SymbolKind::BuiltinType(Ty::Vec(Box::new(Ty::Error))),
        );
        self.intern(
            "std::vec_new".to_string(),
            zero,
            SymbolKind::BuiltinFn(BuiltinFn {
                name: "vec_new",
                params: vec![],
                ret: Ty::Vec(Box::new(Ty::Int(IntTy::I64))),
            }),
        );
        // HashMap parallel to Vec — a builtin parametric type and a
        // poly builtin constructor. Both bare names (`HashMap` /
        // `hashmap_new`) and `std::`-prefixed forms work. The
        // checker resolves `HashMap<K, V>` paths into
        // `Ty::HashMap(K, V)`; `hashmap_new()` is a polymorphic
        // builtin call whose return type is inferred from the
        // surrounding annotation (`let m: HashMap<i64, str> =
        // hashmap_new();`).
        self.intern(
            "HashMap".to_string(),
            zero,
            SymbolKind::BuiltinType(Ty::HashMap(
                Box::new(Ty::Error),
                Box::new(Ty::Error),
            )),
        );
        self.intern(
            "hashmap_new".to_string(),
            zero,
            SymbolKind::PolyBuiltinFn("hashmap_new"),
        );
        self.intern(
            "hashmap_str_new".to_string(),
            zero,
            SymbolKind::PolyBuiltinFn("hashmap_str_new"),
        );
        // Low-level inspection builtins used by std::HashMapKeysIter
        // — the user shouldn't call these directly, but they're
        // resolved like any other polybuiltin so std.rn can wire
        // them into the iterator's `next` body.
        self.intern(
            "hashmap_cap".to_string(),
            zero,
            SymbolKind::PolyBuiltinFn("hashmap_cap"),
        );
        self.intern(
            "hashmap_is_live_at".to_string(),
            zero,
            SymbolKind::PolyBuiltinFn("hashmap_is_live_at"),
        );
        self.intern(
            "hashmap_key_at".to_string(),
            zero,
            SymbolKind::PolyBuiltinFn("hashmap_key_at"),
        );
        self.intern(
            "std::HashMap".to_string(),
            zero,
            SymbolKind::BuiltinType(Ty::HashMap(
                Box::new(Ty::Error),
                Box::new(Ty::Error),
            )),
        );
        self.intern(
            "std::hashmap_new".to_string(),
            zero,
            SymbolKind::PolyBuiltinFn("hashmap_new"),
        );
        self.intern(
            "std::hashmap_str_new".to_string(),
            zero,
            SymbolKind::PolyBuiltinFn("hashmap_str_new"),
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

    /// Intern a generic parameter, reusing a prior sym whose span
    /// matches. Used by `declare_impl` (twice, once per scope) and
    /// `resolve_fn` so every reference to an impl's `T` agrees on a
    /// single `SymbolId`. The first call mints + records in
    /// `decl_to_sym`; subsequent calls hit the cached sym and only
    /// re-add it to the current scope. Mirrors `resolve_fn`'s
    /// existing pattern (session 048).
    /// If we're inside a closure body (`open_closure_spans`
    /// non-empty), reject any `Local`/`Param` resolution whose
    /// declaration span lies outside the innermost closure's span.
    /// v0.x closures don't capture; the diagnostic is the
    /// definitive signal the user can fix.
    fn check_closure_capture(&mut self, resolved: SymbolId, _use_span: Span) {
        let Some(&closure_span) = self.open_closure_spans.last() else {
            return;
        };
        let sym = &self.symbols[resolved.0 as usize];
        if !matches!(sym.kind, SymbolKind::Local { .. } | SymbolKind::Param) {
            return;
        }
        if sym.span.start >= closure_span.start && sym.span.end <= closure_span.end {
            return;
        }
        // Capture detected. Record it (deduped) on the closure's
        // span list. The lowerer + checker materialize a synth
        // struct field per capture; codegen treats the closure as
        // a struct value with one field per captured binding.
        let entry = self.closure_captures.entry(closure_span).or_default();
        if !entry.contains(&resolved) {
            entry.push(resolved);
        }
    }

    fn intern_generic_param(&mut self, g: &crate::ast::GenericParam) -> SymbolId {
        if let Some(&existing) = self.decl_to_sym.get(&g.name.span) {
            self.scopes
                .last_mut()
                .unwrap()
                .insert(g.name.name.clone(), existing);
            existing
        } else {
            let id =
                self.intern(g.name.name.clone(), g.name.span, SymbolKind::TypeParam);
            self.decl_to_sym.insert(g.name.span, id);
            id
        }
    }

    fn enter_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn exit_scope(&mut self) {
        self.scopes.pop();
    }

    fn lookup(&self, name: &str) -> Option<SymbolId> {
        // Lexical scopes (locals) — skip scopes[0], which is the
        // global namespace handled by the qualified lookups below.
        for scope in self.scopes.iter().skip(1).rev() {
            if let Some(&id) = scope.get(name) {
                return Some(id);
            }
        }
        // Qualified lookup against the current module path, longest
        // prefix first: inside `mod a { mod b { ... } }`, a bare
        // `name` tries `a::b::name`, then `a::name`, then `name`.
        for depth in (0..=self.current_path.len()).rev() {
            let key = if depth == 0 {
                name.to_string()
            } else {
                format!("{}::{}", self.current_path[..depth].join("::"), name)
            };
            if let Some(&id) = self.scopes[0].get(&key) {
                return Some(id);
            }
        }
        None
    }

    /// Resolve a multi-segment path (`a::b::c`) to a symbol *and the
    /// global-namespace key it matched*. Tries the path absolutely
    /// (from root) and relative to the current module. The key lets
    /// callers walk the path's module prefixes for privacy checks.
    fn lookup_path(&self, segments: &[Ident]) -> Option<(SymbolId, String)> {
        if segments.len() == 1 {
            // A bare name's key isn't a qualified path; callers only
            // use the key for multi-segment prefix walks.
            return self
                .lookup(&segments[0].name)
                .map(|id| (id, segments[0].name.clone()));
        }
        let joined: Vec<&str> =
            segments.iter().map(|s| s.name.as_str()).collect();
        let tail = joined.join("::");
        // Absolute.
        if let Some(&id) = self.scopes[0].get(&tail) {
            return Some((id, tail));
        }
        // Relative to each enclosing module path prefix.
        for depth in (0..=self.current_path.len()).rev() {
            if depth == 0 {
                continue; // depth 0 == absolute, already tried
            }
            let key =
                format!("{}::{}", self.current_path[..depth].join("::"), tail);
            if let Some(&id) = self.scopes[0].get(&key) {
                return Some((id, key));
            }
        }
        None
    }

    /// Whether `sym` is visible from the current module. A non-`pub`
    /// item is reachable only from its declaring module and that
    /// module's descendants. Symbols with no recorded visibility
    /// (builtins, locals, type params) are always visible.
    fn is_visible(&self, sym: SymbolId) -> bool {
        match self.item_vis.get(&sym) {
            None => true,
            Some((decl_mod, is_pub)) => {
                *is_pub || self.current_path.starts_with(decl_mod)
            }
        }
    }

    /// Check that every module prefix of a resolved path key — and
    /// the item itself — is visible from the current module. A `pub
    /// use` re-export key short-circuits the check (the path, or a
    /// prefix of it, is a public re-export).
    fn check_path_visibility(&mut self, resolved_key: &str, span: Span) {
        if self.pub_reexport_keys.contains(resolved_key) {
            return;
        }
        let parts: Vec<&str> = resolved_key.split("::").collect();
        for len in 1..=parts.len() {
            let prefix = parts[..len].join("::");
            if self.pub_reexport_keys.contains(&prefix) {
                continue;
            }
            if let Some(&pid) = self.scopes[0].get(&prefix) {
                if !self.is_visible(pid) {
                    self.visibility_error(parts[len - 1], pid, span);
                    return;
                }
            }
        }
    }

    /// Record a "private item" resolve error against `span`.
    fn visibility_error(&mut self, display: &str, sym: SymbolId, span: Span) {
        let where_ = match self.item_vis.get(&sym) {
            Some((m, _)) if m.is_empty() => "the crate root".to_string(),
            Some((m, _)) => format!("module `{}`", m.join("::")),
            None => "another module".to_string(),
        };
        self.error(format!("`{}` is private to {}", display, where_), span);
    }

    /// Build the codegen-visible (mangled) name for a function
    /// declared in the current module. Root functions keep their
    /// bare name so `main` stays `main`.
    fn mangled_fn_name(&self, bare: &str) -> String {
        if self.current_path.is_empty() {
            bare.to_string()
        } else {
            format!("{}__{}", self.current_path.join("__"), bare)
        }
    }

    /// Insert `bare` into the global namespace under its module-
    /// qualified key. Returns the fresh SymbolId.
    fn intern_item(
        &mut self,
        bare: &str,
        span: Span,
        kind: SymbolKind,
        codegen_name: String,
    ) -> SymbolId {
        let id = SymbolId(self.symbols.len() as u32);
        self.symbols.push(Symbol {
            name: codegen_name,
            span,
            kind,
        });
        let key = if self.current_path.is_empty() {
            bare.to_string()
        } else {
            format!("{}::{}", self.current_path.join("::"), bare)
        };
        self.scopes[0].insert(key, id);
        id
    }

    fn error(&mut self, msg: impl Into<String>, span: Span) {
        self.errors.push(ResolveError { message: msg.into(), span });
    }

    // ---- pass 1: declare items (recursing into modules) ----

    fn declare_items(&mut self, items: &[Item]) {
        for item in items {
            self.declare_item(item);
        }
    }

    fn declare_item(&mut self, item: &Item) {
        // Modules: intern the module name, then recurse with the
        // path extended.
        if let Item::Mod(md) = item {
            let id = self.intern_item(
                &md.name.name,
                md.name.span,
                SymbolKind::Module,
                md.name.name.clone(),
            );
            self.item_vis.insert(
                id,
                (self.current_path.clone(), matches!(md.vis, Visibility::Pub)),
            );
            self.decl_to_sym.insert(md.name.span, id);
            self.current_path.push(md.name.name.clone());
            self.declare_items(&md.items);
            self.current_path.pop();
            return;
        }
        let (name, kind) = match item {
            Item::Fn(f) => (&f.name, SymbolKind::Fn),
            Item::Struct(s) => (&s.name, SymbolKind::Struct),
            Item::Enum(e) => (&e.name, SymbolKind::Enum),
            Item::Const(c) => (&c.name, SymbolKind::Const),
            Item::Trait(t) => (&t.name, SymbolKind::Trait),
            // Impl blocks contribute methods, not a top-level name.
            Item::Impl(_) => return,
            // `use` is resolved in pass 1.7 after all items exist.
            Item::Use(_) => return,
            Item::Mod(_) => unreachable!("handled above"),
        };
        // Functions get a mangled codegen name; everything else keeps
        // its bare name (only functions become Cranelift symbols).
        let codegen_name = if matches!(kind, SymbolKind::Fn) {
            self.mangled_fn_name(&name.name)
        } else {
            name.name.clone()
        };
        let id = self.intern_item(&name.name, name.span, kind, codegen_name);
        self.item_vis
            .insert(id, (self.current_path.clone(), item_is_pub(item)));
        self.decl_to_sym.insert(name.span, id);

        // For enums, also register each variant by name (off-scope —
        // only addressable as EnumName::VariantName).
        if let Item::Enum(e) = item {
            let mut variants: HashMap<String, SymbolId> = HashMap::new();
            let mut any_payload = false;
            // Variants inherit the enum's visibility.
            let enum_is_pub = matches!(e.vis, Visibility::Pub);
            for (discriminant, v) in e.variants.iter().enumerate() {
                // Variant symbols sit outside any lexical scope. They get
                // a fresh entry in `symbols` for span-keyed queries; lookups
                // go through `enum_variants` instead of `scopes`.
                let variant_id = SymbolId(self.symbols.len() as u32);
                self.symbols.push(Symbol {
                    name: v.name.name.clone(),
                    span: v.name.span,
                    kind: SymbolKind::EnumVariant {
                        enum_sym: id,
                        discriminant: discriminant as u32,
                    },
                });
                self.decl_to_sym.insert(v.name.span, variant_id);
                self.item_vis.insert(
                    variant_id,
                    (self.current_path.clone(), enum_is_pub),
                );
                variants.insert(v.name.name.clone(), variant_id);
                // Capture payload types per variant for the checker /
                // lowerer / codegen to look up.
                let payload_tys: Vec<crate::ast::Type> = match &v.fields {
                    crate::ast::VariantFields::Unit => Vec::new(),
                    crate::ast::VariantFields::Tuple(tys) => {
                        any_payload = any_payload || !tys.is_empty();
                        tys.clone()
                    }
                    crate::ast::VariantFields::Named(fields) => {
                        any_payload = any_payload || !fields.is_empty();
                        // Track names in declaration order so the
                        // checker / lowerer can reorder `Variant
                        // { name: val, ... }` into positional form.
                        let names: Vec<String> = fields
                            .iter()
                            .map(|f| f.name.name.clone())
                            .collect();
                        self.enum_variant_field_names
                            .insert(variant_id, names);
                        fields.iter().map(|f| f.ty.clone()).collect()
                    }
                };
                self.enum_variant_payloads.insert(variant_id, payload_tys);
            }
            self.enum_variants.insert(id, variants);
            if any_payload {
                self.enum_has_payload.insert(id);
            }
        }

        // For traits, stash the method signatures so the checker can
        // validate impls and resolve bounded-generic method calls.
        if let Item::Trait(t) = item {
            self.trait_methods.insert(id, t.methods.clone());
            let assoc: Vec<String> =
                t.assoc_types.iter().map(|a| a.name.name.clone()).collect();
            self.trait_assoc_types.insert(id, assoc);
            // Session 071: for each method with a default body,
            // mint a fresh default-fn sym + a Self-type-param sym
            // bounded by the trait. The body itself is resolved in
            // pass 2; here we just allocate the names so other
            // items in pass 1 / pass 2 can reference them.
            for m in &t.methods {
                if m.body.is_some() {
                    let default_name = format!(
                        "__default_{}__{}",
                        t.name.name, m.name.name
                    );
                    let default_sym = SymbolId(self.symbols.len() as u32);
                    // Span = m.name.span so user_method_sig's
                    // `fn_signatures[symbol(sym).span]` lookup works
                    // (matches the key the checker uses when stashing
                    // the default's fn signature).
                    self.symbols.push(Symbol {
                        name: default_name.clone(),
                        span: m.name.span,
                        kind: SymbolKind::Fn,
                    });
                    // Visibility: same as the trait (anyone who
                    // can see the trait can dispatch to the default).
                    self.item_vis.insert(
                        default_sym,
                        (self.current_path.clone(), t.vis == crate::ast::Visibility::Pub),
                    );
                    self.trait_defaults
                        .insert((id, m.name.name.clone()), default_sym);
                    // Self-type-param sym for this default. The
                    // bound is the trait itself, so `self.next()`
                    // inside the body routes via trait_bound_method_sig.
                    let self_sym = SymbolId(self.symbols.len() as u32);
                    self.symbols.push(Symbol {
                        name: "Self".into(),
                        span: m.span,
                        kind: SymbolKind::TypeParam,
                    });
                    self.default_self_syms.insert(default_sym, self_sym);
                    self.generic_bounds.insert(self_sym, vec![id]);
                }
            }
        }
    }

    // ---- pass 2: resolve bodies (recursing into modules) ----

    /// For each trait, BFS its supertrait chain — if the trait is
    /// reachable from itself there is a cycle. The conformance and
    /// method-lookup walks below all use visited-sets so a cycle
    /// never hangs them, but it is worth surfacing as a clear error.
    fn validate_supertrait_cycles(&mut self) {
        let traits: Vec<SymbolId> = self.trait_supertraits.keys().copied().collect();
        for start in traits {
            let mut queue: std::collections::VecDeque<SymbolId> =
                std::collections::VecDeque::new();
            let mut visited: std::collections::HashSet<SymbolId> =
                std::collections::HashSet::new();
            if let Some(supers) = self.trait_supertraits.get(&start) {
                queue.extend(supers);
            }
            let mut cyclic = false;
            while let Some(t) = queue.pop_front() {
                if t == start {
                    cyclic = true;
                    break;
                }
                if !visited.insert(t) {
                    continue;
                }
                if let Some(supers) = self.trait_supertraits.get(&t) {
                    queue.extend(supers);
                }
            }
            if cyclic {
                let name = self.symbols[start.0 as usize].name.clone();
                let span = self.symbols[start.0 as usize].span;
                self.error(
                    format!("supertrait cycle through `{}`", name),
                    span,
                );
            }
        }
    }

    fn resolve_items(&mut self, items: &[Item]) {
        for item in items {
            self.resolve_item(item);
        }
    }

    fn resolve_item(&mut self, item: &Item) {
        match item {
            Item::Fn(f) => self.resolve_fn(f),
            Item::Struct(s) => self.resolve_struct(s),
            Item::Enum(e) => self.resolve_enum(e),
            Item::Const(c) => self.resolve_const(c),
            Item::Impl(i) => {
                // Now that struct_generics is populated (we're past
                // the source-order resolve_struct of the for-type),
                // wire impl-side generic syms to struct-side ones
                // positionally. Bound info recorded in resolve_fn
                // (called below for each method) is keyed by impl's
                // syms; the checker translates via this map at
                // struct-lit time.
                if let Some(&struct_sym) =
                    self.path_to_sym.get(&i.type_path.span)
                {
                    if let Some(struct_gens) =
                        self.struct_generics.get(&struct_sym).cloned()
                    {
                        for (idx, g) in i.generics.iter().enumerate() {
                            if let Some(&impl_g_sym) =
                                self.decl_to_sym.get(&g.name.span)
                            {
                                if let Some(&struct_g) = struct_gens.get(idx) {
                                    self.impl_to_struct_generic
                                        .insert(impl_g_sym, struct_g);
                                }
                            }
                        }
                    }
                }
                for method in &i.methods {
                    self.resolve_fn(method);
                }
            }
            Item::Trait(t) => {
                // Resolve supertrait paths to trait symbols. Each
                // entry may be a single-segment name (`A`) or a
                // module-qualified path (`std::Iterator`); both go
                // through `lookup_path`.
                if let Some(&trait_sym) = self.decl_to_sym.get(&t.name.span) {
                    let mut super_syms = Vec::new();
                    for s in &t.supertraits {
                        let display = path_display(s);
                        if let Some((ssym, _)) = self.lookup_path(&s.segments) {
                            if matches!(
                                self.symbols[ssym.0 as usize].kind,
                                SymbolKind::Trait
                            ) {
                                super_syms.push(ssym);
                            } else {
                                self.error(
                                    format!("`{}` is not a trait", display),
                                    s.span,
                                );
                            }
                        } else {
                            self.error(
                                format!("unresolved trait `{}`", display),
                                s.span,
                            );
                        }
                    }
                    self.trait_supertraits.insert(trait_sym, super_syms);
                }
                // Resolve the parameter / return types of each trait
                // method signature so `Self` and any referenced types
                // bind. The bodies don't exist (signatures only).
                // The trait's `<A, R>` generic params are in scope
                // for every method signature (and any assoc-type
                // binding). Mint them via `intern_generic_param`
                // so the checker / lowerer agree on a single sym
                // per param across the trait's surface — session
                // 056's pattern for impl/struct generics.
                self.enter_scope();
                let mut trait_gen_syms: Vec<SymbolId> = Vec::with_capacity(t.generics.len());
                for g in &t.generics {
                    trait_gen_syms.push(self.intern_generic_param(g));
                }
                if !trait_gen_syms.is_empty() {
                    if let Some(&trait_sym) = self.decl_to_sym.get(&t.name.span) {
                        self.trait_generics.insert(trait_sym, trait_gen_syms);
                    }
                }
                for m in &t.methods {
                    self.enter_scope();
                    // Session 071: for methods with a default body,
                    // bring `Self` into scope as a TypeParam (the
                    // sym was minted in pass 1; bound is the trait).
                    // `self.next()`-style calls inside the body then
                    // resolve via trait_bound_method_sig at type-check
                    // time. Self stays in scope only for this method;
                    // a sibling method without a default doesn't see
                    // it.
                    let trait_sym = self
                        .decl_to_sym
                        .get(&t.name.span)
                        .copied();
                    let default_self_sym = trait_sym
                        .and_then(|ts| {
                            self.trait_defaults
                                .get(&(ts, m.name.name.clone()))
                                .copied()
                        })
                        .and_then(|fn_sym| {
                            self.default_self_syms.get(&fn_sym).copied()
                        });
                    if let Some(self_sym) = default_self_sym {
                        self.scopes
                            .last_mut()
                            .unwrap()
                            .insert("Self".into(), self_sym);
                    }
                    for p in &m.params {
                        self.resolve_type(&p.ty);
                    }
                    if let Some(rt) = &m.return_type {
                        self.resolve_type(rt);
                    }
                    // Default body resolution: scope params as locals
                    // (so the body's `self`, etc. resolve to Param
                    // syms), then resolve the body's expressions /
                    // statements. Identical to resolve_fn's body
                    // pass — just inlined here since we don't have
                    // a FnDecl to hand off.
                    if let Some(body) = &m.body {
                        for p in &m.params {
                            let pid = self.intern(
                                p.name.name.clone(),
                                p.name.span,
                                SymbolKind::Param,
                            );
                            self.decl_to_sym.insert(p.name.span, pid);
                        }
                        self.resolve_block(body);
                    }
                    self.exit_scope();
                }
                self.exit_scope();
            }
            Item::Mod(md) => {
                self.current_path.push(md.name.name.clone());
                self.resolve_items(&md.items);
                self.current_path.pop();
            }
            Item::Use(_) => {
                // Already resolved in pass 1.7.
            }
        }
    }

    fn resolve_fn(&mut self, f: &FnDecl) {
        self.enter_scope();
        // Two passes over generic params: first intern every name so
        // a later param's bound can mention an earlier param OR a
        // later one — `<I, F: Fn1<I::Item, U>, U>` references `U`
        // before its declaration. Without this, bound resolution of
        // F sees `U` unresolved. Second pass does the bound-arg
        // resolution itself.
        let mut param_ids: Vec<SymbolId> = Vec::with_capacity(f.generics.len());
        for g in &f.generics {
            // A generic-impl method carries the impl's `<T>` (merged
            // in by the parser). The first method of the impl to
            // resolve interns that param; later methods reuse the
            // same symbol — keyed by span — so every method agrees
            // on which `SymbolId` the impl's `T` is.
            let id = if let Some(&existing) = self.decl_to_sym.get(&g.name.span) {
                self.scopes
                    .last_mut()
                    .unwrap()
                    .insert(g.name.name.clone(), existing);
                existing
            } else {
                let id = self.intern(g.name.name.clone(), g.name.span, SymbolKind::TypeParam);
                self.decl_to_sym.insert(g.name.span, id);
                id
            };
            param_ids.push(id);
        }
        for (g, id) in f.generics.iter().zip(param_ids.iter().copied()) {
            // Resolve each `T: Bound` to the bound trait's symbol.
            // Bounds may be paths (`std::Iterator`), so dispatch
            // through `lookup_path` rather than `lookup`. Each bound
            // path may carry generic args (`F: Fn1<I::Item, U>`);
            // resolve those into `type_resolutions` so the checker
            // can propagate inference from bound args back to outer
            // type params.
            let mut bound_syms: Vec<SymbolId> = Vec::new();
            for b in &g.bounds {
                let display = path_display(b);
                if let Some((bsym, _)) = self.lookup_path(&b.segments) {
                    if matches!(self.symbols[bsym.0 as usize].kind, SymbolKind::Trait) {
                        bound_syms.push(bsym);
                        if !b.generic_args.is_empty() {
                            let mut arg_spans = Vec::with_capacity(b.generic_args.len());
                            for a in &b.generic_args {
                                self.resolve_type(a);
                                arg_spans.push(a.span());
                            }
                            self.generic_bound_args.insert((id, bsym), arg_spans);
                        }
                    } else {
                        self.error(
                            format!("`{}` is not a trait", display),
                            b.span,
                        );
                    }
                } else {
                    self.error(format!("unresolved trait `{}`", display), b.span);
                }
            }
            if !bound_syms.is_empty() {
                self.generic_bounds.insert(id, bound_syms);
            }
        }
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
        self.enter_scope();
        let mut gen_syms: Vec<SymbolId> = Vec::with_capacity(s.generics.len());
        for g in &s.generics {
            let id = self.intern(g.name.name.clone(), g.name.span, SymbolKind::TypeParam);
            self.decl_to_sym.insert(g.name.span, id);
            gen_syms.push(id);
        }
        if !gen_syms.is_empty() {
            if let Some(&struct_sym) = self.decl_to_sym.get(&s.name.span) {
                self.struct_generics.insert(struct_sym, gen_syms);
            }
        }
        for f in &s.fields {
            self.resolve_type(&f.ty);
        }
        self.exit_scope();
    }

    fn resolve_enum(&mut self, e: &EnumDecl) {
        self.enter_scope();
        let mut gen_syms: Vec<SymbolId> = Vec::with_capacity(e.generics.len());
        for g in &e.generics {
            let id = self.intern(g.name.name.clone(), g.name.span, SymbolKind::TypeParam);
            self.decl_to_sym.insert(g.name.span, id);
            gen_syms.push(id);
        }
        if !gen_syms.is_empty() {
            if let Some(&enum_sym) = self.decl_to_sym.get(&e.name.span) {
                self.enum_generics.insert(enum_sym, gen_syms);
            }
        }
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
        self.exit_scope();
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
            Pattern::Range { .. } => {
                // Bounds are literals — no names to resolve, no bindings.
            }
            Pattern::Path { path, .. } => {
                // No binding — just resolve the path so the checker /
                // lowerer can look up what variant it refers to.
                self.resolve_path(path);
            }
            Pattern::TupleVariant { path, fields, .. } => {
                // Resolve the variant path; declare any inner bindings.
                // The checker validates that each sub-pattern position
                // matches the variant's payload type.
                self.resolve_path(path);
                for sub in fields {
                    self.declare_pattern(sub, mutable_let);
                }
            }
            Pattern::NamedVariant { path, fields, .. } => {
                self.resolve_path(path);
                for (_, sub) in fields {
                    self.declare_pattern(sub, mutable_let);
                }
            }
            Pattern::Or { patterns, .. } => {
                for sub in patterns {
                    self.declare_pattern(sub, mutable_let);
                }
            }
            Pattern::Tuple { patterns, .. } => {
                for sub in patterns {
                    self.declare_pattern(sub, mutable_let);
                }
            }
        }
    }

    fn resolve_type(&mut self, t: &Type) {
        match t {
            Type::Path(p) => {
                // `Self::Item` — leave it for the checker, which
                // resolves it against the enclosing trait/impl.
                // Resolving here would report `Self` unresolved.
                // Single-segment `Self`: session 071 puts a real
                // sym in scope for default-method bodies, so look
                // it up here — that lets the checker's resolve_type
                // see `Ty::TypeVar(self_sym)` for `self: Self`.
                // Multi-segment `Self::...` still defers.
                if p.segments.first().map(|s| s.name.as_str()) == Some("Self") {
                    if p.segments.len() == 1 {
                        if let Some(sym) = self.lookup("Self") {
                            self.path_to_sym.insert(p.span, sym);
                        }
                    }
                    return;
                }
                // `T::Item` where `T` is a type parameter in scope —
                // the checker turns this into a `Ty::Assoc(TypeVar(T),
                // name)` projection. Resolving the path here would
                // search for a global `T::Item` and report it
                // unresolved.
                if p.segments.len() == 2 {
                    if let Some(sym) = self.lookup(&p.segments[0].name) {
                        if matches!(
                            self.symbols[sym.0 as usize].kind,
                            SymbolKind::TypeParam
                        ) {
                            // Record the base TypeParam so the checker
                            // builds `Ty::Assoc(TypeVar(T), name)`.
                            self.assoc_proj_bases.insert(p.span, sym);
                            return;
                        }
                    }
                }
                self.resolve_path(p);
            }
            Type::Dyn(p) => self.resolve_path(p),
            Type::Array { elem, .. } => self.resolve_type(elem),
            Type::Fn { params, ret, .. } => {
                for p in params {
                    self.resolve_type(p);
                }
                if let Some(r) = ret {
                    self.resolve_type(r);
                }
            }
            Type::Tuple { elems, .. } => {
                for e in elems {
                    self.resolve_type(e);
                }
            }
        }
    }

    fn resolve_path(&mut self, p: &Path) {
        // Recurse into generic args so any nested paths resolve.
        for arg in &p.generic_args {
            self.resolve_type(arg);
        }
        // 1. Try the whole path as a single (possibly module-
        //    qualified) item: `f`, `m::f`, `a::b::c`.
        if let Some((id, key)) = self.lookup_path(&p.segments) {
            self.check_path_visibility(&key, p.span);
            self.path_to_sym.insert(p.span, id);
            self.check_closure_capture(id, p.span);
            return;
        }
        // 2. `Enum::Variant` — the leading segments name an enum
        //    type (possibly module-qualified), the last is a variant.
        if p.segments.len() >= 2 {
            let n = p.segments.len();
            let type_segs = &p.segments[..n - 1];
            let variant_name = &p.segments[n - 1].name;
            if let Some((enum_id, type_key)) = self.lookup_path(type_segs) {
                if matches!(
                    self.symbols[enum_id.0 as usize].kind,
                    SymbolKind::Enum
                ) {
                    if let Some(map) = self.enum_variants.get(&enum_id) {
                        if let Some(&variant_id) = map.get(variant_name) {
                            self.check_path_visibility(&type_key, p.span);
                            if !self.is_visible(variant_id) {
                                self.visibility_error(
                                    variant_name,
                                    variant_id,
                                    p.span,
                                );
                            }
                            self.path_to_sym.insert(p.span, variant_id);
                            return;
                        }
                    }
                    self.error(
                        format!("no variant `{}` on that enum", variant_name),
                        p.span,
                    );
                    return;
                }
            }
        }
        let shown: Vec<&str> =
            p.segments.iter().map(|s| s.name.as_str()).collect();
        if shown.len() == 1 {
            self.error(format!("unresolved name `{}`", shown[0]), p.span);
        } else {
            self.error(
                format!("unresolved path `{}`", shown.join("::")),
                p.span,
            );
        }
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
            Expr::Tuple { elems, .. } => {
                for e in elems {
                    self.resolve_expr(e);
                }
            }
            Expr::TupleIndex { receiver, .. } => {
                self.resolve_expr(receiver);
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
            Expr::StructLit { path, fields, .. } => {
                self.resolve_path(path);
                for f in fields {
                    self.resolve_expr(&f.value);
                }
            }
            Expr::Return { value, .. } => {
                if let Some(v) = value {
                    self.resolve_expr(v);
                }
            }
            Expr::Break(_) | Expr::Continue(_) => {}
            Expr::Closure { params, body, span } => {
                self.resolve_closure(params, body, *span);
            }
        }
    }

    /// Resolve a closure expression. Mints a synthetic fn `SymbolId`
    /// (kind `Fn`) keyed by the closure's source span so the
    /// checker and lowerer agree on its identity, opens a body
    /// scope, declares each param as `Param`, resolves param types
    /// + body, and rejects any path inside the body that escapes
    /// to a Local/Param declared outside the closure's span (v0.x
    /// non-capturing only).
    fn resolve_closure(
        &mut self,
        params: &[crate::ast::ClosureParam],
        body: &Expr,
        span: Span,
    ) {
        // Mint the synthetic fn sym at the global level (its
        // mangled name lives in scopes[0] so the lowerer's lookup
        // by sym works). Use a module-prefix-aware name so two
        // closures in different modules don't collide on codegen
        // names.
        let counter = self.lambda_counter;
        self.lambda_counter += 1;
        let lambda_name = format!("__lambda_{}", counter);
        let module_qualified = if self.current_path.is_empty() {
            lambda_name.clone()
        } else {
            format!("{}::{}", self.current_path.join("::"), lambda_name)
        };
        let fn_sym = SymbolId(self.symbols.len() as u32);
        self.symbols.push(Symbol {
            name: lambda_name,
            span,
            kind: SymbolKind::Fn,
        });
        self.scopes[0].insert(module_qualified, fn_sym);
        self.closure_fn_sym.insert(span, fn_sym);
        // Closure body scope.
        self.enter_scope();
        self.open_closure_spans.push(span);
        let mut param_syms: Vec<SymbolId> = Vec::with_capacity(params.len());
        for p in params {
            let id = self.intern(p.name.name.clone(), p.name.span, SymbolKind::Param);
            self.decl_to_sym.insert(p.name.span, id);
            if let Some(t) = &p.ty {
                self.resolve_type(t);
            }
            param_syms.push(id);
        }
        self.closure_params.insert(span, param_syms);
        self.resolve_expr(body);
        self.open_closure_spans.pop();
        self.exit_scope();
        // If any captures were detected during body resolution,
        // mint the synth struct + call method syms now. The
        // lowerer materializes a struct value with one field per
        // capture, plus an impl method whose body is the
        // capture-rewritten lambda body. Non-capturing closures
        // skip this and reuse the session-057 anonymous-fn path
        // via `closure_fn_sym`.
        if self.closure_captures.get(&span).map(|c| !c.is_empty()).unwrap_or(false) {
            let struct_name = format!("__Closure_{}", counter);
            let struct_sym = SymbolId(self.symbols.len() as u32);
            self.symbols.push(Symbol {
                name: struct_name.clone(),
                span,
                kind: SymbolKind::Struct,
            });
            let struct_qualified = if self.current_path.is_empty() {
                struct_name.clone()
            } else {
                format!("{}::{}", self.current_path.join("::"), struct_name)
            };
            self.scopes[0].insert(struct_qualified, struct_sym);
            self.closure_struct_sym.insert(span, struct_sym);

            let call_name = format!("__Closure_{}__call", counter);
            let call_sym = SymbolId(self.symbols.len() as u32);
            self.symbols.push(Symbol {
                name: call_name,
                span,
                kind: SymbolKind::Fn,
            });
            self.closure_call_method_sym.insert(span, call_sym);
            // Register as an impl method so monomorphize's
            // resolve_method_calls can rewrite `x.call(arg)` on a
            // closure struct into a direct Call.
            self.impl_methods.insert((struct_sym, "call".into()), call_sym);
            // Mark the closure struct as implementing Fn1 (found
            // by name). The conformance check in `check_assignable`
            // reads this set when a closure value is coerced to a
            // Ty::Fn position. If Fn1 isn't in the prelude yet
            // (shouldn't happen at this point), skip silently —
            // the dispatch falls through to other paths.
            for (idx, sym) in self.symbols.iter().enumerate() {
                if sym.name == "Fn1" && matches!(sym.kind, SymbolKind::Trait) {
                    self.impls_for
                        .entry(struct_sym)
                        .or_default()
                        .insert(SymbolId(idx as u32));
                    break;
                }
            }
        }
    }
}

/// `a::b::c` for diagnostics. The resolver carries `Path` values
/// in trait-bound positions now; the original error messages used
/// the bare `Ident.name`, so this preserves the format for the
/// single-segment case while showing the full path for qualified
/// bounds like `std::Iterator`.
fn path_display(p: &crate::ast::Path) -> String {
    p.segments
        .iter()
        .map(|s| s.name.as_str())
        .collect::<Vec<_>>()
        .join("::")
}
