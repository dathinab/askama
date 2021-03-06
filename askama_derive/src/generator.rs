use input::TemplateInput;
use parser::{self, Cond, Expr, Macro, MatchParameter, MatchVariant, Node, Target, When, WS};
use shared::{filters, path};

use quote::ToTokens;
use proc_macro2::Span;

use std::{cmp, hash, str};
use std::path::Path;
use std::collections::{HashMap, HashSet};

use syn;


pub fn generate(input: &TemplateInput, nodes: &[Node], imported: &HashMap<(&str, &str), Macro>)
                -> String {
    Generator::default().build(&State::new(input, nodes, imported))
}

struct State<'a> {
    input: &'a TemplateInput<'a>,
    nodes: &'a [Node<'a>],
    blocks: Vec<&'a Node<'a>>,
    macros: MacroMap<'a>,
    trait_name: String,
    derived: bool,
}

impl<'a> State<'a> {
    fn new<'n>(input: &'n TemplateInput, nodes: &'n [Node], imported:
               &'n HashMap<(&'n str, &'n str), Macro<'n>>) -> State<'n> {
        let mut base = None;
        let mut blocks = Vec::new();
        let mut macros = HashMap::new();

        for n in nodes {
            match n {
                Node::Extends(Expr::StrLit(path)) => match base {
                    Some(_) => panic!("multiple extend blocks found"),
                    None => {
                        base = Some(*path);
                    },
                },
                def @ Node::BlockDef(_, _, _, _) => {
                    blocks.push(def);
                },
                Node::Macro(name, m) => {
                    macros.insert((None, *name), m);
                },
                _ => {},
            }
        }

        let mut check_nested = 0;
        let mut nested_blocks = Vec::new();
        while check_nested < blocks.len() {
            if let Node::BlockDef(_, _, ref nodes, _) = blocks[check_nested] {
                for n in nodes {
                    if let def @ Node::BlockDef(_, _, _, _) = n {
                        nested_blocks.push(def);
                    }
                }
            } else {
                panic!("non block found in list of blocks");
            }
            blocks.append(&mut nested_blocks);
            check_nested += 1;
        }

        for (&(scope, name), m) in imported {
            macros.insert((Some(scope), name), m);
        }

        State {
            input,
            nodes,
            blocks,
            macros,
            trait_name: match base {
                Some(user_path) => trait_name_for_path(
                    &path::find_template_from_path(user_path, Some(&input.path))
                ),
                None => trait_name_for_path(&input.path),
            },
            derived: base.is_some(),
        }
    }
}

fn trait_name_for_path(path: &Path) -> String {
    let mut res = String::new();
    res.push_str("TraitFrom");
    for c in path.to_string_lossy().chars() {
        if c.is_alphanumeric() {
            res.push(c);
        } else {
            res.push_str(&format!("{:x}", c as u32));
        }
    }
    res
}

fn get_parent_type(ast: &syn::DeriveInput) -> Option<&syn::Type> {
    match ast.data {
        syn::Data::Struct(syn::DataStruct {
            fields: syn::Fields::Named(ref fields),
            ..
        }) => fields.named.iter().filter_map(|f| {
            f.ident.as_ref().and_then(|name| {
                if name == "_parent" {
                    Some(&f.ty)
                } else {
                    None
                }
            })
        }),
        _ => panic!("derive(Template) only works for struct items"),
    }.next()
}

struct Generator<'a> {
    buf: String,
    indent: u8,
    start: bool,
    locals: SetChain<'a, &'a str>,
    next_ws: Option<&'a str>,
    skip_ws: bool,
    vars: usize,
    impl_blocks: bool,
}

impl<'a> Generator<'a> {
    fn new<'n>(locals: SetChain<'n, &'n str>, indent: u8) -> Generator<'n> {
        Generator {
            buf: String::new(),
            indent,
            start: true,
            locals,
            next_ws: None,
            skip_ws: false,
            vars: 0,
            impl_blocks: false,
        }
    }

