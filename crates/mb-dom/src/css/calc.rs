// `calc()` / `min()` / `max()` / `clamp()` (compat round 3 step 5a).
//
// A math function is parsed into a normalised tree: linear parts are one
// [`CalcSum`] (a coefficient per unit — `*` and `/` by numbers are folded
// in at parse time), and only `min()` / `max()` (`clamp(a, b, c)` =
// `max(a, min(b, c))`) stay as nodes. Font and viewport units are folded
// to px by the style pass ([`Calc::resolve`]); what layout sees is `px` +
// `%` and evaluates against the property's own percentage basis
// ([`Calc::eval`]). A math function with no unit is a number.
//
// Invalid at parse time (the declaration drops, as in Chrome): a length
// times a length, division by zero or by a length, a number added to a
// length, a non-finite result, `+`/`-` without whitespace around them,
// nesting deeper than [`super::MAX_VAR_NESTING`] (Robustness Rule 2 — the
// parser recurses per level).

use cssparser::{Parser as CssParser, Token};

use super::{MAX_VAR_NESTING, Unit, Value};
use crate::style::MAX_CSS_LENGTH;

/// A linear combination of units: `px + percent% + em·1em + …`.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct CalcSum {
    pub px: f32,
    pub percent: f32,
    pub em: f32,
    pub rem: f32,
    pub vw: f32,
    pub vh: f32,
    pub vmin: f32,
    pub vmax: f32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Calc {
    Sum(CalcSum),
    Min(Vec<Calc>),
    Max(Vec<Calc>),
    /// `k × node` for a non-linear node (`2 * min(…)`).
    Scaled(f32, Box<Calc>),
    /// `a + b` where one side is non-linear (`10px + min(…)`).
    Plus(Box<Calc>, Box<Calc>),
}

/// What the style pass resolves units against, in px: `em` (the element's
/// font size, or the parent's for `font-size` itself), `rem`, and one
/// `vw` / `vh` (1% of the page viewport).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Units {
    pub em: f32,
    pub rem: f32,
    pub vw: f32,
    pub vh: f32,
}

impl CalcSum {
    fn fields(&self) -> [f32; 8] {
        [
            self.px,
            self.percent,
            self.em,
            self.rem,
            self.vw,
            self.vh,
            self.vmin,
            self.vmax,
        ]
    }

    fn map(self, f: impl Fn(f32) -> f32) -> CalcSum {
        CalcSum {
            px: f(self.px),
            percent: f(self.percent),
            em: f(self.em),
            rem: f(self.rem),
            vw: f(self.vw),
            vh: f(self.vh),
            vmin: f(self.vmin),
            vmax: f(self.vmax),
        }
    }

    fn plus(self, other: CalcSum) -> CalcSum {
        CalcSum {
            px: self.px + other.px,
            percent: self.percent + other.percent,
            em: self.em + other.em,
            rem: self.rem + other.rem,
            vw: self.vw + other.vw,
            vh: self.vh + other.vh,
            vmin: self.vmin + other.vmin,
            vmax: self.vmax + other.vmax,
        }
    }

    fn is_finite(&self) -> bool {
        self.fields().iter().all(|v| v.is_finite())
    }

    /// Only px (or nothing): a constant.
    fn constant(&self) -> Option<f32> {
        self.fields()[1..]
            .iter()
            .all(|v| *v == 0.0)
            .then_some(self.px)
    }

    fn unit(value: f32, unit: Unit) -> CalcSum {
        let mut sum = CalcSum::default();
        match unit {
            Unit::Px => sum.px = value,
            Unit::Percent => sum.percent = value,
            Unit::Em => sum.em = value,
            Unit::Rem => sum.rem = value,
            Unit::Ch => sum.em = value * 0.5,
            Unit::Pt => sum.px = value * 4.0 / 3.0,
            Unit::Vw => sum.vw = value,
            Unit::Vh => sum.vh = value,
            Unit::Vmin => sum.vmin = value,
            Unit::Vmax => sum.vmax = value,
        }
        sum
    }
}

