//! Formula evaluation, conversion tables, coefficients, and checksums.
//! Evaluators translate between raw and physical values using A2L conversion
//! metadata. The expression evaluator follows IEEE-754 arithmetic semantics,
//! while checksum helpers operate on explicitly bounded byte ranges.

use std::collections::HashMap;
use std::fmt;

use indexmap::IndexMap;

use autors_a2l::model::compu::{CompuTab, RationalCoeffs};
use autors_a2l::model::enums::{ChecksumType, ConversionType, DataType};

use crate::error::{Error, Result};

// ============================================================================
// IEvaluator
// ============================================================================

pub trait Evaluator {
    fn eval(&self, x: f64) -> f64;
}

// ============================================================================
// ============================================================================

fn round_ties_even(v: f64) -> f64 {
    v.round_ties_even()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MathFn {
    Sin,
    Cos,
    Tan,
    Asin,
    Acos,
    Atan,
    Atan2,
    Sinh,
    Cosh,
    Tanh,
    Exp,
    Log,
    Log10,
    Pow,
    Sqrt,
    /// C `abs` / `fabs` → `Math.Abs`.
    Abs,
    Floor,
    Ceil,
    Fmod,
    Round,
    Min,
    Max,
}

impl MathFn {
    fn from_name(name: &str) -> Option<(MathFn, usize)> {
        let f = match name.to_ascii_lowercase().as_str() {
            "sin" => (MathFn::Sin, 1),
            "cos" => (MathFn::Cos, 1),
            "tan" => (MathFn::Tan, 1),
            "asin" | "arcsin" => (MathFn::Asin, 1),
            "acos" | "arccos" => (MathFn::Acos, 1),
            "atan" | "arctan" => (MathFn::Atan, 1),
            "atan2" => (MathFn::Atan2, 2),
            "sinh" => (MathFn::Sinh, 1),
            "cosh" => (MathFn::Cosh, 1),
            "tanh" => (MathFn::Tanh, 1),
            "exp" => (MathFn::Exp, 1),
            "log" | "ln" => (MathFn::Log, 1),
            "log10" => (MathFn::Log10, 1),
            "pow" | "power" => (MathFn::Pow, 2),
            "sqrt" => (MathFn::Sqrt, 1),
            "abs" | "fabs" => (MathFn::Abs, 1),
            "floor" => (MathFn::Floor, 1),
            "ceil" => (MathFn::Ceil, 1),
            "fmod" => (MathFn::Fmod, 2),
            "round" => (MathFn::Round, 1),
            "min" | "fmin" => (MathFn::Min, 2),
            "max" | "fmax" => (MathFn::Max, 2),
            _ => return None,
        };
        Some(f)
    }

    fn eval(self, a: f64, b: Option<f64>) -> f64 {
        let b = b.unwrap_or(f64::NAN);
        match self {
            MathFn::Sin => a.sin(),
            MathFn::Cos => a.cos(),
            MathFn::Tan => a.tan(),
            MathFn::Asin => a.asin(),
            MathFn::Acos => a.acos(),
            MathFn::Atan => a.atan(),
            MathFn::Atan2 => a.atan2(b),
            MathFn::Sinh => a.sinh(),
            MathFn::Cosh => a.cosh(),
            MathFn::Tanh => a.tanh(),
            MathFn::Exp => a.exp(),
            MathFn::Log => a.ln(),
            MathFn::Log10 => a.log10(),
            MathFn::Pow => a.powf(b),
            MathFn::Sqrt => a.sqrt(),
            MathFn::Abs => a.abs(),
            MathFn::Floor => a.floor(),
            MathFn::Ceil => a.ceil(),
            MathFn::Fmod => a % b,
            MathFn::Round => round_ties_even(a),
            MathFn::Min => {
                if a.is_nan() || b.is_nan() {
                    f64::NAN
                } else {
                    a.min(b)
                }
            }
            MathFn::Max => {
                if a.is_nan() || b.is_nan() {
                    f64::NAN
                } else {
                    a.max(b)
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Expr {
    Const(f64),
    Var,
    Neg(Box<Expr>),
    Add(Box<Expr>, Box<Expr>),
    Sub(Box<Expr>, Box<Expr>),
    Mul(Box<Expr>, Box<Expr>),
    Div(Box<Expr>, Box<Expr>),
    Mod(Box<Expr>, Box<Expr>),
    Call(MathFn, Box<Expr>, Option<Box<Expr>>),
}

impl Expr {
    fn eval(&self, x: f64) -> f64 {
        match self {
            Expr::Const(v) => *v,
            Expr::Var => x,
            Expr::Neg(e) => -e.eval(x),
            Expr::Add(a, b) => a.eval(x) + b.eval(x),
            Expr::Sub(a, b) => a.eval(x) - b.eval(x),
            Expr::Mul(a, b) => a.eval(x) * b.eval(x),
            Expr::Div(a, b) => a.eval(x) / b.eval(x),
            Expr::Mod(a, b) => a.eval(x) % b.eval(x),
            Expr::Call(f, a, b) => f.eval(a.eval(x), b.as_ref().map(|e| e.eval(x))),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    Num(f64),
    Ident(String),
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    LParen,
    RParen,
    Comma,
}

fn tokenize(text: &str) -> Result<Vec<Tok>> {
    let chars: Vec<char> = text.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        match c {
            ' ' | '\t' | '\n' | '\r' => i += 1,
            '+' => {
                toks.push(Tok::Plus);
                i += 1;
            }
            '-' => {
                toks.push(Tok::Minus);
                i += 1;
            }
            '*' => {
                toks.push(Tok::Star);
                i += 1;
            }
            '/' => {
                toks.push(Tok::Slash);
                i += 1;
            }
            '%' => {
                toks.push(Tok::Percent);
                i += 1;
            }
            '(' => {
                toks.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                toks.push(Tok::RParen);
                i += 1;
            }
            ',' => {
                toks.push(Tok::Comma);
                i += 1;
            }
            _ if c.is_ascii_digit() || c == '.' => {
                let start = i;
                if c == '0' && i + 1 < chars.len() && (chars[i + 1] == 'x' || chars[i + 1] == 'X') {
                    i += 2;
                    let hex_start = i;
                    while i < chars.len() && chars[i].is_ascii_hexdigit() {
                        i += 1;
                    }
                    if i == hex_start {
                        return Err(Error::Formula(format!(
                            "invalid hex literal at offset {start}"
                        )));
                    }
                    let s: String = chars[hex_start..i].iter().collect();
                    let v = u64::from_str_radix(&s, 16)
                        .map_err(|_| Error::Formula(format!("invalid hex literal {s:?}")))?;
                    toks.push(Tok::Num(v as f64));
                    continue;
                }
                while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
                    i += 1;
                }
                if i < chars.len() && (chars[i] == 'e' || chars[i] == 'E') {
                    let mut j = i + 1;
                    if j < chars.len() && (chars[j] == '+' || chars[j] == '-') {
                        j += 1;
                    }
                    if j < chars.len() && chars[j].is_ascii_digit() {
                        i = j;
                        while i < chars.len() && chars[i].is_ascii_digit() {
                            i += 1;
                        }
                    }
                }
                let s: String = chars[start..i].iter().collect();
                let v: f64 = s
                    .parse()
                    .map_err(|_| Error::Formula(format!("invalid number {s:?}")))?;
                toks.push(Tok::Num(v));
            }
            _ if c.is_alphabetic() || c == '_' => {
                let start = i;
                while i < chars.len() && (chars[i].is_alphanumeric() || chars[i] == '_') {
                    i += 1;
                }
                toks.push(Tok::Ident(chars[start..i].iter().collect()));
            }
            _ => {
                return Err(Error::Formula(format!(
                    "unexpected character {c:?} at offset {i}"
                )));
            }
        }
    }
    Ok(toks)
}

struct Compiler<'a> {
    toks: Vec<Tok>,
    pos: usize,
    constants: Option<&'a HashMap<String, f64>>,
    uses_x_var: bool,
    unknowns: Vec<String>,
    const_context: bool,
}

impl<'a> Compiler<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn err(&self, msg: impl Into<String>) -> Error {
        Error::Formula(format!("{} (token {})", msg.into(), self.pos))
    }

    /// expr := additive
    fn parse_expr(&mut self) -> Result<Expr> {
        self.parse_additive()
    }

    /// additive := multiplicative (('+' | '-') multiplicative)*
    fn parse_additive(&mut self) -> Result<Expr> {
        let mut lhs = self.parse_multiplicative()?;
        loop {
            match self.peek() {
                Some(Tok::Plus) => {
                    self.next();
                    let rhs = self.parse_multiplicative()?;
                    lhs = Expr::Add(Box::new(lhs), Box::new(rhs));
                }
                Some(Tok::Minus) => {
                    self.next();
                    let rhs = self.parse_multiplicative()?;
                    lhs = Expr::Sub(Box::new(lhs), Box::new(rhs));
                }
                _ => return Ok(lhs),
            }
        }
    }

    /// multiplicative := unary (('*' | '/' | '%') unary)*
    fn parse_multiplicative(&mut self) -> Result<Expr> {
        let mut lhs = self.parse_unary()?;
        loop {
            match self.peek() {
                Some(Tok::Star) => {
                    self.next();
                    let rhs = self.parse_unary()?;
                    lhs = Expr::Mul(Box::new(lhs), Box::new(rhs));
                }
                Some(Tok::Slash) => {
                    self.next();
                    let rhs = self.parse_unary()?;
                    lhs = Expr::Div(Box::new(lhs), Box::new(rhs));
                }
                Some(Tok::Percent) => {
                    self.next();
                    let rhs = self.parse_unary()?;
                    lhs = Expr::Mod(Box::new(lhs), Box::new(rhs));
                }
                _ => return Ok(lhs),
            }
        }
    }

    /// unary := ('-' | '+') unary | primary
    fn parse_unary(&mut self) -> Result<Expr> {
        match self.peek() {
            Some(Tok::Minus) => {
                self.next();
                match self.parse_unary()? {
                    Expr::Const(v) => Ok(Expr::Const(-v)),
                    e => Ok(Expr::Neg(Box::new(e))),
                }
            }
            Some(Tok::Plus) => {
                self.next();
                self.parse_unary()
            }
            _ => self.parse_primary(),
        }
    }

    /// primary := number | ident | ident '(' args ')' | '(' expr ')'
    fn parse_primary(&mut self) -> Result<Expr> {
        match self.next() {
            Some(Tok::Num(v)) => Ok(Expr::Const(v)),
            Some(Tok::LParen) => {
                let e = self.parse_expr()?;
                match self.next() {
                    Some(Tok::RParen) => Ok(e),
                    _ => Err(self.err("expected ')'")),
                }
            }
            Some(Tok::Ident(name)) => {
                if matches!(self.peek(), Some(Tok::LParen)) {
                    self.next(); // consume '('
                    self.parse_call(&name)
                } else {
                    Ok(self.resolve_ident(&name)?)
                }
            }
            other => Err(self.err(format!("unexpected token {other:?}"))),
        }
    }

    fn parse_call(&mut self, name: &str) -> Result<Expr> {
        let (f, arity) = MathFn::from_name(name)
            .ok_or_else(|| Error::Formula(format!("unknown function {name:?}")))?;
        let a = self.parse_expr()?;
        let b = if arity == 2 {
            match self.next() {
                Some(Tok::Comma) => Some(self.parse_expr()?),
                _ => return Err(self.err(format!("function {name:?} expects 2 arguments"))),
            }
        } else {
            None
        };
        match self.next() {
            Some(Tok::RParen) => Ok(Expr::Call(f, Box::new(a), b.map(Box::new))),
            _ => Err(self.err("expected ')' after function arguments")),
        }
    }

    fn resolve_ident(&mut self, name: &str) -> Result<Expr> {
        if is_x_variable(name) {
            if self.const_context {
                return Err(Error::Formula(format!(
                    "variable {name} not allowed in system constant expression"
                )));
            }
            self.uses_x_var = true;
            return Ok(Expr::Var);
        }
        if let Some(constants) = self.constants {
            if let Some(v) = constants.get(name) {
                return Ok(Expr::Const(*v));
            }
        }
        if !self.unknowns.iter().any(|u| u == name) {
            self.unknowns.push(name.to_string());
        }
        Ok(Expr::Var)
    }
}

fn is_x_variable(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 2 && (b[0] == b'x' || b[0] == b'X') && (b'1'..=b'9').contains(&b[1])
}

fn compile(formula: &str, system_constants: Option<&HashMap<String, f64>>) -> Result<Expr> {
    compile_impl(formula, system_constants, false)
}

fn compile_impl(
    formula: &str,
    system_constants: Option<&HashMap<String, f64>>,
    const_context: bool,
) -> Result<Expr> {
    let toks = tokenize(formula)?;
    if toks.is_empty() {
        return Err(Error::Formula("empty formula".to_string()));
    }
    let mut c = Compiler {
        toks,
        pos: 0,
        constants: system_constants,
        uses_x_var: false,
        unknowns: Vec::new(),
        const_context,
    };
    let expr = c.parse_expr()?;
    if c.pos != c.toks.len() {
        return Err(c.err(format!("trailing tokens ({:?})", c.toks[c.pos])));
    }
    match c.unknowns.len() {
        0 => Ok(expr),
        1 if !c.uses_x_var && !const_context => Ok(expr),
        _ => Err(Error::Formula(format!(
            "unresolved identifier(s): {}",
            c.unknowns.join(", ")
        ))),
    }
}

fn normalize_formula(formula: &str) -> String {
    let mut out = String::with_capacity(formula.len());
    for c in formula.chars() {
        match c {
            ',' => out.push('.'),
            '\t' | '\n' | ' ' => {}
            _ => out.push(c),
        }
    }
    out
}

fn bracket_error(text: &str) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in text.chars().enumerate() {
        if c == '(' {
            depth += 1;
        } else if c == ')' {
            depth -= 1;
        }
        if depth < 0 {
            return Some(i);
        }
    }
    if depth != 0 {
        return Some(text.chars().count().saturating_sub(1));
    }
    None
}

fn parse_double_val(text: &str) -> Option<f64> {
    let t = text.trim();
    if let Ok(v) = t.parse::<f64>() {
        return Some(v);
    }
    let (neg, digits) = match t.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, t),
    };
    let hex = digits
        .strip_prefix("0x")
        .or_else(|| digits.strip_prefix("0X"))?;
    let v = u64::from_str_radix(hex, 16).ok()? as f64;
    Some(if neg { -v } else { v })
}