    fn default<'n>() -> Generator<'n> {
        Self::new(SetChain::new(), 0)
    }

    fn child(&mut self) -> Generator {
        let locals = SetChain::with_parent(&self.locals);
        Self::new(locals, self.indent)
    }

    // Takes a State and generates the relevant implementations.
    fn build(mut self, state: &'a State) -> String {
        if !state.blocks.is_empty() {
            if !state.derived {
                self.define_trait(state);
            } else {
                let parent_type = get_parent_type(state.input.ast)
                    .expect("expected field '_parent' in extending template struct");
                self.deref_to_parent(state, parent_type);
            }

            let trait_nodes = if !state.derived {
                Some(&state.nodes[..])
            } else {
                None
            };
            self.impl_trait(state, trait_nodes);
            self.impl_template_for_trait(state);
        } else {
            self.impl_template(state);
        }
        self.impl_display(state);
        if cfg!(feature = "iron") {
            self.impl_modifier_response(state);
        }
        if cfg!(feature = "rocket") {
            self.impl_responder(state);
        }
        self.buf
    }

    // Implement `Template` for the given context struct.
    fn impl_template(&mut self, state: &'a State) {
        self.write_header(state, "::askama::Template", None);
        self.writeln("fn render_into(&self, writer: &mut ::std::fmt::Write) -> \
                      ::askama::Result<()> {");
        self.writeln("#[allow(unused_imports)] use ::std::ops::Deref as HiddenDerefTrait;");
        self.handle(state, state.nodes, AstLevel::Top);
        self.flush_ws(&WS(false, false));
        self.writeln("Ok(())");
        self.writeln("}");
        self.writeln("}");
    }

    // Implement `Display` for the given context struct.
    fn impl_display(&mut self, state: &'a State) {
        self.write_header(state, "::std::fmt::Display", None);
        self.writeln("fn fmt(&self, f: &mut ::std::fmt::Formatter) -> ::std::fmt::Result {");
        self.writeln("self.render_into(f).map_err(|_| ::std::fmt::Error {})");
        self.writeln("}");
        self.writeln("}");
    }

    // Implement `Deref<Parent>` for an inheriting context struct.
    fn deref_to_parent(&mut self, state: &'a State, parent_type: &syn::Type) {
        self.write_header(state, "::std::ops::Deref", None);
        self.writeln(&format!("type Target = {};", parent_type.into_token_stream()));
        self.writeln("fn deref(&self) -> &Self::Target {");
        self.writeln("&self._parent");
        self.writeln("}");
        self.writeln("}");
    }

    // Implement `TraitFromPathName` for the given context struct.
    fn impl_trait(&mut self, state: &'a State, nodes: Option<&'a [Node]>) {
        self.write_header(state, &state.trait_name, None);
        self.write_block_defs(state);

        self.writeln("#[allow(unused_variables)]");
        self.writeln(&format!(
            "fn render_trait_into(&self, timpl: &{}, writer: &mut ::std::fmt::Write) \
             -> ::askama::Result<()> {{",
            state.trait_name
        ));
        self.writeln("#[allow(unused_imports)] use ::std::ops::Deref as HiddenDerefTrait;");

        if let Some(nodes) = nodes {
            self.impl_blocks = true;
            self.handle(state, nodes, AstLevel::Top);
            self.flush_ws(&WS(false, false));
            self.impl_blocks = false;
            self.writeln("Ok(())");
        } else {
            self.writeln("self._parent.render_trait_into(self, writer)");
        }

        self.writeln("}");
        self.flush_ws(&WS(false, false));
        self.writeln("}");
    }

    // Implement `Template` for templates that implement a template trait.
    fn impl_template_for_trait(&mut self, state: &'a State) {
        self.write_header(state, "::askama::Template", None);
        self.writeln("fn render_into(&self, writer: &mut ::std::fmt::Write) \
                      -> ::askama::Result<()> {");
        if state.derived {
            self.writeln("self._parent.render_trait_into(self, writer)");
        } else {
            self.writeln("self.render_trait_into(self, writer)");
        }
        self.writeln("}");
        self.writeln("}");
    }