impl Calc {
    /// Fold the font and viewport units to px; `%` stays for layout.
    pub fn resolve(&self, units: &Units) -> Calc {
        match self {
            Calc::Sum(s) => {
                // Each term saturated, an unused unit skipped: an infinite
                // `em` (a `1e39px` font) must not turn 0 × ∞ or ∞ − ∞ into
                // NaN (layout fuzzer).
                let term = |k: f32, unit: f32| {
                    if k == 0.0 { 0.0 } else { saturate(k * unit) }
                };
                let px = saturate(
                    saturate(s.px)
                        + term(s.em, units.em)
                        + term(s.rem, units.rem)
                        + term(s.vw, units.vw)
                        + term(s.vh, units.vh)
                        + term(s.vmin, units.vw.min(units.vh))
                        + term(s.vmax, units.vw.max(units.vh)),
                );
                Calc::Sum(CalcSum {
                    px,
                    percent: saturate(s.percent),
                    ..CalcSum::default()
                })
            }
            Calc::Min(args) => min_max(true, args.iter().map(|a| a.resolve(units)).collect()),
            Calc::Max(args) => min_max(false, args.iter().map(|a| a.resolve(units)).collect()),
            Calc::Scaled(k, node) => scale(*k, node.resolve(units)),
            Calc::Plus(a, b) => plus(a.resolve(units), b.resolve(units)),
        }
    }

    /// The value in px with `%` of `basis` (units already resolved). A
    /// non-finite result is 0.
    pub fn eval(&self, basis: f32) -> f32 {
        let value = match self {
            Calc::Sum(s) => s.px + s.percent / 100.0 * basis,
            Calc::Min(args) => args
                .iter()
                .map(|a| a.eval(basis))
                .fold(f32::INFINITY, f32::min),
            Calc::Max(args) => args
                .iter()
                .map(|a| a.eval(basis))
                .fold(f32::NEG_INFINITY, f32::max),
            Calc::Scaled(k, node) => k * node.eval(basis),
            Calc::Plus(a, b) => a.eval(basis) + b.eval(basis),
        };
        if value.is_finite() { value } else { 0.0 }
    }

    /// Whether any part is a percentage (needs a basis at layout).
    pub fn has_percent(&self) -> bool {
        self.any(&|s| s.percent != 0.0)
    }

    /// Whether resolving it reads the viewport height (`vh`, `vmin`,
    /// `vmax`) — the view cache must then key on the height.
    pub fn uses_viewport_height(&self) -> bool {
        self.any(&|s| s.vh != 0.0 || s.vmin != 0.0 || s.vmax != 0.0)
    }

    fn any(&self, test: &dyn Fn(&CalcSum) -> bool) -> bool {
        match self {
            Calc::Sum(s) => test(s),
            Calc::Min(args) | Calc::Max(args) => args.iter().any(|a| a.any(test)),
            Calc::Scaled(_, node) => node.any(test),
            Calc::Plus(a, b) => a.any(test) || b.any(test),
        }
    }

    /// As a stored value: a single unit becomes a plain `Length`.
    pub fn into_value(self) -> Value {
        if let Calc::Sum(s) = &self {
            let units = [
                (s.px, Unit::Px),
                (s.percent, Unit::Percent),
                (s.em, Unit::Em),
                (s.rem, Unit::Rem),
                (s.vw, Unit::Vw),
                (s.vh, Unit::Vh),
                (s.vmin, Unit::Vmin),
                (s.vmax, Unit::Vmax),
            ];
            let mut nonzero = units.iter().filter(|(v, _)| *v != 0.0);
            match (nonzero.next(), nonzero.next()) {
                (None, _) => return Value::Length(0.0, Unit::Px),
                (Some(&(v, unit)), None) => return Value::Length(v, unit),
                _ => {}
            }
        }
        Value::Calc(Box::new(self))
    }
}

/// Within the engine's length range (`style::MAX_CSS_LENGTH`); NaN → 0.
fn saturate(v: f32) -> f32 {
    if v.is_nan() {
        0.0
    } else {
        v.clamp(-MAX_CSS_LENGTH, MAX_CSS_LENGTH)
    }
}

fn scale(k: f32, node: Calc) -> Calc {
    match node {
        Calc::Sum(s) => Calc::Sum(s.map(|v| v * k)),
        Calc::Scaled(j, inner) => Calc::Scaled(k * j, inner),
        Calc::Plus(a, b) => Calc::Plus(Box::new(scale(k, *a)), Box::new(scale(k, *b))),
        other if k == 1.0 => other,
        other => Calc::Scaled(k, Box::new(other)),
    }
}

fn plus(a: Calc, b: Calc) -> Calc {
    match (a, b) {
        (Calc::Sum(x), Calc::Sum(y)) => Calc::Sum(x.plus(y)),
        (a, b) => Calc::Plus(Box::new(a), Box::new(b)),
    }
}

