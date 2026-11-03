//! Named parameters and a small expression language.
//!
//! Grammar: `+ - * /`, parentheses, unary minus, numbers with optional unit
//! suffix (`mm` default, `cm`, `m`, `in`/`"`, `mil`, `deg` default for angles,
//! `rad`), parameter references, and functions `sqrt abs min max sin cos tan`
//! (trig in degrees). Values are stored in mm / degrees.
//!
//! A fraction of two integer literals binds to a following unit, so `1/8"` and
//! `3/4 in` are fractional inches, and `1 1/2"` is a mixed number (1.5 in). Without
//! a unit (`10/4`) or with a non-literal (`w/2in` = w ÷ 2 in) it is plain division.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use crate::process::{Fit, FitClass};

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

/// A numeric value written as an expression (e.g. `"wall*2 + 0.2"`, `"30"`), with an
/// optional process [`Fit`] applied against the active process profile.
///
/// Serializes as a plain string when there is no fit, else as
/// `{"expr": "...", "fit": {"role": "Hole", "class": "Slide"}}`.
#[derive(Clone, Debug, PartialEq)]
pub struct Expr(pub String, pub Option<Fit>);

impl Serialize for Expr {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match &self.1 {
            None => s.serialize_str(&self.0),
            Some(fit) => {
                use serde::ser::SerializeStruct;
                let mut st = s.serialize_struct("Expr", 2)?;
                st.serialize_field("expr", &self.0)?;
                st.serialize_field("fit", fit)?;
                st.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for Expr {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Plain(String),
            WithFit { expr: String, fit: Option<Fit> },
        }
        Ok(match Repr::deserialize(d)? {
            Repr::Plain(s) => Expr(s, None),
            Repr::WithFit { expr, fit } => Expr(expr, fit),
        })
    }
}

impl Expr {
    pub fn new(s: impl Into<String>) -> Self {
        Expr(s.into(), None)
    }
    /// Same expression with a process fit.
    pub fn fit(mut self, fit: Fit) -> Self {
        self.1 = Some(fit);
        self
    }
    /// Evaluate, applying this expression's fit (if any) against the profile
    /// numbers in `vars` (see `ProcessProfile::inject`).
    pub fn eval(&self, vars: &Values) -> Result<f64, ExprError> {
        let v = eval(&self.0, vars)?;
        Ok(match self.1 {
            Some(f) => crate::process::resolve_in(v, f, vars),
            None => v,
        })
    }
    /// Evaluate with `default` applied when the expression has no fit of its own
    /// (features use this for their sensible default roles).
    pub fn eval_or(&self, vars: &Values, default: Fit) -> Result<f64, ExprError> {
        let v = eval(&self.0, vars)?;
        Ok(crate::process::resolve_in(v, self.1.unwrap_or(default), vars))
    }
    /// Nominal value, ignoring any fit.
    pub fn eval_nominal(&self, vars: &Values) -> Result<f64, ExprError> {
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
        Expr(format_num(v), None)
    }
}
impl From<i32> for Expr {
    fn from(v: i32) -> Self {
        Expr(v.to_string(), None)
    }
}
impl From<&str> for Expr {
    fn from(v: &str) -> Self {
        Expr(v.to_string(), None)
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
    matches!(n, "mm" | "cm" | "m" | "in" | "mil" | "deg" | "rad" | "pi" | "sqrt" | "abs" | "min" | "max" | "sin" | "cos" | "tan" | "hole" | "pin" | "gap" | "tol" | "press" | "snug" | "slide" | "loose")
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
        self.evaluate_seeded(Values::new())
    }