    // Defines the `TraitFromPathName` trait.
    fn define_trait(&mut self, state: &'a State) {
        self.writeln(&format!("pub trait {} {{", state.trait_name));
        self.write_block_defs(state);
        self.writeln(&format!(
            "fn render_trait_into(&self, timpl: &{}, writer: &mut ::std::fmt::Write) \
             -> ::askama::Result<()>;",
            state.trait_name));
        self.writeln("}");
    }

    // Implement iron's Modifier<Response> if enabled
    fn impl_modifier_response(&mut self, state: &'a State) {
        self.write_header(state, "::askama::iron::Modifier<::askama::iron::Response>", None);
        self.writeln("fn modify(self, res: &mut ::askama::iron::Response) {");
        self.writeln("res.body = Some(Box::new(self.render().unwrap().into_bytes()));");

        let ext = state.input.path.extension().map_or("", |s| s.to_str().unwrap_or(""));
        match ext {
            "html" | "htm" => {
                self.writeln("::askama::iron::ContentType::html().0.modify(res);");
            },
            _ => (),
        };

        self.writeln("}");
        self.writeln("}");
    }

    // Implement Rocket's `Responder`.
    fn impl_responder(&mut self, state: &'a State) {
        let lifetime = syn::Lifetime::new("'askama", Span::call_site());
        let param = syn::GenericParam::Lifetime(syn::LifetimeDef::new(lifetime));
        self.write_header(state, "::askama::rocket::Responder<'askama>", Some(vec![param]));
        self.writeln("fn respond_to(self, _: &::askama::rocket::Request) \
                      -> ::askama::rocket::Result<'askama> {");

        let ext = match state.input.path.extension() {
            Some(s) => s.to_str().unwrap(),
            None => "txt",
        };
        self.writeln(&format!("::askama::rocket::respond(&self, {:?})", ext));

        self.writeln("}");
        self.writeln("}");
    }