/// `min(…)` / `max(…)`; all-constant arguments fold to one px sum.
fn min_max(is_min: bool, args: Vec<Calc>) -> Calc {
    let constants: Option<Vec<f32>> = args
        .iter()
        .map(|a| match a {
            Calc::Sum(s) => s.constant(),
            _ => None,
        })
        .collect();
    match constants {
        Some(values) if !values.is_empty() => {
            let px = if is_min {
                values.into_iter().fold(f32::INFINITY, f32::min)
            } else {
                values.into_iter().fold(f32::NEG_INFINITY, f32::max)
            };
            Calc::Sum(CalcSum {
                px,
                ..CalcSum::default()
            })
        }
        _ if args.len() == 1 => args.into_iter().next().expect("one argument"),
        _ if is_min => Calc::Min(args),
        _ => Calc::Max(args),
    }
}

// ---- parsing ---------------------------------------------------------

/// A math function's value while parsing: a number or a length.
#[derive(Debug, Clone)]
enum Term {
    Number(f32),
    Length(Calc),
}

/// Whether `name` is a math function this module parses.
pub(super) fn is_math_function(name: &str) -> bool {
    matches!(
        name.to_ascii_lowercase().as_str(),
        "calc" | "-webkit-calc" | "-moz-calc" | "min" | "max" | "clamp"
    )
}

/// Parse the inside of math function `name` (the parser is in its nested
/// block) into a value: `Number`, `Length` or `Calc`. `None` = invalid.
pub(super) fn parse_math_function(name: &str, input: &mut CssParser<'_>) -> Option<Value> {
    let term = parse_function_body(&name.to_ascii_lowercase(), input, 1).ok()?;
    match term {
        Term::Number(n) => n.is_finite().then_some(Value::Number(n)),
        Term::Length(calc) => Some(calc.into_value()),
    }
}

type Parsed<T> = Result<T, ()>;

fn parse_function_body(name: &str, input: &mut CssParser<'_>, depth: usize) -> Parsed<Term> {
    match name {
        "calc" | "-webkit-calc" | "-moz-calc" => {
            let term = parse_sum(input, depth)?;
            input.expect_exhausted().map_err(|_| ())?;
            Ok(term)
        }
        "min" | "max" | "clamp" => {
            let mut args = Vec::new();
            loop {
                args.push(parse_sum(input, depth)?);
                match input.next() {
                    Err(_) => break,
                    Ok(Token::Comma) => {}
                    Ok(_) => return Err(()),
                }
            }
            if name == "clamp" {
                let [low, value, high]: [Term; 3] = args.try_into().map_err(|_| ())?;
                let inner = combine_min_max(true, vec![value, high])?;
                combine_min_max(false, vec![low, inner])
            } else {
                combine_min_max(name == "min", args)
            }
        }
        _ => Err(()),
    }
}

fn combine_min_max(is_min: bool, args: Vec<Term>) -> Parsed<Term> {
    if args.is_empty() {
        return Err(());
    }
    if args.iter().all(|a| matches!(a, Term::Number(_))) {
        let values = args.into_iter().map(|a| match a {
            Term::Number(n) => n,
            Term::Length(_) => 0.0,
        });
        let n = if is_min {
            values.fold(f32::INFINITY, f32::min)
        } else {
            values.fold(f32::NEG_INFINITY, f32::max)
        };
        return Ok(Term::Number(n));
    }
    let lengths: Parsed<Vec<Calc>> = args
        .into_iter()
        .map(|a| match a {
            Term::Length(c) => Ok(c),
            Term::Number(_) => Err(()),
        })
        .collect();
    Ok(Term::Length(min_max(is_min, lengths?)))
}

/// `product ( ws [+|-] ws product )*` — `+`/`-` need whitespace on both
/// sides. Stops before a `,` (an argument separator) or at the end.
fn parse_sum(input: &mut CssParser<'_>, depth: usize) -> Parsed<Term> {
    let mut acc = parse_product(input, depth)?;
    loop {
        let before = input.state();
        match input.next_including_whitespace() {
            Err(_) => return Ok(acc),
            Ok(Token::WhiteSpace(_)) => {}
            Ok(Token::Comma) => {
                input.reset(&before);
                return Ok(acc);
            }
            Ok(_) => return Err(()),
        }
        let operator = input.state();
        let sign = match input.next_including_whitespace() {
            Err(_) => return Ok(acc),
            Ok(Token::Delim('+')) => 1.0,
            Ok(Token::Delim('-')) => -1.0,
            Ok(Token::Comma) => {
                input.reset(&operator);
                return Ok(acc);
            }
            Ok(_) => return Err(()),
        };
        if !matches!(input.next_including_whitespace(), Ok(Token::WhiteSpace(_))) {
            return Err(());
        }
        let rhs = parse_product(input, depth)?;
        acc = add(acc, rhs, sign)?;
    }
}