fn is_valid_ident(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_alphabetic() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_alphanumeric() || c == '_')
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone, PartialEq)]
pub struct A2LFormula {
    phys: Expr,
    raw: Option<Expr>,
}

impl A2LFormula {
    pub fn new(formula: &str, formula_inv: Option<&str>) -> Result<Self> {
        Self::with_constants(formula, formula_inv, None)
    }

    pub fn with_constants(
        formula: &str,
        formula_inv: Option<&str>,
        system_constants: Option<&HashMap<String, f64>>,
    ) -> Result<Self> {
        let phys = compile(formula, system_constants)?;
        let raw = match formula_inv {
            Some(inv) if !inv.is_empty() => Some(compile(inv, system_constants)?),
            _ => None,
        };
        Ok(A2LFormula { phys, raw })
    }

    pub fn to_physical(&self, raw_value: f64) -> f64 {
        self.phys.eval(raw_value)
    }

    pub fn to_raw(&self, data_type: DataType, phys_value: f64) -> f64 {
        let raw = match &self.raw {
            Some(e) => e.eval(phys_value),
            None => f64::NAN,
        };
        if raw.is_nan() {
            return f64::NAN;
        }
        if needs_rounding(data_type) {
            round_ties_even(raw)
        } else {
            raw
        }
    }

