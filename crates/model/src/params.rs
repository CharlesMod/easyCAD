//! Named parameters and a small expression language.
//!
//! Grammar: `+ - * /`, parentheses, unary minus, numbers with optional unit
//! suffix (`mm` default, `cm`, `m`, `in`/`"`, `mil`, `deg` default for angles,
//! `rad`), parameter references, and functions `sqrt abs min max sin cos tan`
//! (trig in degrees). Values are stored in mm / degrees.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ExprError {
    #[error("syntax error at {pos}: {msg}")]
    Syntax { pos: usize, msg: String },
    #[error("unknown parameter `{0}`")]
    Unknown(String),
    #[error("unknown unit `{0}`")]
    Unit(String),
    #[error("unknown function `{0}`")]
    Function(String),
    #[error("parameter cycle: {0}")]
    Cycle(String),
    #[error("invalid parameter name `{0}`")]
    BadName(String),
    #[error("division by zero")]
    DivZero,
}

/// A numeric value written as an expression (e.g. `"wall*2 + 0.2"`, `"30"`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Expr(pub String);

impl Expr {
    pub fn new(s: impl Into<String>) -> Self {
        Expr(s.into())
    }
    pub fn eval(&self, vars: &Values) -> Result<f64, ExprError> {
        eval(&self.0, vars)
    }
    /// Parameter names referenced by this expression.
    pub fn refs(&self) -> Result<Vec<String>, ExprError> {
        let ast = parse(&self.0)?;
        let mut out = Vec::new();
        ast.refs(&mut out);
        Ok(out)
    }
}

impl From<f64> for Expr {
    fn from(v: f64) -> Self {
        Expr(format_num(v))
    }
}
impl From<i32> for Expr {
    fn from(v: i32) -> Self {
        Expr(v.to_string())
    }
}
impl From<&str> for Expr {
    fn from(v: &str) -> Self {
        Expr(v.to_string())
    }
}

fn format_num(v: f64) -> String {
    let s = format!("{v}");
    if s.contains('e') {
        format!("{v:.12}")
    } else {
        s
    }
}

/// Evaluated parameter values.
pub type Values = HashMap<String, f64>;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    pub expr: Expr,
    #[serde(default)]
    pub comment: String,
}

/// Ordered list of user parameters.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Params {
    pub list: Vec<Param>,
}

pub fn valid_name(n: &str) -> bool {
    let mut c = n.chars();
    matches!(c.next(), Some(ch) if ch.is_ascii_alphabetic() || ch == '_')
        && c.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
        && !is_reserved(n)
}

fn is_reserved(n: &str) -> bool {
    matches!(n, "mm" | "cm" | "m" | "in" | "mil" | "deg" | "rad" | "pi" | "sqrt" | "abs" | "min" | "max" | "sin" | "cos" | "tan")
}

impl Params {
    /// Insert or replace a parameter.
    pub fn set(&mut self, name: &str, expr: impl Into<Expr>) -> Result<(), ExprError> {
        if !valid_name(name) {
            return Err(ExprError::BadName(name.into()));
        }
        let expr = expr.into();
        parse(&expr.0)?;
        match self.list.iter_mut().find(|p| p.name == name) {
            Some(p) => p.expr = expr,
            None => self.list.push(Param { name: name.into(), expr, comment: String::new() }),
        }
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&Param> {
        self.list.iter().find(|p| p.name == name)
    }

    pub fn remove(&mut self, name: &str) {
        self.list.retain(|p| p.name != name);
    }

    /// Evaluate all parameters in dependency order; reports cycles and unknown refs.
    pub fn evaluate(&self) -> Result<Values, ExprError> {
        let mut asts = BTreeMap::new();
        for p in &self.list {
            asts.insert(p.name.as_str(), parse(&p.expr.0)?);
        }
        #[derive(Clone, Copy, PartialEq)]
        enum St {
            Visiting,
            Done,
        }
        fn visit<'a>(
            n: &'a str,
            asts: &BTreeMap<&'a str, Ast>,
            st: &mut HashMap<&'a str, St>,
            stack: &mut Vec<&'a str>,
            vals: &mut Values,
        ) -> Result<(), ExprError> {
            match st.get(n) {
                Some(St::Done) => return Ok(()),
                Some(St::Visiting) => {
                    let start = stack.iter().position(|s| *s == n).unwrap_or(0);
                    let mut cyc: Vec<&str> = stack[start..].to_vec();
                    cyc.push(n);
                    return Err(ExprError::Cycle(cyc.join(" -> ")));
                }
                None => {}
            }
            let (key, ast) = asts.get_key_value(n).ok_or_else(|| ExprError::Unknown(n.into()))?;
            st.insert(key, St::Visiting);
            stack.push(key);
            let mut refs = Vec::new();
            ast.refs(&mut refs);
            for r in &refs {
                if !asts.contains_key(r.as_str()) {
                    return Err(ExprError::Unknown(r.clone()));
                }
                let (rk, _) = asts.get_key_value(r.as_str()).unwrap();
                visit(rk, asts, st, stack, vals)?;
            }
            let v = ast.eval(vals)?;
            vals.insert(key.to_string(), v);
            stack.pop();
            st.insert(key, St::Done);
            Ok(())
        }
        let mut st = HashMap::new();
        let mut vals = Values::new();
        for p in &self.list {
            visit(p.name.as_str(), &asts, &mut st, &mut Vec::new(), &mut vals)?;
        }
        Ok(vals)
    }
}

