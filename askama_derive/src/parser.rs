// rustfmt doesn't do a very good job on nom parser invocations.
#![cfg_attr(rustfmt, rustfmt_skip)]

use nom;
use std::str;

#[derive(Debug)]
pub enum Expr<'a> {
    NumLit(&'a str),
    StrLit(&'a str),
    Var(&'a str),
    Path(Vec<&'a str>),
    Array(Vec<Expr<'a>>),
    Attr(Box<Expr<'a>>, &'a str),
    Filter(&'a str, Vec<Expr<'a>>),
    Unary(&'a str, Box<Expr<'a>>),
    BinOp(&'a str, Box<Expr<'a>>, Box<Expr<'a>>),
    Group(Box<Expr<'a>>),
    MethodCall(Box<Expr<'a>>, &'a str, Vec<Expr<'a>>),
}

#[derive(Debug)]
pub enum MatchVariant<'a> {
    Path(Vec<&'a str>),
    Name(&'a str),
    NumLit(&'a str),
    StrLit(&'a str),
}

#[derive(Debug)]
pub enum MatchParameter<'a> {
    Name(&'a str),
    NumLit(&'a str),
    StrLit(&'a str),
}

#[derive(Debug)]
pub enum Target<'a> {
    Name(&'a str),
}

#[derive(Clone, Copy, Debug)]
pub struct WS(pub bool, pub bool);

#[derive(Debug)]
pub struct Macro<'a> {
    pub ws1: WS,
    pub args: Vec<&'a str>,
    pub nodes: Vec<Node<'a>>,
    pub ws2: WS,
}

#[derive(Debug)]
pub enum Node<'a> {
    Lit(&'a str, &'a str, &'a str),
    Comment(WS),
    Expr(WS, Expr<'a>),
    Call(WS, Option<& 'a str>, &'a str, Vec<Expr<'a>>),
    LetDecl(WS, Target<'a>),
    Let(WS, Target<'a>, Expr<'a>),
    Cond(Vec<(WS, Option<Expr<'a>>, Vec<Node<'a>>)>, WS),
    Match(WS, Expr<'a>, Option<&'a str>, Vec<When<'a>>, WS),
    Loop(WS, Target<'a>, Expr<'a>, Vec<Node<'a>>, WS),
    Extends(Expr<'a>),
    BlockDef(WS, &'a str, Vec<Node<'a>>, WS),
    Include(WS, &'a str),
    Import(WS, &'a str, &'a str),
    Macro(&'a str, Macro<'a>),
}

pub type Cond<'a> = (WS, Option<Expr<'a>>, Vec<Node<'a>>);
pub type When<'a> = (WS, Option<MatchVariant<'a>>, Vec<MatchParameter<'a>>, Vec<Node<'a>>);

fn split_ws_parts(s: &[u8]) -> Node {
    if s.is_empty() {
        let rs = str::from_utf8(s).unwrap();
        return Node::Lit(rs, rs, rs);
    }
    let is_ws = |c: &u8| {
        *c != b' ' && *c != b'\t' && *c != b'\r' && *c != b'\n'
    };
    let start = s.iter().position(&is_ws);
    let res = if start.is_none() {
            (s, &s[0..0], &s[0..0])
        } else {
            let start = start.unwrap();
            let end = s.iter().rposition(&is_ws);
            if end.is_none() {
                (&s[..start], &s[start..], &s[0..0])
            } else {
                let end = end.unwrap();
                (&s[..start], &s[start..end + 1], &s[end + 1..])
            }
        };
    Node::Lit(str::from_utf8(res.0).unwrap(),
              str::from_utf8(res.1).unwrap(),
              str::from_utf8(res.2).unwrap())
}

#[derive(Debug)]
enum ContentState {
    Any,
    Brace(usize),
    End(usize),
}

fn take_content(i: &[u8]) -> Result<(&[u8], Node), nom::Err<&[u8]>> {
    use parser::ContentState::*;
    let mut state = Any;
    for (idx, c) in i.iter().enumerate() {
        state = match (state, *c) {
            (Any, b'{') => Brace(idx),
            (Brace(start), b'{') |
            (Brace(start), b'%') |
            (Brace(start), b'#') => End(start),
            (Any, _) |
            (Brace(_), _) => Any,
            (End(_), _) => panic!("cannot happen"),
        };
        if let End(_) = state {
            break;
        }
    }
    match state {
        Any |
        Brace(_) => Ok((&i[..0], split_ws_parts(i))),
        End(0) => Err(nom::Err::Error(error_position!(i, nom::ErrorKind::Custom(0)))),
        End(start) => Ok((&i[start..], split_ws_parts(&i[..start]))),
    }
}

fn identifier(input: &[u8]) -> Result<(&[u8], &str), nom::Err<&[u8]>> {
    if !nom::is_alphabetic(input[0]) && input[0] != b'_' {
        return Err(nom::Err::Error(error_position!(input, nom::ErrorKind::Custom(0))));
    }
    for (i, ch) in input.iter().enumerate() {
        if i == 0 || nom::is_alphanumeric(*ch) || *ch == b'_' {
            continue;
        }
        return Ok((&input[i..],
                   str::from_utf8(&input[..i]).unwrap()));
    }
    Ok((&input[1..], str::from_utf8(&input[..1]).unwrap()))
}

named!(num_lit<&str>, map!(nom::digit,
    |s| str::from_utf8(s).unwrap()
));

named!(expr_num_lit<Expr>, map!(num_lit,
    |s| Expr::NumLit(s)
));

named!(expr_array_lit<Expr>, do_parse!(
    ws!(tag_s!("[")) >>
    first: expr_any >>
    rest: many0!(do_parse!(
        ws!(tag_s!(",")) >>
        part: expr_any >>
        (part)
    )) >>
    ws!(tag_s!("]")) >>
    ({
        let mut elements = vec![first];
        elements.extend(rest);
        Expr::Array(elements)
    })
));

named!(variant_num_lit<MatchVariant>, map!(num_lit,
    |s| MatchVariant::NumLit(s)
));

named!(param_num_lit<MatchParameter>, map!(num_lit,
    |s| MatchParameter::NumLit(s)
));

named!(expr_str_lit<Expr>, map!(
    delimited!(char!('"'), take_until!("\""), char!('"')),
    |s| Expr::StrLit(str::from_utf8(s).unwrap())
));

named!(variant_str_lit<MatchVariant>, map!(
    delimited!(char!('"'), is_not!("\""), char!('"')),
    |s| MatchVariant::StrLit(str::from_utf8(s).unwrap())
));

named!(param_str_lit<MatchParameter>, map!(
    delimited!(char!('"'), is_not!("\""), char!('"')),
    |s| MatchParameter::StrLit(str::from_utf8(s).unwrap())
));

named!(expr_var<Expr>, map!(identifier,
    |s| Expr::Var(s))
);

named!(expr_path<Expr>, do_parse!(
    start: call!(identifier) >>
    rest: many1!(do_parse!(
        tag_s!("::") >>
        part: identifier >>
        (part)
    )) >>
    ({
        let mut path = vec![start];
        path.extend(rest);
        Expr::Path(path)
    })
));

named!(variant_path<MatchVariant>, do_parse!(
    start: call!(identifier) >>
    rest: many1!(do_parse!(
        tag_s!("::") >>
        part: identifier >>
        (part)
    )) >>
    ({
        let mut path = vec![start];
        path.extend(rest);
        MatchVariant::Path(path)
    })
));

named!(target_single<Target>, map!(identifier,
    |s| Target::Name(s)
));

named!(variant_name<MatchVariant>, map!(identifier,
    |s| MatchVariant::Name(s)
));

named!(param_name<MatchParameter>, map!(identifier,
    |s| MatchParameter::Name(s)
));

named!(arguments<Vec<Expr>>, do_parse!(
    tag_s!("(") >>
    args: opt!(do_parse!(
        arg0: ws!(expr_any) >>
        args: many0!(do_parse!(
            tag_s!(",") >>
            argn: ws!(expr_any) >>
            (argn)
        )) >>
        ({
           let mut res = vec![arg0];
           res.extend(args);
           res
        })
    )) >>
    tag_s!(")") >>
    (args.unwrap_or_default())
));

named!(parameters<Vec<&str>>, do_parse!(
    tag_s!("(") >>
    vals: opt!(do_parse!(
        arg0: ws!(identifier) >>
        args: many0!(do_parse!(
            tag_s!(",") >>
            argn: ws!(identifier) >>
            (argn)
        )) >>
        ({
            let mut res = vec![arg0];
            res.extend(args);
            res
        })
    )) >>
    tag_s!(")") >>
    (vals.unwrap_or_default())
));

named!(with_parameters<Vec<MatchParameter>>, do_parse!(
    tag_s!("with") >>
    ws!(tag_s!("(")) >>
    vals: opt!(do_parse!(
        arg0: ws!(match_parameter) >>
        args: many0!(do_parse!(
            tag_s!(",") >>
            argn: ws!(match_parameter) >>
            (argn)
        )) >>
        ({
            let mut res = vec![arg0];
            res.extend(args);
            res
        })
    )) >>
    tag_s!(")") >>
    (vals.unwrap_or_default())
));

named!(expr_group<Expr>, map!(
    delimited!(char!('('), expr_any, char!(')')),
    |s| Expr::Group(Box::new(s))
));

named!(expr_single<Expr>, alt!(
    expr_num_lit |
    expr_str_lit |
    expr_path |
    expr_array_lit |
    expr_var |
    expr_group
));

named!(match_variant<MatchVariant>, alt!(
    variant_path |
    variant_name |
    variant_num_lit |
    variant_str_lit
));

named!(match_parameter<MatchParameter>, alt!(
    param_name |
    param_num_lit |
    param_str_lit
));

named!(attr<(&str, Option<Vec<Expr>>)>, do_parse!(
    tag_s!(".") >>
    attr: alt!(num_lit | identifier) >>
    args: opt!(arguments) >>
    (attr, args)
));

named!(expr_attr<Expr>, do_parse!(
    obj: expr_single >>
    attrs: many0!(attr) >>
    ({
        let mut res = obj;
        for (aname, args) in attrs {
            res = if args.is_some() {
                Expr::MethodCall(Box::new(res), aname, args.unwrap())
            } else {
                Expr::Attr(Box::new(res), aname)
            };
        }
        res
    })
));

named!(filter<(&str, Option<Vec<Expr>>)>, do_parse!(
    tag_s!("|") >>
    fname: identifier >>
    args: opt!(arguments) >>
    (fname, args)
));

named!(expr_filtered<Expr>, do_parse!(
    obj: expr_attr >>
    filters: many0!(filter) >>
    ({
       let mut res = obj;
       for (fname, args) in filters {
           res = Expr::Filter(fname, {
               let mut args = match args {
                   Some(inner) => inner,
                   None => Vec::new(),
               };
               args.insert(0, res);
               args
           });
       }
       res
    })
));

named!(expr_unary<Expr>, do_parse!(
    op: opt!(alt!(tag_s!("!") | tag_s!("-"))) >>
    expr: expr_filtered >>
    (match op {
        Some(op) => Expr::Unary(str::from_utf8(op).unwrap(), Box::new(expr)),
        None => expr,
    })
));

macro_rules! expr_prec_layer {
    ( $name:ident, $inner:ident, $( $op:expr ),* ) => {
        named!($name<Expr>, do_parse!(
            left: $inner >>
            op_and_right: opt!(pair!(ws!(alt!($( tag_s!($op) )|*)), expr_any)) >>
            (match op_and_right {
                Some((op, right)) => Expr::BinOp(
                    str::from_utf8(op).unwrap(), Box::new(left), Box::new(right)
                ),
                None => left,
            })
        ));
    }
}

expr_prec_layer!(expr_muldivmod, expr_unary, "*", "/", "%");
expr_prec_layer!(expr_addsub, expr_muldivmod, "+", "-");
expr_prec_layer!(expr_shifts, expr_addsub, ">>", "<<");
expr_prec_layer!(expr_band, expr_shifts, "&");
expr_prec_layer!(expr_bxor, expr_band, "^");
expr_prec_layer!(expr_bor, expr_bxor, "|");
expr_prec_layer!(expr_compare, expr_bor,
    "==", "!=", ">=", ">", "<=", "<"
);
expr_prec_layer!(expr_and, expr_compare, "&&");
expr_prec_layer!(expr_any, expr_and, "||");

named!(expr_node<Node>, do_parse!(
    tag_s!("{{") >>
    pws: opt!(tag_s!("-")) >>
    expr: ws!(expr_any) >>
    nws: opt!(tag_s!("-")) >>
    tag_s!("}}") >>
    (Node::Expr(WS(pws.is_some(), nws.is_some()), expr))
));

named!(block_call<Node>, do_parse!(
    pws: opt!(tag_s!("-")) >>
    ws!(tag_s!("call")) >>
    scope: opt!(do_parse!(
        scope: ws!(identifier) >>
        ws!(tag_s!("::")) >>
        (scope)
    )) >>
    name: ws!(identifier) >>
    args: ws!(arguments) >>
    nws: opt!(tag_s!("-")) >>
    (Node::Call(WS(pws.is_some(), nws.is_some()), scope, name, args))
));

named!(cond_if<Expr>, do_parse!(
    ws!(tag_s!("if")) >>
    cond: ws!(expr_any) >>
    (cond)
));

named!(cond_block<Cond>, do_parse!(
    tag_s!("{%") >>
    pws: opt!(tag_s!("-")) >>
    ws!(tag_s!("else")) >>
    cond: opt!(cond_if) >>
    nws: opt!(tag_s!("-")) >>
    tag_s!("%}") >>
    block: parse_template >>
    (WS(pws.is_some(), nws.is_some()), cond, block)
));

named!(block_if<Node>, do_parse!(
    pws1: opt!(tag_s!("-")) >>
    cond: ws!(cond_if) >>
    nws1: opt!(tag_s!("-")) >>
    tag_s!("%}") >>
    block: parse_template >>
    elifs: many0!(cond_block) >>
    tag_s!("{%") >>
    pws2: opt!(tag_s!("-")) >>
    ws!(tag_s!("endif")) >>
    nws2: opt!(tag_s!("-")) >>
    ({
       let mut res = Vec::new();
       res.push((WS(pws1.is_some(), nws1.is_some()), Some(cond), block));
       res.extend(elifs);
       Node::Cond(res, WS(pws2.is_some(), nws2.is_some()))
    })
));

named!(match_else_block<When>, do_parse!(
    tag_s!("{%") >>
    pws: opt!(tag_s!("-")) >>
    ws!(tag_s!("else")) >>
    nws: opt!(tag_s!("-")) >>
    tag_s!("%}") >>
    block: parse_template >>
    (WS(pws.is_some(), nws.is_some()), None, vec![], block)
));

named!(when_block<When>, do_parse!(
    tag_s!("{%") >>
    pws: opt!(tag_s!("-")) >>
    ws!(tag_s!("when")) >>
    variant: ws!(match_variant) >>
    params: opt!(ws!(with_parameters)) >>
    nws: opt!(tag_s!("-")) >>
    tag_s!("%}") >>
    block: parse_template >>
    (WS(pws.is_some(), nws.is_some()), Some(variant), params.unwrap_or_default(), block)
));

named!(block_match<Node>, do_parse!(
    pws1: opt!(tag_s!("-")) >>
    ws!(tag_s!("match")) >>
    expr: ws!(expr_any) >>
    nws1: opt!(tag_s!("-")) >>
    tag_s!("%}") >>
    inter: opt!(take_content) >>
    arms: many1!(when_block) >>
    else_arm: opt!(match_else_block) >>
    ws!(tag_s!("{%")) >>
    pws2: opt!(tag_s!("-")) >>
    ws!(tag_s!("endmatch")) >>
    nws2: opt!(tag_s!("-")) >>
    ({
        let mut arms = arms;
        if let Some(arm) = else_arm {
            arms.push(arm);
        }
        let inter = match inter {
            Some(Node::Lit(lws, val, rws)) => {
                assert!(val.is_empty(),
                        "only whitespace allowed between match and first when, found {}", val);
                assert!(rws.is_empty(),
                        "only whitespace allowed between match and first when, found {}", rws);
                Some(lws)
            },
            None => None,
            _ => panic!("only literals allowed between match and first when"),
        };
        Node::Match(
            WS(pws1.is_some(), nws1.is_some()),
            expr,
            inter,
            arms,
            WS(pws2.is_some(), nws2.is_some()),
        )
    })
));

named!(block_let<Node>, do_parse!(
    pws: opt!(tag_s!("-")) >>
    ws!(tag_s!("let")) >>
    var: ws!(target_single) >>
    val: opt!(do_parse!(
        ws!(tag_s!("=")) >>
        val: ws!(expr_any) >>
        (val)
    )) >>
    nws: opt!(tag_s!("-")) >>
    (if val.is_some() {
        Node::Let(WS(pws.is_some(), nws.is_some()), var, val.unwrap())
    } else {
        Node::LetDecl(WS(pws.is_some(), nws.is_some()), var)
    })
));

named!(block_for<Node>, do_parse!(
    pws1: opt!(tag_s!("-")) >>
    ws!(tag_s!("for")) >>
    var: ws!(target_single) >>
    ws!(tag_s!("in")) >>
    iter: ws!(expr_any) >>
    nws1: opt!(tag_s!("-")) >>
    tag_s!("%}") >>
    block: parse_template >>
    tag_s!("{%") >>
    pws2: opt!(tag_s!("-")) >>
    ws!(tag_s!("endfor")) >>
    nws2: opt!(tag_s!("-")) >>
    (Node::Loop(WS(pws1.is_some(), nws1.is_some()),
                var, iter, block,
                WS(pws2.is_some(), nws2.is_some())))
));

named!(block_extends<Node>, do_parse!(
    ws!(tag_s!("extends")) >>
    name: ws!(expr_str_lit) >>
    (Node::Extends(name))
));

named!(block_block<Node>, do_parse!(
    pws1: opt!(tag_s!("-")) >>
    ws!(tag_s!("block")) >>
    name: ws!(identifier) >>
    nws1: opt!(tag_s!("-")) >>
    tag_s!("%}") >>
    contents: parse_template >>
    tag_s!("{%") >>
    pws2: opt!(tag_s!("-")) >>
    ws!(tag_s!("endblock")) >>
    opt!(ws!(tag_s!(name))) >>
    nws2: opt!(tag_s!("-")) >>
    (Node::BlockDef(WS(pws1.is_some(), nws1.is_some()),
                    name, contents,
                    WS(pws2.is_some(), nws2.is_some())))
));

named!(block_include<Node>, do_parse!(
    pws: opt!(tag_s!("-")) >>
    ws!(tag_s!("include")) >>
    name: ws!(expr_str_lit) >>
    nws: opt!(tag_s!("-")) >>
    (Node::Include(WS(pws.is_some(), nws.is_some()), match name {
        Expr::StrLit(s) => s,
        _ => panic!("include path must be a string literal"),
    }))
));

named!(block_import<Node>, do_parse!(
    pws: opt!(tag_s!("-")) >>
    ws!(tag_s!("import")) >>
    name: ws!(expr_str_lit) >>
    ws!(tag_s!("as")) >>
    scope: ws!(identifier) >>
    nws: opt!(tag_s!("-")) >>
    (Node::Import(WS(pws.is_some(), nws.is_some()), match name {
        Expr::StrLit(s) => s,
        _ => panic!("import path must be a string literal"),
    }, scope))
));

named!(block_macro<Node>, do_parse!(
    pws1: opt!(tag_s!("-")) >>
    ws!(tag_s!("macro")) >>
    name: ws!(identifier) >>
    params: ws!(parameters) >>
    nws1: opt!(tag_s!("-")) >>
    tag_s!("%}") >>
    contents: parse_template >>
    tag_s!("{%") >>
    pws2: opt!(tag_s!("-")) >>
    ws!(tag_s!("endmacro")) >>
    nws2: opt!(tag_s!("-")) >>
    (Node::Macro(
         name,
         Macro {
             ws1: WS(pws1.is_some(), nws1.is_some()),
             args: params,
             nodes: contents,
             ws2: WS(pws2.is_some(), nws2.is_some())
         }
    ))
));

named!(block_node<Node>, dbg_dmp!(do_parse!(
    tag_s!("{%") >>
    contents: alt!(
        block_call |
        block_let |
        block_if |
        block_for |
        block_match |
        block_extends |
        block_include |
        block_import |
        block_block |
        block_macro
    ) >>
    tag_s!("%}") >>
    (contents)
)));

named!(block_comment<Node>, do_parse!(
    tag_s!("{#") >>
    pws: opt!(tag_s!("-")) >>
    inner: take_until_s!("#}") >>
    tag_s!("#}") >>
    (Node::Comment(WS(pws.is_some(), inner.len() > 1 && inner[inner.len() - 1] == b'-')))
));

named!(parse_template<Vec<Node>>, many0!(alt!(
    take_content |
    block_comment |
    expr_node |
    block_node
)));

pub fn parse(src: &str) -> Vec<Node> {
    match parse_template(src.as_bytes()) {
        Ok((left, res)) => {
            if !left.is_empty() {
                let s = str::from_utf8(left).unwrap();
                panic!("unable to parse template:\n\n{:?}", s);
            } else {
                res
            }
        },
        Err(nom::Err::Error(err)) => panic!("problems parsing template source: {:?}", err),
        Err(nom::Err::Failure(err)) => panic!("problems parsing template source: {:?}", err),
        Err(nom::Err::Incomplete(_)) => panic!("parsing incomplete"),
    }
}

#[cfg(test)]
mod tests {
    fn check_ws_split(s: &str, res: &(&str, &str, &str)) {
        let node = super::split_ws_parts(s.as_bytes());
        match node {
            super::Node::Lit(lws, s, rws) => {
                assert_eq!(lws, res.0);
                assert_eq!(s, res.1);
                assert_eq!(rws, res.2);
            },
            _ => { panic!("fail"); },
        }
    }
    #[test]
    fn test_ws_splitter() {
        check_ws_split("", &("", "", ""));
        check_ws_split("a", &("", "a", ""));
        check_ws_split("\ta", &("\t", "a", ""));
        check_ws_split("b\n", &("", "b", "\n"));
        check_ws_split(" \t\r\n", &(" \t\r\n", "", ""));
    }
    #[test]
    #[should_panic]
    fn test_invalid_block() {
        super::parse("{% extend \"blah\" %}");
    }
}