/// `value ( [*|/] value )*` (whitespace optional).
fn parse_product(input: &mut CssParser<'_>, depth: usize) -> Parsed<Term> {
    let mut acc = parse_value_term(input, depth)?;
    loop {
        let before = input.state();
        input.skip_whitespace();
        let divide = match input.next_including_whitespace() {
            Ok(Token::Delim('*')) => false,
            Ok(Token::Delim('/')) => true,
            _ => {
                input.reset(&before);
                return Ok(acc);
            }
        };
        let rhs = parse_value_term(input, depth)?;
        acc = if divide {
            divide_term(acc, rhs)?
        } else {
            multiply(acc, rhs)?
        };
    }
}

fn parse_value_term(input: &mut CssParser<'_>, depth: usize) -> Parsed<Term> {
    input.skip_whitespace();
    let token = input.next().map_err(|_| ())?.clone();
    let term = match token {
        Token::Number { value, .. } => Term::Number(value),
        Token::Percentage { unit_value, .. } => {
            Term::Length(Calc::Sum(CalcSum::unit(unit_value * 100.0, Unit::Percent)))
        }
        Token::Dimension { value, unit, .. } => {
            let unit = length_unit(&unit).ok_or(())?;
            Term::Length(Calc::Sum(CalcSum::unit(value, unit)))
        }
        Token::ParenthesisBlock | Token::Function(_) => {
            if depth >= MAX_VAR_NESTING {
                return Err(());
            }
            let name = match &token {
                Token::Function(name) => name.to_ascii_lowercase(),
                _ => "calc".to_string(),
            };
            if !is_math_function(&name) {
                return Err(());
            }
            input
                .parse_nested_block(|inner| {
                    parse_function_body(&name, inner, depth + 1)
                        .map_err(|()| cssparser::ParseError::<()>::custom(()))
                })
                .map_err(|_| ())?
        }
        _ => return Err(()),
    };
    check(term)
}

fn check(term: Term) -> Parsed<Term> {
    let finite = match &term {
        Term::Number(n) => n.is_finite(),
        Term::Length(c) => !c.any(&|s| !s.is_finite()),
    };
    if finite { Ok(term) } else { Err(()) }
}

fn add(a: Term, b: Term, sign: f32) -> Parsed<Term> {
    check(match (a, b) {
        (Term::Number(x), Term::Number(y)) => Term::Number(x + sign * y),
        (Term::Length(x), Term::Length(y)) => Term::Length(plus(x, scale(sign, y))),
        _ => return Err(()),
    })
}

fn multiply(a: Term, b: Term) -> Parsed<Term> {
    check(match (a, b) {
        (Term::Number(x), Term::Number(y)) => Term::Number(x * y),
        (Term::Number(k), Term::Length(c)) | (Term::Length(c), Term::Number(k)) => {
            Term::Length(scale(k, c))
        }
        (Term::Length(_), Term::Length(_)) => return Err(()),
    })
}

fn divide_term(a: Term, b: Term) -> Parsed<Term> {
    let Term::Number(divisor) = b else {
        return Err(());
    };
    if divisor == 0.0 {
        return Err(());
    }
    check(match a {
        Term::Number(x) => Term::Number(x / divisor),
        Term::Length(c) => Term::Length(scale(1.0 / divisor, c)),
    })
}