/// Evaluate an expression string against parameter values.
pub fn eval(src: &str, vars: &Values) -> Result<f64, ExprError> {
    parse(src)?.eval(vars)
}

#[derive(Clone, Debug, PartialEq)]
enum Ast {
    Num(f64),
    Var(String),
    Neg(Box<Ast>),
    Bin(char, Box<Ast>, Box<Ast>),
    Call(String, Vec<Ast>),
}

impl Ast {
    fn refs(&self, out: &mut Vec<String>) {
        match self {
            Ast::Num(_) => {}
            Ast::Var(v) => {
                if !out.contains(v) {
                    out.push(v.clone())
                }
            }
            Ast::Neg(a) => a.refs(out),
            Ast::Bin(_, a, b) => {
                a.refs(out);
                b.refs(out);
            }
            Ast::Call(_, args) => args.iter().for_each(|a| a.refs(out)),
        }
    }

    fn eval(&self, vars: &Values) -> Result<f64, ExprError> {
        Ok(match self {
            Ast::Num(v) => *v,
            Ast::Var(n) => *vars.get(n).ok_or_else(|| ExprError::Unknown(n.clone()))?,
            Ast::Neg(a) => -a.eval(vars)?,
            Ast::Bin(op, a, b) => {
                let (x, y) = (a.eval(vars)?, b.eval(vars)?);
                match op {
                    '+' => x + y,
                    '-' => x - y,
                    '*' => x * y,
                    '/' => {
                        if y == 0.0 {
                            return Err(ExprError::DivZero);
                        }
                        x / y
                    }
                    _ => unreachable!(),
                }
            }
            Ast::Call(f, args) => {
                let a: Vec<f64> = args.iter().map(|a| a.eval(vars)).collect::<Result<_, _>>()?;
                let arity = |n: usize| {
                    if a.len() == n {
                        Ok(())
                    } else {
                        Err(ExprError::Syntax { pos: 0, msg: format!("{f} takes {n} argument(s)") })
                    }
                };
                match f.as_str() {
                    "sqrt" => {
                        arity(1)?;
                        a[0].sqrt()
                    }
                    "abs" => {
                        arity(1)?;
                        a[0].abs()
                    }
                    "sin" => {
                        arity(1)?;
                        a[0].to_radians().sin()
                    }
                    "cos" => {
                        arity(1)?;
                        a[0].to_radians().cos()
                    }
                    "tan" => {
                        arity(1)?;
                        a[0].to_radians().tan()
                    }
                    "min" | "max" if !a.is_empty() => {
                        let it = a.iter().cloned();
                        if f == "min" {
                            it.fold(f64::INFINITY, f64::min)
                        } else {
                            it.fold(f64::NEG_INFINITY, f64::max)
                        }
                    }
                    _ => return Err(ExprError::Function(f.clone())),
                }
            }
        })
    }
}

fn unit_factor(u: &str) -> Option<f64> {
    Some(match u {
        "mm" | "deg" => 1.0,
        "cm" => 10.0,
        "m" => 1000.0,
        "in" | "\"" => 25.4,
        "mil" => 0.0254,
        "rad" => 180.0 / std::f64::consts::PI,
        _ => return None,
    })
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
}