    pub fn has_inverse(&self) -> bool {
        self.raw.is_some()
    }
}

fn needs_rounding(data_type: DataType) -> bool {
    !matches!(
        data_type,
        DataType::Float16Ieee | DataType::Float32Ieee | DataType::Float64Ieee
    )
}

impl fmt::Display for A2LFormula {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = self.to_physical(1.0);
        match &self.raw {
            None => write!(f, "FORMULA f(1)={p}"),
            Some(_) => write!(
                f,
                "FORMULA f(1)={p}, FORMULA_INV f_inv({p})={}",
                self.to_raw(DataType::AInt64, p)
            ),
        }
    }
}

// ============================================================================
// ============================================================================

#[derive(Debug, Clone)]
struct PendingEntry {
    formula: String,
    inv: Option<String>,
}

#[derive(Debug, Default)]
pub struct FormulaDict {
    case_insensitive: bool,
    pending_names: IndexMap<String, usize>,
    entries: Vec<PendingEntry>,
    seen_texts: HashMap<String, usize>,
    formulas: IndexMap<String, A2LFormula>,
    key_index: HashMap<String, usize>,
}

impl FormulaDict {
    pub fn new(strict_parsing: bool) -> Self {
        FormulaDict {
            case_insensitive: strict_parsing,
            ..Default::default()
        }
    }

    fn lookup_key(&self, name: &str) -> String {
        if self.case_insensitive {
            name.to_lowercase()
        } else {
            name.to_string()
        }
    }

    pub fn add(&mut self, ident: &str, formula: &str) -> Result<()> {
        let text = normalize_formula(formula);
        if text.is_empty() {
            return Ok(());
        }
        if let Some(pos) = bracket_error(&text) {
            return Err(Error::Formula(format!(
                "unbalanced brackets in formula for {ident:?} at offset {pos}"
            )));
        }
        if let Some(&idx) = self.seen_texts.get(&text) {
            self.pending_names.insert(ident.to_string(), idx);
            return Ok(());
        }
        let idx = self.entries.len();
        self.entries.push(PendingEntry {
            formula: text.clone(),
            inv: None,
        });
        self.seen_texts.insert(text, idx);
        self.pending_names.insert(ident.to_string(), idx);
        Ok(())
    }

    pub fn add_formula(
        &mut self,
        name: &str,
        formula: &str,
        formula_inv: Option<&str>,
    ) -> Result<()> {
        let text = normalize_formula(formula);
        if text.is_empty() {
            return Ok(());
        }
        if let Some(pos) = bracket_error(&text) {
            return Err(Error::Formula(format!(
                "unbalanced brackets in formula for {name:?} at offset {pos}"
            )));
        }
        let inv = match formula_inv {
            Some(s) if !s.is_empty() => {
                let t = normalize_formula(s);
                if t.is_empty() {
                    return Ok(());
                }
                if let Some(pos) = bracket_error(&t) {
                    return Err(Error::Formula(format!(
                        "unbalanced brackets in FORMULA_INV for {name:?} at offset {pos}"
                    )));
                }
                Some(t)
            }
            _ => None,
        };
        if let Some(&idx) = self.seen_texts.get(&text) {
            if self.entries[idx].inv.is_none() {
                self.entries[idx].inv = inv;
            }
            self.pending_names.insert(name.to_string(), idx);
            return Ok(());
        }
        let idx = self.entries.len();
        self.entries.push(PendingEntry {
            formula: text.clone(),
            inv,
        });
        self.seen_texts.insert(text, idx);
        self.pending_names.insert(name.to_string(), idx);
        Ok(())
    }