    // Writes header for the `impl` for `TraitFromPathName` or `Template`
    // for the given context struct.
    fn write_header(&mut self, state: &'a State, target: &str,
                    params: Option<Vec<syn::GenericParam>>) {
        let mut generics = state.input.ast.generics.clone();
        if let Some(params) = params {
            for param in params {
                generics.params.push(param);
            }
        }
        let (_, orig_ty_generics, _) = state.input.ast.generics.split_for_impl();
        let (impl_generics, _, where_clause) = generics.split_for_impl();
        self.writeln(
            format!(
                "{} {} for {}{} {{",
                quote!(impl#impl_generics),
                target,
                state.input.ast.ident,
                quote!(#orig_ty_generics #where_clause),
            ).as_ref(),
        );
    }

    /* Helper methods for handling node types */

    fn handle(&mut self, state: &'a State, nodes: &'a [Node], level: AstLevel) {
        for n in nodes {
            match *n {
                Node::Lit(lws, val, rws) => {
                    self.write_lit(lws, val, rws);
                },
                Node::Comment(ref ws) => {
                    self.write_comment(ws);
                },
                Node::Expr(ref ws, ref val) => {
                    self.write_expr(state, ws, val);
                },
                Node::LetDecl(ref ws, ref var) => {
                    self.write_let_decl(ws, var);
                },
                Node::Let(ref ws, ref var, ref val) => {
                    self.write_let(ws, var, val);
                },
                Node::Cond(ref conds, ref ws) => {
                    self.write_cond(state, conds, ws);
                },
                Node::Match(ref ws1, ref expr, inter, ref arms, ref ws2) => {
                    self.write_match(state, ws1, expr, inter, arms, ws2);
                },
                Node::Loop(ref ws1, ref var, ref iter, ref body, ref ws2) => {
                    self.write_loop(state, ws1, var, iter, body, ws2);
                },
                Node::BlockDef(ref ws1, name, _, ref ws2) => {
                    if let AstLevel::Nested = level {
                        panic!("blocks ('{}') are only allowed at the top level of a template \
                                or another block", name);
                    }
                    self.write_block(ws1, name, ws2);
                },
                Node::Include(ref ws, path) => {
                    self.handle_include(state, ws, path);
                },
                Node::Call(ref ws, scope, name, ref args) => {
                    self.write_call(state, ws, scope, name, args);
                },
                Node::Macro(_, ref m) => {
                    if let AstLevel::Nested = level {
                        panic!("macro blocks only allowed at the top level");
                    }
                    self.flush_ws(&m.ws1);
                    self.prepare_ws(&m.ws2);
                },
                Node::Import(ref ws, _, _) => {
                    if let AstLevel::Nested = level {
                        panic!("import blocks only allowed at the top level");
                    }
                    self.handle_ws(ws);
                },
                Node::Extends(_) => {
                    if let AstLevel::Nested = level {
                        panic!("extend blocks only allowed at the top level");
                    }
                    // No whitespace handling: child template top-level is not used,
                    // except for the blocks defined in it.
                },
            }
        }
    }

    fn write_block_defs(&mut self, state: &'a State) {
        for b in &state.blocks {
            if let Node::BlockDef(ref ws1, name, ref nodes, ref ws2) = **b {
                self.writeln("#[allow(unused_variables)]");
                self.writeln(&format!(
                    "fn render_block_{}_into(&self, writer: &mut ::std::fmt::Write) \
                     -> ::askama::Result<()> {{",
                    name
                ));
                self.prepare_ws(ws1);

                self.locals.push();
                self.handle(state, nodes, AstLevel::Block);
                self.locals.pop();

                self.flush_ws(ws2);
                self.writeln("Ok(())");
                self.writeln("}");
            } else {
                panic!("only block definitions allowed here");
            }
        }
    }

    fn write_cond(&mut self, state: &'a State, conds: &'a [Cond], ws: &WS) {
        for (i, &(ref cws, ref cond, ref nodes)) in conds.iter().enumerate() {
            self.handle_ws(cws);
            match *cond {
                Some(ref expr) => {
                    let expr_code = self.visit_expr_root(expr);
                    if i == 0 {
                        self.write("if ");
                    } else {
                        self.dedent();
                        self.write("} else if ");
                    }
                    self.write(&expr_code);
                },
                None => {
                    self.dedent();
                    self.write("} else");
                },
            }
            self.writeln(" {");
            self.locals.push();
            self.handle(state, nodes, AstLevel::Nested);
            self.locals.pop();
        }
        self.handle_ws(ws);
        self.writeln("}");
    }

    fn write_match(&mut self, state: &'a State, ws1: &WS, expr: &Expr, inter: Option<&'a str>,
                   arms: &'a [When], ws2: &WS) {
        self.flush_ws(ws1);
        if let Some(inter) = inter {
            if !inter.is_empty() {
                self.next_ws = Some(inter);
            }
        }

        let expr_code = self.visit_expr_root(expr);
        self.writeln(&format!("match (&{}).deref() {{", expr_code));
        for arm in arms {
            let &(ref ws, ref variant, ref params, ref body) = arm;
            self.locals.push();
            match *variant {
                Some(ref param) => {
                    self.visit_match_variant(param);
                },
                None => self.write("_"),
            };
            if !params.is_empty() {
                self.write("(");
                for (i, param) in params.iter().enumerate() {
                    if let MatchParameter::Name(p) = *param {
                        self.locals.insert(p);
                    }
                    if i > 0 {
                        self.write(", ");
                    }
                    self.visit_match_param(param);
                }
                self.write(")");
            }
            self.writeln(" => {");
            self.handle_ws(ws);
            self.handle(state, body, AstLevel::Nested);
            self.writeln("}");
            self.locals.pop();
        }

        self.writeln("}");
        self.handle_ws(ws2);
    }

    fn write_loop(&mut self, state: &'a State, ws1: &WS, var: &'a Target, iter: &Expr,
                  body: &'a [Node], ws2: &WS) {
        self.handle_ws(ws1);
        self.locals.push();

        let expr_code = self.visit_expr_root(iter);
        self.write("for (_loop_index, ");
        let targets = self.visit_target(var);
        for name in &targets {
            self.locals.insert(name);
            self.write(name);
        }
        self.writeln(&format!(") in (&{}).into_iter().enumerate() {{", expr_code));

        self.handle(state, body, AstLevel::Nested);
        self.handle_ws(ws2);
        self.writeln("}");
        self.locals.pop();
    }

    fn write_call(&mut self, state: &'a State, ws: &WS, scope: Option<&str>, name: &str,
                  args: &[Expr]) {
        let def = state.macros.get(&(scope, name)).unwrap_or_else(|| {
            if let Some(s) = scope {
                panic!(format!("macro '{}::{}' not found", s, name));
            } else {
                panic!(format!("macro '{}' not found", name));
            }
        });

        self.flush_ws(ws); // Cannot handle_ws() here: whitespace from macro definition comes first
        self.locals.push();
        self.writeln("{");
        self.prepare_ws(&def.ws1);

        for (i, arg) in def.args.iter().enumerate() {
            let expr_code = self.visit_expr_root(args.get(i)
                .expect(&format!("macro '{}' takes more than {} arguments", name, i)));
            self.write(&format!("let {} = &{};", arg, expr_code));
            self.locals.insert(arg);
        }
        self.handle(state, &def.nodes, AstLevel::Nested);

        self.flush_ws(&def.ws2);
        self.writeln("}");
        self.locals.pop();
        self.prepare_ws(ws);
    }

    fn handle_include(&mut self, state: &'a State, ws: &WS, path: &str) {
        self.flush_ws(ws);
        let path = path::find_template_from_path(path, Some(&state.input.path));
        let src = path::get_template_source(&path);
        let nodes = parser::parse(&src);
        let nested = {
            let mut gen = self.child();
            gen.handle(state, &nodes, AstLevel::Nested);
            gen.buf
        };
        self.buf.push_str(&nested);
        self.prepare_ws(ws);
    }

    fn write_let_decl(&mut self, ws: &WS, var: &'a Target) {
        self.handle_ws(ws);
        self.write("let ");
        match *var {
            Target::Name(name) => {
                self.locals.insert(name);
                self.write(name);
            },
        }
        self.writeln(";");
    }

    fn write_let(&mut self, ws: &WS, var: &'a Target, val: &Expr) {
        self.handle_ws(ws);
        let mut code = String::new();
        self.visit_expr(val, &mut code);

        match *var {
            Target::Name(name) => {
                if !self.locals.contains(name) {
                    self.write("let ");
                    self.locals.insert(name);
                }
                self.write(name);
            },
        }
        self.write(&format!(" = {};", &code));
    }

    fn write_block(&mut self, ws1: &WS, name: &str, ws2: &WS) {
        self.flush_ws(ws1);
        let ctx = if self.impl_blocks {
            "timpl"
        } else {
            "self"
        };
        self.writeln(&format!("{}.render_block_{}_into(writer)?;", ctx, name));
        self.prepare_ws(ws2);
    }

    fn write_expr(&mut self, state: &'a State, ws: &WS, s: &Expr) {
        self.handle_ws(ws);
        let mut code = String::new();
        let wrapped = self.visit_expr(s, &mut code);
        self.writeln(&format!("let askama_expr = &{};", code));

        use self::DisplayWrap::*;
        use super::input::EscapeMode::*;
        self.write("writer.write_fmt(format_args!(\"{}\", ");
        self.write(match (wrapped, &state.input.meta.escaping) {
            (Wrapped, &Html) |
            (Wrapped, &None) |
            (Unwrapped, &None) => "askama_expr",
            (Unwrapped, &Html) => "&::askama::MarkupDisplay::from(askama_expr)",
        });
        self.writeln("))?;");
    }

    fn write_lit(&mut self, lws: &'a str, val: &str, rws: &'a str) {
        assert!(self.next_ws.is_none());
        if !lws.is_empty() {
            if self.skip_ws {
                self.skip_ws = false;
            } else if val.is_empty() {
                assert!(rws.is_empty());
                self.next_ws = Some(lws);
            } else {
                self.writeln(&format!("writer.write_str({:#?})?;", lws));
            }
        }
        if !val.is_empty() {
            self.writeln(&format!("writer.write_str({:#?})?;", val));
        }
        if !rws.is_empty() {
            self.next_ws = Some(rws);
        }
    }

    fn write_comment(&mut self, ws: &WS) {
        self.handle_ws(ws);
    }

    /* Visitor methods for expression types */

    fn visit_expr_root(&mut self, expr: &Expr) -> String {
        let mut code = String::new();
        self.visit_expr(expr, &mut code);
        code
    }

    fn visit_expr(&mut self, expr: &Expr, code: &mut String) -> DisplayWrap {
        match *expr {
            Expr::NumLit(s) => self.visit_num_lit(s, code),
            Expr::StrLit(s) => self.visit_str_lit(s, code),
            Expr::Var(s) => self.visit_var(s, code),
            Expr::Path(ref path) => self.visit_path(path, code),
            Expr::Array(ref elements) => self.visit_array(elements, code),
            Expr::Attr(ref obj, name) => self.visit_attr(obj, name, code),
            Expr::Filter(name, ref args) => self.visit_filter(name, args, code),
            Expr::Unary(op, ref inner) => self.visit_unary(op, inner, code),
            Expr::BinOp(op, ref left, ref right) => self.visit_binop(op, left, right, code),
            Expr::Group(ref inner) => self.visit_group(inner, code),
            Expr::MethodCall(ref obj, method, ref args) => {
                self.visit_method_call(obj, method, args, code)
            },
        }
    }

    fn visit_match_variant(&mut self, param: &MatchVariant) -> DisplayWrap {
        let mut code = String::new();
        let wrapped = match *param {
            MatchVariant::StrLit(s) => self.visit_str_lit(s, &mut code),
            MatchVariant::NumLit(s) => {
                // Variants need to be references until match-modes land
                code.push_str("&");
                self.visit_num_lit(s, &mut code)
            },
            MatchVariant::Name(s) => {
                code.push_str("&");
                code.push_str(s);
                DisplayWrap::Unwrapped
            },
            MatchVariant::Path(ref s) => {
                code.push_str("&");
                code.push_str(&s.join("::"));
                DisplayWrap::Unwrapped
            },
        };
        self.write(&code);
        wrapped
    }

    fn visit_match_param(&mut self, param: &MatchParameter) -> DisplayWrap {
        let mut code = String::new();
        let wrapped = match *param {
            MatchParameter::NumLit(s) => self.visit_num_lit(s, &mut code),
            MatchParameter::StrLit(s) => self.visit_str_lit(s, &mut code),
            MatchParameter::Name(s) => {
                code.push_str("ref ");
                code.push_str(s);
                DisplayWrap::Unwrapped
            },
        };
        self.write(&code);
        wrapped
    }

    fn visit_filter(&mut self, name: &str, args: &[Expr], code: &mut String) -> DisplayWrap {
        if name == "format" {
            self._visit_format_filter(args, code);
            return DisplayWrap::Unwrapped;
        } else if name == "join" {
            self._visit_join_filter(args, code);
            return DisplayWrap::Unwrapped;
        }

        if filters::BUILT_IN_FILTERS.contains(&name) {
            code.push_str(&format!("::askama::filters::{}(&", name));
        } else {
            code.push_str(&format!("filters::{}(&", name));
        }

        self._visit_args(args, code);
        code.push_str(")?");
        if name == "safe" || name == "escape" || name == "e" || name == "json" {
            DisplayWrap::Wrapped
        } else {
            DisplayWrap::Unwrapped
        }
    }

    fn _visit_format_filter(&mut self, args: &[Expr], code: &mut String) {
        code.push_str("format!(");
        self._visit_args(args, code);
        code.push_str(")");
    }

    // Force type coercion on first argument to `join` filter (see #39).
    fn _visit_join_filter(&mut self, args: &[Expr], code: &mut String) {
        code.push_str("::askama::filters::join((&");
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                code.push_str(", &");
            }
            self.visit_expr(arg, code);
            if i == 0 {
                code.push_str(").into_iter()");
            }
        }
        code.push_str(")?");
    }