fn parse(src: &str) -> Result<Ast, ExprError> {
    let mut p = Parser { s: src.as_bytes(), pos: 0 };
    let a = p.expr()?;
    p.ws();
    if p.pos != p.s.len() {
        return Err(p.err("unexpected input"));
    }
    Ok(a)
}

impl Parser<'_> {
    fn err(&self, m: &str) -> ExprError {
        ExprError::Syntax { pos: self.pos, msg: m.into() }
    }
    fn ws(&mut self) {
        while self.pos < self.s.len() && self.s[self.pos].is_ascii_whitespace() {
            self.pos += 1;
        }
    }
    fn peek(&mut self) -> Option<u8> {
        self.ws();
        self.s.get(self.pos).copied()
    }
    fn expr(&mut self) -> Result<Ast, ExprError> {
        let mut a = self.term()?;
        while let Some(c @ (b'+' | b'-')) = self.peek() {
            self.pos += 1;
            let b = self.term()?;
            a = Ast::Bin(c as char, Box::new(a), Box::new(b));
        }
        Ok(a)
    }
    fn term(&mut self) -> Result<Ast, ExprError> {
        let mut a = self.unary()?;
        while let Some(c @ (b'*' | b'/')) = self.peek() {
            self.pos += 1;
            let b = self.unary()?;
            a = Ast::Bin(c as char, Box::new(a), Box::new(b));
        }
        Ok(a)
    }
    fn unary(&mut self) -> Result<Ast, ExprError> {
        match self.peek() {
            Some(b'-') => {
                self.pos += 1;
                Ok(Ast::Neg(Box::new(self.unary()?)))
            }
            Some(b'+') => {
                self.pos += 1;
                self.unary()
            }
            _ => self.atom(),
        }
    }
    fn ident(&mut self) -> String {
        let st = self.pos;
        while self.pos < self.s.len() && (self.s[self.pos].is_ascii_alphanumeric() || self.s[self.pos] == b'_') {
            self.pos += 1;
        }
        String::from_utf8_lossy(&self.s[st..self.pos]).into_owned()
    }
    fn atom(&mut self) -> Result<Ast, ExprError> {
        match self.peek() {
            Some(b'(') => {
                self.pos += 1;
                let a = self.expr()?;
                if self.peek() != Some(b')') {
                    return Err(self.err("expected `)`"));
                }
                self.pos += 1;
                Ok(a)
            }
            Some(c) if c.is_ascii_digit() || c == b'.' => {
                let st = self.pos;
                while self.pos < self.s.len() && (self.s[self.pos].is_ascii_digit() || self.s[self.pos] == b'.') {
                    self.pos += 1;
                }
                // exponent
                if self.pos < self.s.len() && (self.s[self.pos] == b'e' || self.s[self.pos] == b'E') {
                    let save = self.pos;
                    self.pos += 1;
                    if self.pos < self.s.len() && (self.s[self.pos] == b'+' || self.s[self.pos] == b'-') {
                        self.pos += 1;
                    }
                    if self.pos < self.s.len() && self.s[self.pos].is_ascii_digit() {
                        while self.pos < self.s.len() && self.s[self.pos].is_ascii_digit() {
                            self.pos += 1;
                        }
                    } else {
                        self.pos = save;
                    }
                }
                let txt = std::str::from_utf8(&self.s[st..self.pos]).unwrap();
                let v: f64 = txt.parse().map_err(|_| ExprError::Syntax { pos: st, msg: format!("bad number `{txt}`") })?;
                // unit suffix directly after the number (spaces allowed)
                let save = self.pos;
                self.ws();
                if self.pos < self.s.len() && self.s[self.pos] == b'"' {
                    self.pos += 1;
                    return Ok(Ast::Num(v * 25.4));
                }
                if self.pos < self.s.len() && self.s[self.pos].is_ascii_alphabetic() {
                    let u = self.ident();
                    return match unit_factor(&u) {
                        Some(f) => Ok(Ast::Num(v * f)),
                        None => Err(ExprError::Unit(u)),
                    };
                }
                self.pos = save;
                Ok(Ast::Num(v))
            }
            Some(c) if c.is_ascii_alphabetic() || c == b'_' => {
                let id = self.ident();
                if self.peek() == Some(b'(') {
                    self.pos += 1;
                    let mut args = Vec::new();
                    if self.peek() != Some(b')') {
                        loop {
                            args.push(self.expr()?);
                            match self.peek() {
                                Some(b',') => self.pos += 1,
                                Some(b')') => break,
                                _ => return Err(self.err("expected `,` or `)`")),
                            }
                        }
                    }
                    self.pos += 1;
                    return Ok(Ast::Call(id, args));
                }
                if id == "pi" {
                    return Ok(Ast::Num(std::f64::consts::PI));
                }
                Ok(Ast::Var(id))
            }
            _ => Err(self.err("expected a number, name or `(`")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(s: &str) -> f64 {
        eval(s, &Values::new()).unwrap()
    }

    #[test]
    fn arithmetic_and_precedence() {
        assert_eq!(ev("1 + 2 * 3"), 7.0);
        assert_eq!(ev("(1 + 2) * 3"), 9.0);
        assert_eq!(ev("-2 * -3"), 6.0);
        assert_eq!(ev("10 / 4 - 1"), 1.5);
        assert_eq!(ev("-(3 - 5)"), 2.0);
        assert_eq!(ev("1e1 + 2.5e-1"), 10.25);
        assert_eq!(ev("8 - 2 - 1"), 5.0);
    }

    #[test]
    fn units() {
        assert!((ev("1in") - 25.4).abs() < 1e-12);
        assert!((ev("2 cm + 5mm") - 25.0).abs() < 1e-12);
        assert!((ev("pi rad") - 180.0).abs() < 1e-9);
        assert!((ev("90deg") - 90.0).abs() < 1e-12);
        assert!((ev("40mil") - 1.016).abs() < 1e-12);
        assert!(matches!(eval("3furlong", &Values::new()), Err(ExprError::Unit(_))));
    }

    #[test]
    fn functions() {
        assert_eq!(ev("sqrt(16) + abs(-1)"), 5.0);
        assert_eq!(ev("max(1, 7, 3) - min(4, 2)"), 5.0);
        assert!((ev("sin(30)") - 0.5).abs() < 1e-12);
    }

    #[test]
    fn errors() {
        assert!(matches!(eval("1 +", &Values::new()), Err(ExprError::Syntax { .. })));
        assert!(matches!(eval("(1", &Values::new()), Err(ExprError::Syntax { .. })));
        assert!(matches!(eval("x", &Values::new()), Err(ExprError::Unknown(_))));
        assert!(matches!(eval("1/0", &Values::new()), Err(ExprError::DivZero)));
        assert!(matches!(eval("foo(1)", &Values::new()), Err(ExprError::Function(_))));
    }

    #[test]
    fn params_with_references() {
        let mut p = Params::default();
        p.set("wall", "2").unwrap();
        p.set("inner", "width - 2*wall").unwrap(); // forward reference is fine
        p.set("width", "80mm").unwrap();
        p.set("lid_h", "-(-5)").unwrap();
        let v = p.evaluate().unwrap();
        assert_eq!(v["inner"], 76.0);
        assert_eq!(v["lid_h"], 5.0);
        assert_eq!(Expr::from("inner/2 + wall").eval(&v).unwrap(), 40.0);
        assert_eq!(Expr::from("inner + wall*width").refs().unwrap(), vec!["inner", "wall", "width"]);
    }

    #[test]
    fn cycle_detection() {
        let mut p = Params::default();
        p.set("a", "b + 1").unwrap();
        p.set("b", "c * 2").unwrap();
        p.set("c", "a").unwrap();
        match p.evaluate() {
            Err(ExprError::Cycle(c)) => assert!(c.contains("a -> b -> c -> a"), "{c}"),
            other => panic!("{other:?}"),
        }
        let mut p = Params::default();
        p.set("s", "s").unwrap();
        assert!(matches!(p.evaluate(), Err(ExprError::Cycle(_))));
    }

    #[test]
    fn unknown_reference_and_bad_names() {
        let mut p = Params::default();
        p.set("a", "nope * 2").unwrap();
        assert_eq!(p.evaluate(), Err(ExprError::Unknown("nope".into())));
        assert!(p.set("2x", "1").is_err());
        assert!(p.set("mm", "1").is_err());
    }
}