    pub fn build(&mut self, system_constants: Option<&IndexMap<String, String>>) {
        let constants = system_constants.map(resolve_system_constants);
        let mut compiled: HashMap<usize, A2LFormula> = HashMap::new();
        for (idx, entry) in self.entries.iter().enumerate() {
            if compiled.contains_key(&idx) {
                continue;
            }
            if is_lone_unknown_ident(&entry.formula, constants.as_ref()) {
                continue;
            }
            let inv = entry.inv.as_deref();
            if let Ok(f) = A2LFormula::with_constants(&entry.formula, inv, constants.as_ref()) {
                compiled.insert(idx, f);
            }
        }
        for (name, &idx) in &self.pending_names {
            if let Some(f) = compiled.get(&idx) {
                let key = self.lookup_key(name);
                self.key_index.insert(key, self.formulas.len());
                self.formulas.insert(name.clone(), f.clone());
            }
        }
        self.pending_names.clear();
        self.entries.clear();
        self.seen_texts.clear();
    }

    pub fn get(&self, name: &str) -> Option<&A2LFormula> {
        let key = self.lookup_key(name);
        self.key_index
            .get(&key)
            .and_then(|&i| self.formulas.get_index(i).map(|(_, f)| f))
    }

    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    pub fn len(&self) -> usize {
        self.formulas.len()
    }

    pub fn is_empty(&self) -> bool {
        self.formulas.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &A2LFormula)> {
        self.formulas.iter().map(|(k, v)| (k.as_str(), v))
    }
}

fn is_lone_unknown_ident(text: &str, constants: Option<&HashMap<String, f64>>) -> bool {
    if !is_valid_ident(text) || is_x_variable(text) {
        return false;
    }
    if MathFn::from_name(text).is_some() {
        return false;
    }
    match constants {
        Some(c) => !c.contains_key(text),
        None => true,
    }
}

fn resolve_system_constants(map: &IndexMap<String, String>) -> HashMap<String, f64> {
    let mut resolved: HashMap<String, f64> = HashMap::new();
    for (key, value) in map {
        if !is_valid_ident(key) {
            continue;
        }
        if let Some(v) = parse_double_val(value) {
            resolved.insert(key.clone(), v);
            continue;
        }
        if let Ok(expr) = compile_impl(value, Some(&resolved), true) {
            resolved.insert(key.clone(), expr.eval(f64::NAN));
        }
    }
    resolved
}

// ============================================================================
// ============================================================================

pub trait RationalCoeffsEval {
    fn is_identity(&self) -> bool;
    fn is_rational(&self) -> bool;
    fn coeffs_to_physical(&self, raw_value: f64) -> f64;
    fn coeffs_to_raw(&self, x: f64, data_type: DataType) -> f64;
}

impl RationalCoeffsEval for RationalCoeffs {
    fn is_identity(&self) -> bool {
        self.coeffs == RationalCoeffs::default().coeffs
    }

    fn is_rational(&self) -> bool {
        let c = &self.coeffs;
        c[3] != 0.0 || c[4] != 0.0 || c[5] != 1.0
    }

    fn coeffs_to_physical(&self, raw_value: f64) -> f64 {
        if self.is_identity() || raw_value.is_nan() {
            return raw_value;
        }
        let c = &self.coeffs;
        let (num, num2, num3) = if self.is_rational() {
            (
                c[3] * raw_value - c[0],
                c[4] * raw_value - c[1],
                c[5] * raw_value - c[2],
            )
        } else {
            (-c[0], -c[1], raw_value - c[2])
        };
        if num == 0.0 {
            return -num3 / num2;
        }
        let disc = (num2 * num2 - 4.0 * num * num3).sqrt();
        let denom = 2.0 * num;
        let r1 = (-num2 + disc) / denom;
        let r2 = (-num2 - disc) / denom;
        if r1 != r2 {
            return raw_value;
        }
        r1
    }

    fn coeffs_to_raw(&self, x: f64, data_type: DataType) -> f64 {
        if self.is_identity() || x.is_nan() {
            return x;
        }
        let c = &self.coeffs;
        let mut num = if c[0] != 0.0 {
            c[0] * x * x + c[1] * x + c[2]
        } else {
            c[1] * x + c[2]
        };
        if self.is_rational() {
            num /= if c[3] != 0.0 {
                c[3] * x * x + c[4] * x + c[5]
            } else {
                c[4] * x + c[5]
            };
        }
        if needs_rounding(data_type) {
            round_ties_even(num)
        } else {
            num
        }
    }
}

pub struct TabCoeffs;

impl TabCoeffs {
    pub fn to_physical(conv_type: ConversionType, tab: &CompuTab, raw_value: f64) -> f64 {
        let values = &tab.values;
        match conv_type {
            ConversionType::TAB_INTP => {
                if values.is_empty() {
                    return 0.0;
                }
                let mut prev_k = f64::from(values[0].0);
                let mut prev_v = values[0].1;
                for &(k, v) in &values[1..] {
                    if raw_value < prev_k {
                        return prev_v;
                    }
                    let k = f64::from(k);
                    if raw_value <= k {
                        return (raw_value - prev_k) / (k - prev_k) * (v - prev_v) + prev_v;
                    }
                    prev_k = k;
                    prev_v = v;
                }
                prev_v
            }
            ConversionType::TAB_NOINTP => {
                let key = round_ties_even(raw_value) as i64 as f32;
                for &(k, v) in values {
                    if k == key {
                        return v;
                    }
                }
                tab.default_value_numeric.unwrap_or(f64::NAN)
            }
            _ => 0.0,
        }
    }

    pub fn to_raw(
        conv_type: ConversionType,
        tab: &CompuTab,
        _data_type: DataType,
        physical_value: f64,
    ) -> f64 {
        let values = &tab.values;
        match conv_type {
            ConversionType::TAB_INTP => {
                if values.is_empty() {
                    return 0.0;
                }
                let mut prev_k = f64::from(values[0].0);
                let mut prev_v = values[0].1;
                for &(k, v) in &values[1..] {
                    if physical_value < prev_v {
                        return prev_k;
                    }
                    let k = f64::from(k);
                    if physical_value <= v {
                        return (physical_value - prev_v) / (v - prev_v) * (k - prev_k) + prev_k;
                    }
                    prev_k = k;
                    prev_v = v;
                }
                prev_k
            }
            ConversionType::TAB_NOINTP => {
                for &(k, v) in values {
                    if v == physical_value {
                        return f64::from(k);
                    }
                }
                0.0
            }
            _ => 0.0,
        }
    }
}