    fn _visit_args(&mut self, args: &[Expr], code: &mut String) {
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                code.push_str(", &");
            }

            let intercept = match *arg {
                Expr::Filter(_, _) | Expr::MethodCall(_, _, _) => true,
                _ => false,
            };

            if intercept {
                let offset = code.len();
                self.visit_expr(arg, code);
                let idx = self.vars;
                self.vars += 1;
                self.writeln(&format!("let var{} = {};", idx, &code[offset..]));
                code.truncate(offset);
                code.push_str(&format!("var{}", idx));
            } else {
                self.visit_expr(arg, code);
            }
        }
    }

    fn visit_attr(&mut self, obj: &Expr, attr: &str, code: &mut String) -> DisplayWrap {
        if let Expr::Var(name) = *obj {
            if name == "loop" {
                code.push_str("_loop_index");
                if attr == "index" {
                    code.push_str(" + 1");
                    return DisplayWrap::Unwrapped;
                } else if attr == "index0" {
                    return DisplayWrap::Unwrapped;
                } else {
                    panic!("unknown loop variable");
                }
            }
        }
        self.visit_expr(obj, code);
        code.push_str(&format!(".{}", attr));
        DisplayWrap::Unwrapped
    }

    fn visit_method_call(&mut self, obj: &Expr, method: &str, args: &[Expr], code: &mut String)
                         -> DisplayWrap {
        if let Expr::Var("self") = obj {
            code.push_str("self");
        } else {
            self.visit_expr(obj, code);
        }

        code.push_str(&format!(".{}(", method));
        self._visit_args(args, code);
        code.push_str(")");
        DisplayWrap::Unwrapped
    }

    fn visit_unary(&mut self, op: &str, inner: &Expr, code: &mut String) -> DisplayWrap {
        code.push_str(op);
        self.visit_expr(inner, code);
        DisplayWrap::Unwrapped
    }

    fn visit_binop(&mut self, op: &str, left: &Expr, right: &Expr, code: &mut String)
                   -> DisplayWrap {
        self.visit_expr(left, code);
        code.push_str(&format!(" {} ", op));
        self.visit_expr(right, code);
        DisplayWrap::Unwrapped
    }

    fn visit_group(&mut self, inner: &Expr, code: &mut String) -> DisplayWrap {
        code.push_str("(");
        self.visit_expr(inner, code);
        code.push_str(")");
        DisplayWrap::Unwrapped
    }

    fn visit_array(&mut self, elements: &[Expr], code: &mut String) -> DisplayWrap {
        code.push_str("[");
        for (i, el) in elements.iter().enumerate() {
            if i > 0 {
                code.push_str(", ");
            }
            self.visit_expr(el, code);
        }
        code.push_str("]");
        DisplayWrap::Unwrapped
    }

    fn visit_path(&mut self, path: &[&str], code: &mut String) -> DisplayWrap {
        for (i, part) in path.iter().enumerate() {
            if i > 0 {
                code.push_str("::");
            }
            code.push_str(part);
        }
        DisplayWrap::Unwrapped
    }

    fn visit_var(&mut self, s: &str, code: &mut String) -> DisplayWrap {
        if self.locals.contains(s) {
            code.push_str(s);
        } else {
            code.push_str(&format!("self.{}", s));
        }
        DisplayWrap::Unwrapped
    }

    fn visit_str_lit(&mut self, s: &str, code: &mut String) -> DisplayWrap {
        code.push_str(&format!("\"{}\"", s));
        DisplayWrap::Unwrapped
    }

    fn visit_num_lit(&mut self, s: &str, code: &mut String) -> DisplayWrap {
        code.push_str(s);
        DisplayWrap::Unwrapped
    }

    fn visit_target_single<'t>(&mut self, name: &'t str) -> Vec<&'t str> {
        vec![name]
    }

    fn visit_target<'t>(&mut self, target: &'t Target) -> Vec<&'t str> {
        match *target {
            Target::Name(s) => self.visit_target_single(s),
        }
    }

    /* Helper methods for dealing with whitespace nodes */

    // Combines `flush_ws()` and `prepare_ws()` to handle both trailing whitespace from the
    // preceding literal and leading whitespace from the succeeding literal.
    fn handle_ws(&mut self, ws: &WS) {
        self.flush_ws(ws);
        self.prepare_ws(ws);
    }

    // If the previous literal left some trailing whitespace in `next_ws` and the
    // prefix whitespace suppressor from the given argument, flush that whitespace.
    // In either case, `next_ws` is reset to `None` (no trailing whitespace).
    fn flush_ws(&mut self, ws: &WS) {
        if self.next_ws.is_some() && !ws.0 {
            let val = self.next_ws.unwrap();
            if !val.is_empty() {
                self.writeln(&format!("writer.write_str({:#?})?;", val));
            }
        }
        self.next_ws = None;
    }

    // Sets `skip_ws` to match the suffix whitespace suppressor from the given
    // argument, to determine whether to suppress leading whitespace from the
    // next literal.
    fn prepare_ws(&mut self, ws: &WS) {
        self.skip_ws = ws.1;
    }

    /* Helper methods for writing to internal buffer */

    fn writeln(&mut self, s: &str) {
        if s.is_empty() {
            return;
        }
        if s == "}" {
            self.dedent();
        }
        self.write(s);
        if s.ends_with('{') {
            self.indent();
        }
        self.buf.push('\n');
        self.start = true;
    }

    fn write(&mut self, s: &str) {
        if self.start {
            for _ in 0..(self.indent * 4) {
                self.buf.push(' ');
            }
            self.start = false;
        }
        self.buf.push_str(s);
    }

    fn indent(&mut self) {
        self.indent += 1;
    }

    fn dedent(&mut self) {
        if self.indent == 0 {
            panic!("dedent() called while indentation == 0");
        }
        self.indent -= 1;
    }
}