/// The length units the engine models (`ex` reads as `ch`, 0.5em; the
/// dynamic / small / large and logical viewport units are the desktop
/// viewport). `None` = not a length unit (`s`, `deg`, `fr` …).
pub(super) fn length_unit(unit: &str) -> Option<Unit> {
    Some(match unit.to_ascii_lowercase().as_str() {
        "px" => Unit::Px,
        "em" => Unit::Em,
        "rem" => Unit::Rem,
        "ch" | "ex" => Unit::Ch,
        "pt" => Unit::Pt,
        "vw" | "dvw" | "svw" | "lvw" | "vi" | "dvi" | "svi" | "lvi" => Unit::Vw,
        "vh" | "dvh" | "svh" | "lvh" | "vb" | "dvb" | "svb" | "lvb" => Unit::Vh,
        "vmin" | "dvmin" | "svmin" | "lvmin" => Unit::Vmin,
        "vmax" | "dvmax" | "svmax" | "lvmax" => Unit::Vmax,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::{Calc, CalcSum, Units};
    use crate::css::{Unit, Value};

    fn math(source: &str) -> Option<Value> {
        let mut parser = cssparser::Parser::new(source);
        let value = crate::css::parse::parse_value(&mut parser).ok()?;
        parser.expect_exhausted().ok()?;
        Some(value)
    }

    fn sum(f: impl FnOnce(&mut CalcSum)) -> Calc {
        let mut s = CalcSum::default();
        f(&mut s);
        Calc::Sum(s)
    }

    fn calc(c: Calc) -> Option<Value> {
        Some(Value::Calc(Box::new(c)))
    }

    #[test]
    fn calc_mixed_units_parses() {
        assert_eq!(
            math("calc(100% - 16px + 1em - .5rem + 2vw + 1vh)"),
            calc(sum(|s| {
                s.percent = 100.0;
                s.px = -16.0;
                s.em = 1.0;
                s.rem = -0.5;
                s.vw = 2.0;
                s.vh = 1.0;
            }))
        );
        // pt → px, ch / ex → 0.5em; `-webkit-calc` is `calc`.
        assert_eq!(
            math("-webkit-calc(3pt + 2ch + 2ex)"),
            calc(sum(|s| {
                s.px = 4.0;
                s.em = 2.0;
            }))
        );
        // One unit left: a plain length.
        assert_eq!(math("calc(1px + 2px)"), Some(Value::Length(3.0, Unit::Px)));
        assert_eq!(math("calc(50%)"), Some(Value::Length(50.0, Unit::Percent)));
        assert_eq!(math("calc(2rem)"), Some(Value::Length(2.0, Unit::Rem)));
    }

    #[test]
    fn calc_nested_and_parens() {
        assert_eq!(
            math("calc((100% - (2 * 8px)) / 2)"),
            calc(sum(|s| {
                s.percent = 50.0;
                s.px = -8.0;
            }))
        );
        assert_eq!(
            math("calc(calc(1px + 2px) * 3)"),
            Some(Value::Length(9.0, Unit::Px))
        );
        assert_eq!(
            math("calc(-1 * calc(4px))"),
            Some(Value::Length(-4.0, Unit::Px))
        );
    }

    #[test]
    fn calc_multiply_divide_fold() {
        assert_eq!(math("calc(2 * 8px)"), Some(Value::Length(16.0, Unit::Px)));
        assert_eq!(math("calc(8px / 2)"), Some(Value::Length(4.0, Unit::Px)));
        assert_eq!(math("calc(8px*2)"), Some(Value::Length(16.0, Unit::Px)));
        // A number calc is a number (`line-height: calc(1 + .5)`).
        assert_eq!(math("calc(3 * 2)"), Some(Value::Number(6.0)));
        assert_eq!(math("calc(1 + .5)"), Some(Value::Number(1.5)));
    }

    #[test]
    fn calc_length_times_length_is_invalid() {
        for bad in [
            "calc(2px * 3px)",
            "calc(1px / 0)",
            "calc(1px / 0px)",
            "calc(1px / (1 - 1))",
            "calc(1px + 2)",
            "calc(100%-16px)",
            "calc(100% -16px)",
            "calc()",
            "calc(1px 2px)",
            "calc(1s + 1px)",
            "calc(1e30px * 1e30)",
            "min()",
            "clamp(1px, 2px)",
            "calc(+)",
        ] {
            assert_eq!(math(bad), None, "{bad}");
        }
    }

    #[test]
    fn min_max_clamp_parse() {
        assert_eq!(
            math("min(100% - 2rem, 1200px)"),
            calc(Calc::Min(vec![
                sum(|s| {
                    s.percent = 100.0;
                    s.rem = -2.0;
                }),
                sum(|s| s.px = 1200.0),
            ]))
        );
        // clamp(MIN, VAL, MAX) = max(MIN, min(VAL, MAX)).
        assert_eq!(
            math("clamp(1rem, 2.5vw, 2rem)"),
            calc(Calc::Max(vec![
                sum(|s| s.rem = 1.0),
                Calc::Min(vec![sum(|s| s.vw = 2.5), sum(|s| s.rem = 2.0)]),
            ]))
        );
        // All px: folded at parse time; numbers too.
        assert_eq!(
            math("max(1px, 3px, 2px)"),
            Some(Value::Length(3.0, Unit::Px))
        );
        assert_eq!(math("min(2, 3) * 1"), None, "a bare operator outside calc");
        assert_eq!(
            math("calc(min(2, 3) * 4px)"),
            Some(Value::Length(8.0, Unit::Px))
        );
        // A min() inside a sum, scaled.
        assert_eq!(
            math("calc(10px + 2 * max(1em, 5%))"),
            calc(Calc::Plus(
                Box::new(sum(|s| s.px = 10.0)),
                Box::new(Calc::Scaled(
                    2.0,
                    Box::new(Calc::Max(vec![
                        sum(|s| s.em = 1.0),
                        sum(|s| s.percent = 5.0)
                    ]))
                ))
            ))
        );
    }

    #[test]
    fn viewport_units_parse() {
        for (unit, expected) in [
            ("vw", Unit::Vw),
            ("vh", Unit::Vh),
            ("vmin", Unit::Vmin),
            ("vmax", Unit::Vmax),
            ("dvh", Unit::Vh),
            ("svh", Unit::Vh),
            ("lvh", Unit::Vh),
            ("dvw", Unit::Vw),
            ("svw", Unit::Vw),
            ("lvw", Unit::Vw),
            ("vi", Unit::Vw),
            ("vb", Unit::Vh),
            ("dvmin", Unit::Vmin),
            ("svmax", Unit::Vmax),
            ("VW", Unit::Vw),
        ] {
            assert_eq!(
                math(&format!("50{unit}")),
                Some(Value::Length(50.0, expected)),
                "{unit}"
            );
        }
        // `ex` is approximated like `ch` (0.5em).
        assert_eq!(math("2ex"), Some(Value::Length(2.0, Unit::Ch)));
    }

    #[test]
    fn calc_nesting_is_capped() {
        let deep = |n: usize| format!("{}1px{}", "calc(".repeat(n), ")".repeat(n));
        assert_eq!(math(&deep(8)), Some(Value::Length(1.0, Unit::Px)));
        assert_eq!(math(&deep(40)), None);
        let parens = format!("calc({}1px{})", "(".repeat(40), ")".repeat(40));
        assert_eq!(math(&parens), None);
        // Far past cssparser's own block limit: no overflow, just invalid.
        assert_eq!(math(&deep(20_000)), None);
    }

    #[test]
    fn resolve_folds_font_and_viewport_units() {
        let units = Units {
            em: 10.0,
            rem: 16.0,
            vw: 12.8,
            vh: 8.0,
        };
        let c = Calc::Min(vec![
            sum(|s| {
                s.percent = 100.0;
                s.rem = -2.0;
            }),
            sum(|s| {
                s.vw = 50.0;
                s.em = 1.0;
            }),
        ]);
        let resolved = c.resolve(&units);
        assert!(!c.uses_viewport_height());
        assert_eq!(resolved.eval(1000.0), 650.0);
        assert_eq!(resolved.eval(500.0), 468.0);
        assert!(resolved.has_percent());
        // Without a percentage, it folds to a plain px length.
        assert_eq!(
            sum(|s| {
                s.vh = 100.0;
                s.px = -100.0;
            })
            .resolve(&units)
            .into_value(),
            Value::Length(700.0, Unit::Px)
        );
        // vmin / vmax read the height too.
        let vmin = sum(|s| s.vmin = 10.0);
        assert!(vmin.uses_viewport_height());
        assert_eq!(vmin.resolve(&units).eval(0.0), 80.0);
        assert_eq!(sum(|s| s.vmax = 10.0).resolve(&units).eval(0.0), 128.0);
    }

    #[test]
    fn resolve_against_infinite_units_stays_finite() {
        // A `font-size: 1e39px` root makes `rem` infinite (layout fuzzer,
        // seed 106): an unused unit (0 × ∞) or ∞ − ∞ must not make NaN —
        // NaN ≠ NaN broke the cascade comparison, and eval would read 0.
        let units = Units {
            em: f32::INFINITY,
            rem: f32::INFINITY,
            vw: 12.8,
            vh: 8.0,
        };
        let c = sum(|s| {
            s.percent = 33.0;
            s.px = -5.0;
        });
        let resolved = c.resolve(&units);
        assert_eq!(resolved, c.resolve(&units));
        assert_eq!(resolved.eval(100.0), 28.0);
        let both = sum(|s| {
            s.em = 1.0;
            s.rem = -1.0;
        })
        .resolve(&units);
        assert!(both.eval(0.0).is_finite());
        assert_eq!(both, both.clone());
    }
}