// ============================================================================
// ============================================================================

pub struct Checksum;

const fn build_crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut n = 0usize;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 8;
        while k > 0 {
            c = if c & 1 == 1 {
                (c >> 1) ^ 0xEDB8_8320
            } else {
                c >> 1
            };
            k -= 1;
        }
        table[n] = c;
        n += 1;
    }
    table
}

const fn build_crc16_table() -> [u16; 256] {
    let mut table = [0u16; 256];
    let mut n = 0usize;
    while n < 256 {
        let mut crc: u16 = 0;
        let mut c = n as u16;
        let mut k = 0;
        while k < 8 {
            crc = if (crc ^ c) & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xA001
            };
            c >>= 1;
            k += 1;
        }
        table[n] = crc;
        n += 1;
    }
    table
}

static CRC32_TABLE: [u32; 256] = build_crc32_table();
static CRC16_TABLE: [u16; 256] = build_crc16_table();

fn check_range(data: &[u8], from_offset: usize, end: usize) -> Result<()> {
    if from_offset > data.len() || end > data.len() || from_offset > end {
        return Err(Error::Value(format!(
            "checksum range [{from_offset}, {end}) out of bounds (len {})",
            data.len()
        )));
    }
    Ok(())
}

impl Checksum {
    pub fn crc32(data: &[u8], from_offset: usize, length: usize) -> Result<u32> {
        check_range(data, from_offset, from_offset.saturating_add(length))?;
        let mut crc = u32::MAX;
        for i in from_offset..from_offset + length {
            crc = CRC32_TABLE[((crc ^ u32::from(data[i])) & 0xFF) as usize] ^ (crc >> 8);
        }
        Ok(crc ^ 0xFFFF_FFFF)
    }

    pub fn crc16(data: &[u8], from_offset: usize, length: usize) -> Result<u16> {
        check_range(data, from_offset, from_offset.saturating_add(length))?;
        let mut crc: u16 = 0;
        for i in from_offset..from_offset + length {
            crc = CRC16_TABLE[((crc ^ u16::from(data[i])) & 0xFF) as usize] ^ ((crc >> 8) & 0xFF);
        }
        Ok(crc)
    }

    pub fn crc16_ccitt(data: &[u8], from_offset: usize, length: usize) -> Result<u16> {
        check_range(data, from_offset, from_offset.saturating_add(length))?;
        let mut crc: u16 = u16::MAX;
        for &byte in data.iter().skip(from_offset).take(length) {
            let mut b = u16::from(byte) << 8;
            for _ in 0..8 {
                crc = if (crc ^ b) & 0x8000 != 0 {
                    (crc << 1) ^ 0x1021
                } else {
                    crc << 1
                };
                b <<= 1;
            }
        }
        Ok(crc)
    }

    pub fn build_checksum(
        checksum_type: ChecksumType,
        data: &[u8],
        from_offset: usize,
        length: usize,
    ) -> Result<(u32, u8)> {
        match checksum_type {
            ChecksumType::ADD_11 => {
                check_range(data, from_offset, length)?;
                let mut sum: u8 = 0;
                for &b in data.iter().take(length).skip(from_offset) {
                    sum = sum.wrapping_add(b);
                }
                Ok((u32::from(sum), 1))
            }
            ChecksumType::ADD_12 => {
                check_range(data, from_offset, length)?;
                let mut sum: u16 = 0;
                for &b in data.iter().take(length).skip(from_offset) {
                    sum = sum.wrapping_add(u16::from(b));
                }
                Ok((u32::from(sum), 2))
            }
            ChecksumType::ADD_14 => {
                check_range(data, from_offset, length)?;
                let mut sum: u32 = 0;
                for &b in data.iter().take(length).skip(from_offset) {
                    sum += u32::from(b);
                }
                Ok((sum, 4))
            }
            ChecksumType::ADD_22 => {
                check_range(data, from_offset, length)?;
                let mut sum: u16 = 0;
                let mut i = from_offset;
                while i < length {
                    let w = u16::from_le_bytes([data[i], data[i + 1]]);
                    sum = sum.wrapping_add(w);
                    i += 2;
                }
                Ok((u32::from(sum), 2))
            }
            ChecksumType::ADD_24 => {
                check_range(data, from_offset, length)?;
                let mut sum: u32 = 0;
                let mut i = from_offset;
                while i < length {
                    let w = u16::from_le_bytes([data[i], data[i + 1]]);
                    sum += u32::from(w);
                    i += 2;
                }
                Ok((sum, 4))
            }
            ChecksumType::ADD_44 => {
                check_range(data, from_offset, length)?;
                let mut sum: u32 = 0;
                let mut i = from_offset;
                while i < length {
                    let w = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
                    sum = sum.wrapping_add(w);
                    i += 4;
                }
                Ok((sum, 4))
            }
            ChecksumType::CRC_8 => Ok((0, 1)),
            ChecksumType::CRC_16 => Ok((u32::from(Self::crc16(data, from_offset, length)?), 2)),
            ChecksumType::CRC_2_16 => Ok((0, 2)),
            ChecksumType::CRC_16_CITT => {
                Ok((u32::from(Self::crc16_ccitt(data, from_offset, length)?), 2))
            }
            ChecksumType::CRC_32 => Ok((Self::crc32(data, from_offset, length)?, 4)),
            ChecksumType::USER_DEFINED => Ok((0, 4)),
            ChecksumType::NotSet => Err(Error::Value(format!(
                "checksum type not supported: {checksum_type:?}"
            ))),
        }
    }
}