struct SetChain<'a, T: 'a> where T: cmp::Eq + hash::Hash {
    parent: Option<&'a SetChain<'a, T>>,
    scopes: Vec<HashSet<T>>,
}

impl<'a, T: 'a> SetChain<'a, T> where T: cmp::Eq + hash::Hash {
    fn new() -> SetChain<'a, T> {
        SetChain { parent: None, scopes: vec![HashSet::new()] }
    }
    fn with_parent<'p>(parent: &'p SetChain<T>) -> SetChain<'p, T> {
        SetChain { parent: Some(parent), scopes: vec![HashSet::new()] }
    }
    fn contains(&self, val: T) -> bool {
        self.scopes.iter().rev().any(|set| set.contains(&val)) || match self.parent {
            Some(set) => set.contains(val),
            None => false,
        }
    }
    fn insert(&mut self, val: T) {
        self.scopes.last_mut().unwrap().insert(val);
    }
    fn push(&mut self) {
        self.scopes.push(HashSet::new());
    }
    fn pop(&mut self) {
        self.scopes.pop().unwrap();
        assert!(!self.scopes.is_empty());
    }
}

#[derive(Clone)]
enum AstLevel {
    Top,
    Block,
    Nested,
}

impl Copy for AstLevel {}

#[derive(Clone)]
enum DisplayWrap {
    Wrapped,
    Unwrapped,
}

impl Copy for DisplayWrap {}

type MacroMap<'a> = HashMap<(Option<&'a str>, &'a str), &'a Macro<'a>>;