    /// Evaluate on top of `seed` values (e.g. process-profile keys from
    /// `ProcessProfile::inject`, so `hole()`/`pin()` see the active profile).
    pub fn evaluate_seeded(&self, seed: Values) -> Result<Values, ExprError> {
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
        let mut vals = seed;
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
            Ast::Call(f, args) => args.iter().for_each(|a| match a {
                Ast::Var(v) if is_fit_fn(f) && FitClass::from_name(v).is_some() => {}
                _ => a.refs(out),
            }),
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
            Ast::Call(f, args) if is_fit_fn(f) => {
                // hole(x[, class]) pin(x[, class]) gap([class]) tol([class])
                let mut class = FitClass::Slide;
                let mut nums = Vec::new();
                for a in args {
                    match a {
                        Ast::Var(v) if FitClass::from_name(v).is_some() => class = FitClass::from_name(v).unwrap(),
                        _ => nums.push(a.eval(vars)?),
                    }
                }
                let want = if matches!(f.as_str(), "hole" | "pin") { 1 } else { 0 };
                if nums.len() != want {
                    return Err(ExprError::Syntax { pos: 0, msg: format!("{f} takes {want} number(s) and an optional fit class") });
                }
                let x = nums.first().copied().unwrap_or(0.0);
                match f.as_str() {
                    "hole" => crate::process::resolve_in(x, Fit::hole(class), vars),
                    "pin" => crate::process::resolve_in(x, Fit::pin(class), vars),
                    "gap" => crate::process::resolve_in(0.0, Fit::gap(class), vars),
                    _ => crate::process::resolve_in(0.0, Fit::gap(class), vars), // tol = per-side allowance
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

fn is_fit_fn(f: &str) -> bool {
    matches!(f, "hole" | "pin" | "gap" | "tol")
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
    /// An unsigned integer literal at `p` (no whitespace skipping): (value, end).
    fn int_at(&self, p: usize) -> Option<(f64, usize)> {
        let mut e = p;
        while e < self.s.len() && self.s[e].is_ascii_digit() {
            e += 1;
        }
        if e == p || self.s.get(e) == Some(&b'.') {
            return None;
        }
        std::str::from_utf8(&self.s[p..e]).ok()?.parse().ok().map(|v| (v, e))
    }
    /// `/<int>` directly at `p`, then (spaces allowed) a unit: (denominator, unit
    /// factor, end). Used for `1/8"` and the fraction of `1 1/2"`.
    fn fraction_tail(&self, p: usize) -> Option<(f64, f64, usize)> {
        if self.s.get(p) != Some(&b'/') {
            return None;
        }
        let (den, mut e) = self.int_at(p + 1)?;
        if den == 0.0 {
            return None;
        }
        while e < self.s.len() && self.s[e] == b' ' {
            e += 1;
        }
        if self.s.get(e) == Some(&b'"') {
            return Some((den, 25.4, e + 1));
        }
        let st = e;
        while e < self.s.len() && (self.s[e].is_ascii_alphanumeric() || self.s[e] == b'_') {
            e += 1;
        }
        let u = std::str::from_utf8(&self.s[st..e]).ok()?;
        if u.is_empty() || u.as_bytes()[0].is_ascii_digit() || self.s.get(e) == Some(&b'(') {
            return None;
        }
        unit_factor(u).map(|f| (den, f, e))
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
                // Fractions of integer literals bind to a following unit: `1/8"`,
                // `3/4 in`, and the mixed number `1 1/2"` (whole + fraction).
                if txt.bytes().all(|b| b.is_ascii_digit()) {
                    if let Some((den, f, end)) = self.fraction_tail(self.pos) {
                        self.pos = end;
                        return Ok(Ast::Num(v / den * f));
                    }
                    let mut p = self.pos;
                    while p < self.s.len() && self.s[p] == b' ' {
                        p += 1;
                    }
                    if p > self.pos {
                        if let Some((num, e)) = self.int_at(p) {
                            if let Some((den, f, end)) = self.fraction_tail(e) {
                                self.pos = end;
                                return Ok(Ast::Num((v + num / den) * f));
                            }
                        }
                    }
                }
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
        assert!((ev("1rad") - 57.295_779_513_082_32).abs() < 1e-9);
        assert!((ev("90deg") - 90.0).abs() < 1e-12);
        assert!((ev("40mil") - 1.016).abs() < 1e-12);
        assert!(matches!(eval("3furlong", &Values::new()), Err(ExprError::Unit(_))));
    }

    #[test]
    fn fractional_inches() {
        let close = |s: &str, v: f64| assert!((ev(s) - v).abs() < 1e-9, "{s} = {} (want {v})", ev(s));
        close("1/8\"", 3.175);
        close("1/8 in", 3.175);
        close("3/4in", 19.05);
        close("1 1/2\"", 38.1);
        close("1 1/2 in", 38.1);
        close("0.125in", 3.175);
        close("0.125\"", 3.175);
        close("2 * 1/8\" + 1mm", 7.35);
        close("10 3/4\" - 1/4\"", 266.7);
        close("1/2cm", 5.0);
        // unchanged: plain division, unit on a non-literal, decimal fractions
        close("10/4", 2.5);
        close("2 cm + 5mm", 25.0);
        close("1 + 1/2", 1.5);
        let mut v = Values::new();
        v.insert("w".into(), 50.8);
        assert!((eval("w/2in", &v).unwrap() - 1.0).abs() < 1e-12);
        assert!(matches!(eval("1 1/2", &Values::new()), Err(ExprError::Syntax { .. })));
        assert!(matches!(eval("1/0\"", &Values::new()), Err(ExprError::DivZero)));
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