// ============================================================================
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f64 = 1e-12;

    fn eval(formula: &str, x: f64) -> f64 {
        compile(formula, None).unwrap().eval(x)
    }

    #[test]
    fn evaluator_trait() {
        struct Linear;
        impl Evaluator for Linear {
            fn eval(&self, x: f64) -> f64 {
                2.0 * x + 1.0
            }
        }
        assert_eq!(Linear.eval(3.0), 7.0);
    }

    #[test]
    fn normalize_replaces_comma_and_strips_ws() {
        assert_eq!(normalize_formula("X1 * 2,5\n"), "X1*2.5");
        assert_eq!(normalize_formula("a\rb"), "a\rb");
    }

    #[test]
    fn bracket_balance_check() {
        assert_eq!(bracket_error("X1*(2+3)"), None);
        assert_eq!(bracket_error("(X1"), Some(2));
        assert_eq!(bracket_error("X1)"), Some(2));
        assert_eq!(bracket_error("(X1))"), Some(4));
    }

    #[test]
    fn arithmetic_precedence_and_unary() {
        assert_eq!(eval("2+3*4", 0.0), 14.0);
        assert_eq!(eval("(2+3)*4", 0.0), 20.0);
        assert_eq!(eval("10/4", 0.0), 2.5);
        assert_eq!(eval("10%4", 0.0), 2.0);
        assert_eq!(eval("-X1+1", 3.0), -2.0);
        assert_eq!(eval("--X1", 3.0), 3.0);
        assert_eq!(eval("2*-3", 0.0), -6.0);
    }

    #[test]
    fn variable_case_and_suffix_insensitive() {
        assert_eq!(eval("X1*2", 4.0), 8.0);
        assert_eq!(eval("x1*2", 4.0), 8.0);
        assert_eq!(eval("X3+1", 4.0), 5.0);
    }

    #[test]
    fn number_literals() {
        assert_eq!(eval("1.5e3", 0.0), 1500.0);
        assert_eq!(eval("1.5E-3", 0.0), 0.0015);
        assert_eq!(eval(".5+1", 0.0), 1.5);
        assert_eq!(eval("0x1F", 0.0), 31.0);
    }

    #[test]
    fn math_functions() {
        assert!((eval("sin(X1)", 1.0) - 1.0f64.sin()).abs() < EPS);
        assert!((eval("cos(X1)", 1.0) - 1.0f64.cos()).abs() < EPS);
        assert!((eval("sqrt(X1)", 2.0) - std::f64::consts::SQRT_2).abs() < EPS);
        assert_eq!(eval("abs(-3)", 0.0), 3.0);
        assert_eq!(eval("fabs(-3)", 0.0), 3.0);
        assert_eq!(eval("floor(2.7)", 0.0), 2.0);
        assert_eq!(eval("ceil(2.1)", 0.0), 3.0);
        assert!((eval("log(X1)", std::f64::consts::E) - 1.0).abs() < EPS);
        assert_eq!(eval("log10(100)", 0.0), 2.0);
        assert!((eval("exp(1)", 0.0) - std::f64::consts::E).abs() < EPS);
        assert_eq!(eval("pow(2,10)", 0.0), 1024.0);
        assert_eq!(eval("atan2(1,1)", 0.0), std::f64::consts::FRAC_PI_4);
        assert_eq!(eval("fmod(7,3)", 0.0), 1.0);
        assert_eq!(eval("min(2,3)", 0.0), 2.0);
        assert_eq!(eval("max(2,3)", 0.0), 3.0);
        assert_eq!(eval("SIN(0)", 0.0), 0.0);
    }

    #[test]
    fn round_function_is_ties_even() {
        assert_eq!(eval("round(2.5)", 0.0), 2.0);
        assert_eq!(eval("round(3.5)", 0.0), 4.0);
        assert_eq!(eval("round(-2.5)", 0.0), -2.0);
    }

    #[test]
    fn min_max_propagate_nan_under_compatibility_rules() {
        assert!(eval("min(0/0,1)", 0.0).is_nan());
        assert!(eval("max(1,0/0)", 0.0).is_nan());
    }

    #[test]
    fn unknown_ident_aliases_variable() {
        assert_eq!(eval("y*2", 3.0), 6.0);
    }

    #[test]
    fn unknown_ident_with_x_var_fails() {
        assert!(compile("X1+y", None).is_err());
        assert!(compile("a+b", None).is_err());
        assert!(compile("X1*nosuch()", None).is_err());
        assert!(compile("X1^2", None).is_err());
        assert!(compile("X1+", None).is_err());
        assert!(compile("", None).is_err());
    }

    #[test]
    fn system_constants() {
        let mut consts = HashMap::new();
        consts.insert("K".to_string(), 2.5);
        let e = compile("X1*K+1", Some(&consts)).unwrap();
        assert_eq!(e.eval(2.0), 6.0);
        assert!(compile("X1*k", Some(&consts)).is_err());
    }

    #[test]
    fn ieee_semantics_match_expected_contract() {
        assert!(eval("0/0", 0.0).is_nan());
        assert_eq!(eval("1/0", 0.0), f64::INFINITY);
        assert!(eval("sqrt(-1)", 0.0).is_nan());
    }

    // ---------------- A2LFormula ----------------

    #[test]
    fn a2l_formula_to_physical_and_raw() {
        let f = A2LFormula::new("X1*2+1", Some("(X1-1)/2")).unwrap();
        assert_eq!(f.to_physical(4.0), 9.0);
        assert_eq!(f.to_raw(DataType::Float64Ieee, 9.0), 4.0);
        assert!(f.has_inverse());
    }

    #[test]
    fn a2l_formula_to_raw_rounds_for_integer_types() {
        let f = A2LFormula::new("X1*2", Some("X1/2")).unwrap();
        assert_eq!(f.to_raw(DataType::UWord, 5.1), 3.0);
        assert_eq!(f.to_raw(DataType::Float32Ieee, 5.1), 2.55);
        assert_eq!(f.to_raw(DataType::Float16Ieee, 5.1), 2.55);
        assert_eq!(f.to_raw(DataType::SLong, 5.0), 2.0);
        // phys=7 → 3.5 → 4
        assert_eq!(f.to_raw(DataType::SLong, 7.0), 4.0);
    }

    #[test]
    fn a2l_formula_to_raw_without_inverse_is_nan() {
        let f = A2LFormula::new("X1*2", None).unwrap();
        assert!(!f.has_inverse());
        assert!(f.to_raw(DataType::UByte, 4.0).is_nan());
    }

    #[test]
    fn a2l_formula_display() {
        let f = A2LFormula::new("X1*2", Some("X1/2")).unwrap();
        let s = f.to_string();
        assert!(s.contains("f(1)=2"));
        let g = A2LFormula::new("X1*2", None).unwrap();
        assert!(g.to_string().contains("f(1)=2"));
    }

    // ---------------- FormulaDict ----------------

    #[test]
    fn dict_build_and_lookup_case_insensitive_when_strict() {
        let mut d = FormulaDict::new(true);
        d.add_formula("CM1", "X1 * 2", Some("X1 / 2")).unwrap();
        d.build(None);
        let f = d.get("cm1").expect("case-insensitive hit");
        assert_eq!(f.to_physical(3.0), 6.0);
        assert_eq!(f.to_raw(DataType::UWord, 6.0), 3.0);
        assert!(d.contains("CM1"));
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn dict_case_sensitive_when_not_strict() {
        let mut d = FormulaDict::new(false);
        d.add("CM1", "X1*2").unwrap();
        d.build(None);
        assert!(d.get("CM1").is_some());
        assert!(d.get("cm1").is_none());
    }

    #[test]
    fn dict_dedups_identical_formula_text() {
        let mut d = FormulaDict::new(true);
        d.add("A", "X1*2").unwrap();
        d.add("B", "X1*2").unwrap();
        d.add_formula("B", "X1*2", Some("X1/2")).unwrap();
        d.build(None);
        assert_eq!(d.len(), 2);
        assert_eq!(d.get("A").unwrap().to_raw(DataType::Float64Ieee, 4.0), 2.0);
        assert_eq!(d.get("B").unwrap().to_raw(DataType::Float64Ieee, 4.0), 2.0);
    }

    #[test]
    fn dict_skips_empty_lone_ident_and_bad_formulas() {
        let mut d = FormulaDict::new(true);
        d.add("EMPTY", "  ").unwrap();
        d.add("LONE", "Sysco").unwrap();
        d.add("BAD", "X1**2").unwrap();
        d.add("GOOD", "X1+1").unwrap();
        d.build(None);
        assert!(!d.contains("EMPTY"));
        assert!(!d.contains("LONE"));
        assert!(!d.contains("BAD"));
        assert_eq!(d.get("GOOD").unwrap().to_physical(1.0), 2.0);
        d.build(None);
        assert_eq!(d.len(), 1);
    }

    #[test]
    fn dict_rejects_unbalanced_brackets() {
        let mut d = FormulaDict::new(true);
        assert!(d.add("X", "(X1+1").is_err());
        assert!(d.add_formula("X", "X1+1", Some("(X1")).is_err());
    }

    #[test]
    fn dict_with_system_constants() {
        let mut consts = IndexMap::new();
        consts.insert("GAIN".to_string(), "0.5".to_string());
        consts.insert("GAIN2".to_string(), "GAIN*2".to_string());
        consts.insert("HEX".to_string(), "0x10".to_string());
        consts.insert("1BAD".to_string(), "5".to_string());
        consts.insert("UNRESOLVED".to_string(), "NOPE*2".to_string());
        let mut d = FormulaDict::new(true);
        d.add("K1", "X1*GAIN").unwrap();
        d.add("K2", "X1*GAIN2").unwrap();
        d.add("K3", "X1+HEX").unwrap();
        d.build(Some(&consts));
        assert_eq!(d.get("K1").unwrap().to_physical(4.0), 2.0);
        assert_eq!(d.get("K2").unwrap().to_physical(4.0), 4.0);
        assert_eq!(d.get("K3").unwrap().to_physical(1.0), 17.0);
    }

    // ---------------- RationalCoeffs ----------------

    fn linear_coeffs(factor: f64, offset: f64) -> RationalCoeffs {
        let mut c = RationalCoeffs::default();
        c.coeffs[1] = 1.0 / factor;
        c.coeffs[2] = -offset / factor;
        c
    }

    #[test]
    fn coeffs_identity_passthrough() {
        let c = RationalCoeffs::default();
        assert!(c.is_identity());
        assert!(!c.is_rational());
        assert_eq!(c.coeffs_to_physical(3.5), 3.5);
        assert_eq!(c.coeffs_to_raw(2.5, DataType::UWord), 2.5);
        assert!(c.coeffs_to_physical(f64::NAN).is_nan());
    }

    #[test]
    fn coeffs_linear_round_trip() {
        // factor=2, offset=4 → b=0.5, c=-2
        let c = linear_coeffs(2.0, 4.0);
        assert!(!c.is_identity());
        assert!(!c.is_rational());
        assert_eq!(c.coeffs_to_raw(10.0, DataType::Float64Ieee), 3.0);
        assert_eq!(c.coeffs_to_physical(3.0), 10.0);
        assert!((c.factor() - 2.0).abs() < EPS);
        assert!((c.offset() - 4.0).abs() < EPS);
    }

    #[test]
    fn coeffs_rat_func_forward() {
        // COEFFS 1 2 3 4 5 6 → y=(x²+2x+3)/(4x²+5x+6)
        let c = RationalCoeffs {
            coeffs: [1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
        };
        assert!(c.is_rational());
        assert_eq!(c.coeffs_to_raw(2.0, DataType::Float64Ieee), 11.0 / 32.0);
        assert_eq!(c.coeffs_to_physical(0.34375), 0.34375);
    }

    #[test]
    fn coeffs_to_raw_rounds_ties_even() {
        // factor=0.5 → b=2, c=0;to_raw(1.25)=2.5
        let c = linear_coeffs(0.5, 0.0);
        assert_eq!(c.coeffs_to_raw(1.25, DataType::UWord), 2.0);
        assert_eq!(c.coeffs_to_raw(1.75, DataType::UWord), 4.0); // 3.5 → 4
        assert_eq!(c.coeffs_to_raw(1.25, DataType::Float64Ieee), 2.5);
    }

    #[test]
    fn coeffs_inverse_linear_when_numerator_quadratic_coeff_zero() {
        let c = RationalCoeffs {
            coeffs: [0.0, 0.5, -2.0, 0.0, 0.0, 1.0],
        };
        assert_eq!(c.coeffs_to_physical(3.0), 10.0);
    }

    // ---------------- TabCoeffs ----------------

    fn intp_tab() -> CompuTab {
        CompuTab {
            values: vec![(0.0, 0.0), (10.0, 100.0)],
            ..Default::default()
        }
    }

    #[test]
    fn tab_intp_to_physical() {
        let t = intp_tab();
        assert_eq!(
            TabCoeffs::to_physical(ConversionType::TAB_INTP, &t, -5.0),
            0.0
        );
        assert_eq!(
            TabCoeffs::to_physical(ConversionType::TAB_INTP, &t, 0.0),
            0.0
        );
        assert_eq!(
            TabCoeffs::to_physical(ConversionType::TAB_INTP, &t, 5.0),
            50.0
        );
        assert_eq!(
            TabCoeffs::to_physical(ConversionType::TAB_INTP, &t, 10.0),
            100.0
        );
        assert_eq!(
            TabCoeffs::to_physical(ConversionType::TAB_INTP, &t, 20.0),
            100.0
        );
    }

    #[test]
    fn tab_intp_to_raw() {
        let t = intp_tab();
        assert_eq!(
            TabCoeffs::to_raw(ConversionType::TAB_INTP, &t, DataType::UWord, -1.0),
            0.0
        );
        assert_eq!(
            TabCoeffs::to_raw(ConversionType::TAB_INTP, &t, DataType::UWord, 50.0),
            5.0
        );
        assert_eq!(
            TabCoeffs::to_raw(ConversionType::TAB_INTP, &t, DataType::UWord, 150.0),
            10.0
        );
    }

    #[test]
    fn tab_nointp_to_physical() {
        let mut t = intp_tab();
        t.default_value_numeric = Some(-1.0);
        assert_eq!(
            TabCoeffs::to_physical(ConversionType::TAB_NOINTP, &t, 10.0),
            100.0
        );
        assert_eq!(
            TabCoeffs::to_physical(ConversionType::TAB_NOINTP, &t, 5.0),
            -1.0
        );
        assert_eq!(
            TabCoeffs::to_physical(ConversionType::TAB_NOINTP, &t, 4.5),
            -1.0
        );
        assert_eq!(
            TabCoeffs::to_physical(ConversionType::TAB_NOINTP, &t, 9.5),
            100.0
        );
        let t2 = intp_tab();
        assert!(TabCoeffs::to_physical(ConversionType::TAB_NOINTP, &t2, 5.0).is_nan());
    }

    #[test]
    fn tab_nointp_to_raw() {
        let t = intp_tab();
        assert_eq!(
            TabCoeffs::to_raw(ConversionType::TAB_NOINTP, &t, DataType::UWord, 100.0),
            10.0
        );
        assert_eq!(
            TabCoeffs::to_raw(ConversionType::TAB_NOINTP, &t, DataType::UWord, 55.0),
            0.0
        );
        assert_eq!(TabCoeffs::to_physical(ConversionType::LINEAR, &t, 5.0), 0.0);
        assert_eq!(
            TabCoeffs::to_raw(ConversionType::LINEAR, &t, DataType::UWord, 5.0),
            0.0
        );
    }

    // ---------------- Checksum ----------------

    #[test]
    fn crc32_standard_check_value() {
        // CRC-32/ISO-HDLC("123456789") = 0xCBF43926
        assert_eq!(Checksum::crc32(b"123456789", 0, 9).unwrap(), 0xCBF4_3926);
    }

    #[test]
    fn crc16_arc_check_value() {
        assert_eq!(Checksum::crc16(b"123456789", 0, 9).unwrap(), 0xBB3D);
    }

    #[test]
    fn crc16_ccitt_false_check_value() {
        // CRC-16/CCITT-FALSE("123456789") = 0x29B1
        assert_eq!(Checksum::crc16_ccitt(b"123456789", 0, 9).unwrap(), 0x29B1);
    }

    #[test]
    fn crc_with_offset() {
        let data = b"XX123456789YY";
        assert_eq!(Checksum::crc32(data, 2, 9).unwrap(), 0xCBF4_3926);
        assert_eq!(Checksum::crc16(data, 2, 9).unwrap(), 0xBB3D);
    }

    #[test]
    fn checksum_out_of_bounds_errors() {
        assert!(Checksum::crc32(b"12", 0, 3).is_err());
        assert!(Checksum::crc16(b"12", 3, 0).is_err());
    }

    #[test]
    fn build_checksum_add_family() {
        let data = [0xFFu8, 0x01, 0x02, 0x03, 0x04];
        // ADD_11:(0xFF+1+2+3+4) mod 256 = 0x09
        assert_eq!(
            Checksum::build_checksum(ChecksumType::ADD_11, &data, 0, 5).unwrap(),
            (9, 1)
        );
        assert_eq!(
            Checksum::build_checksum(ChecksumType::ADD_12, &data, 0, 5).unwrap(),
            (265, 2)
        );
        assert_eq!(
            Checksum::build_checksum(ChecksumType::ADD_14, &data, 0, 5).unwrap(),
            (265, 4)
        );
        assert_eq!(
            Checksum::build_checksum(ChecksumType::ADD_22, &data, 0, 4).unwrap(),
            (0x0501, 2)
        );
        assert_eq!(
            Checksum::build_checksum(ChecksumType::ADD_24, &data, 0, 4).unwrap(),
            (0x0501, 4)
        );
        assert_eq!(
            Checksum::build_checksum(ChecksumType::ADD_44, &data, 0, 4).unwrap(),
            (0x0302_01FF, 4)
        );
    }

    #[test]
    fn build_checksum_add_family_offset_quirk() {
        let data = [1u8, 2, 3, 4];
        assert_eq!(
            Checksum::build_checksum(ChecksumType::ADD_11, &data, 1, 3).unwrap(),
            (5, 1)
        );
    }

    #[test]
    fn build_checksum_crc_and_special_types() {
        assert_eq!(
            Checksum::build_checksum(ChecksumType::CRC_32, b"123456789", 0, 9).unwrap(),
            (0xCBF4_3926, 4)
        );
        assert_eq!(
            Checksum::build_checksum(ChecksumType::CRC_16, b"123456789", 0, 9).unwrap(),
            (0xBB3D, 2)
        );
        assert_eq!(
            Checksum::build_checksum(ChecksumType::CRC_16_CITT, b"123456789", 0, 9).unwrap(),
            (0x29B1, 2)
        );
        assert_eq!(
            Checksum::build_checksum(ChecksumType::CRC_8, b"abc", 0, 3).unwrap(),
            (0, 1)
        );
        assert_eq!(
            Checksum::build_checksum(ChecksumType::CRC_2_16, b"abc", 0, 3).unwrap(),
            (0, 2)
        );
        assert_eq!(
            Checksum::build_checksum(ChecksumType::USER_DEFINED, b"abc", 0, 3).unwrap(),
            (0, 4)
        );
        assert!(Checksum::build_checksum(ChecksumType::NotSet, b"abc", 0, 3).is_err());
    }
}
