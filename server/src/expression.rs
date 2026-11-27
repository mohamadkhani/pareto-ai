//! The small numeric expression language a connector field may use instead of
//! a bare JSON Pointer (D61).
//!
//! A field is either a *path* (`$.pricing.prompt`, `$.data[0].input`) or a
//! *numeric expression* built from paths, number literals, `+ - * /`, unary
//! minus, parentheses, and a fixed set of functions (`min`, `max`, `abs`,
//! `round`, `floor`, `ceil`). Comparison, logic, and assignment do not exist:
//! the language answers "what number is here", never "what does this mean".
//!
//! ```text
//! parse("$.pricing.prompt * 1000000")
//! evaluate(…, {"pricing":{"prompt":"0.0003"}}) == 300.0
//! ```
//!
//! **Numbers from text.** A JSON number is a number. A JSON string is a number
//! when it survives [`number_from_text`]: surrounding whitespace is trimmed, a
//! leading or trailing currency symbol is dropped, `,`/`_` grouping is
//! removed, and accounting parentheses mean a negative amount. Anything else —
//! `null`, `"call us"`, a boolean — is an error, never zero (D10/D41).
//!
//! **Failure modes.** [`parse`] rejects malformed input with the byte position
//! that caused it, so the operator sees which character is wrong. [`evaluate`]
//! fails when a path is missing, resolves to a non-numeric value, divides by
//! zero, or produces a non-finite number. Neither function guesses.

use std::fmt;

use serde_json::Value;

/// Longest accepted expression, in characters. A real mapping is far shorter;
/// the cap keeps a hand-edited document from becoming a parsing exercise.
pub const MAX_EXPRESSION_CHARS: usize = 512;
/// Deepest accepted parenthesis nesting; bounds recursion in the parser.
pub const MAX_DEPTH: usize = 32;

/// A parsed expression, ready to evaluate against any document.
#[derive(Debug, Clone, PartialEq)]
pub struct Expression {
    node: Node,
}

#[derive(Debug, Clone, PartialEq)]
enum Node {
    Number(f64),
    /// Step 1: the root (`$`), step 2: a key or array index.
    Path(Vec<PathStep>),
    Negate(Box<Node>),
    Binary {
        op: BinaryOp,
        left: Box<Node>,
        right: Box<Node>,
    },
    Call {
        function: Function,
        arguments: Vec<Node>,
    },
}

#[derive(Debug, Clone, PartialEq)]
enum PathStep {
    Key(String),
    Index(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BinaryOp {
    Add,
    Subtract,
    Multiply,
    Divide,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Function {
    Min,
    Max,
    Abs,
    Round,
    Floor,
    Ceil,
}

impl Function {
    fn from_name(name: &str) -> Option<Self> {
        match name {
            "min" => Some(Self::Min),
            "max" => Some(Self::Max),
            "abs" => Some(Self::Abs),
            "round" => Some(Self::Round),
            "floor" => Some(Self::Floor),
            "ceil" => Some(Self::Ceil),
            _ => None,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Min => "min",
            Self::Max => "max",
            Self::Abs => "abs",
            Self::Round => "round",
            Self::Floor => "floor",
            Self::Ceil => "ceil",
        }
    }

    fn arity(self) -> usize {
        match self {
            Self::Min | Self::Max => 2,
            Self::Abs | Self::Round | Self::Floor | Self::Ceil => 1,
        }
    }
}

/// Why an expression failed to parse or evaluate.
#[derive(Debug, Clone, PartialEq)]
pub enum ExpressionError {
    /// The input is not well-formed. `position` is a character offset.
    Syntax { message: String, position: usize },
    /// The input is longer than [`MAX_EXPRESSION_CHARS`].
    TooLong(usize),
    /// A path did not resolve against the document.
    MissingPath { path: String },
    /// A path resolved to a value that is not a number (`null`, text, boolean).
    NotANumber { path: String, found: String },
    /// A division by zero or a non-finite result.
    NotFinite { expression: String },
}

impl fmt::Display for ExpressionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Syntax { message, position } => {
                write!(formatter, "{message} at column {}", position + 1)
            }
            Self::TooLong(length) => write!(
                formatter,
                "expression is {length} characters; the limit is {MAX_EXPRESSION_CHARS}"
            ),
            Self::MissingPath { path } => write!(formatter, "{path} is missing from the response"),
            Self::NotANumber { path, found } => {
                write!(formatter, "{path} is not a number (found {found})")
            }
            Self::NotFinite { expression } => {
                write!(formatter, "{expression} does not produce a finite number")
            }
        }
    }
}

impl std::error::Error for ExpressionError {}

impl ExpressionError {
    fn syntax(message: impl Into<String>, position: usize) -> Self {
        Self::Syntax {
            message: message.into(),
            position,
        }
    }
}

// ---------------------------------------------------------------------------
// Parsing
// ---------------------------------------------------------------------------

/// Parses `input` into an [`Expression`].
///
/// Failure mode: [`ExpressionError::Syntax`] with the offset of the first
/// offending character, or [`ExpressionError::TooLong`].
pub fn parse(input: &str) -> Result<Expression, ExpressionError> {
    let length = input.chars().count();
    if length > MAX_EXPRESSION_CHARS {
        return Err(ExpressionError::TooLong(length));
    }
    let mut parser = Parser {
        chars: input.chars().collect(),
        index: 0,
        depth: 0,
    };
    parser.skip_whitespace();
    if parser.peek().is_none() {
        return Err(ExpressionError::syntax("expression is empty", 0));
    }
    let node = parser.parse_sum()?;
    parser.skip_whitespace();
    if let Some(character) = parser.peek() {
        return Err(ExpressionError::syntax(
            format!("unexpected {character:?}"),
            parser.index,
        ));
    }
    Ok(Expression { node })
}

/// True when `input` is a single path with no operator or function around it.
/// Such a field can answer questions that need the raw JSON value (a string,
/// an array, a boolean) instead of a number.
pub fn is_single_path(expression: &Expression) -> bool {
    matches!(expression.node, Node::Path(_))
}

struct Parser {
    chars: Vec<char>,
    index: usize,
    depth: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.index).copied()
    }

    fn skip_whitespace(&mut self) {
        while matches!(self.peek(), Some(' ' | '\t' | '\n' | '\r')) {
            self.index += 1;
        }
    }

    fn parse_sum(&mut self) -> Result<Node, ExpressionError> {
        let mut left = self.parse_product()?;
        loop {
            self.skip_whitespace();
            let op = match self.peek() {
                Some('+') => BinaryOp::Add,
                Some('-') => BinaryOp::Subtract,
                _ => return Ok(left),
            };
            self.index += 1;
            let right = self.parse_product()?;
            left = Node::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
    }

    fn parse_product(&mut self) -> Result<Node, ExpressionError> {
        let mut left = self.parse_unary()?;
        loop {
            self.skip_whitespace();
            let op = match self.peek() {
                Some('*') => BinaryOp::Multiply,
                Some('/') => BinaryOp::Divide,
                _ => return Ok(left),
            };
            self.index += 1;
            let right = self.parse_unary()?;
            left = Node::Binary {
                op,
                left: Box::new(left),
                right: Box::new(right),
            };
        }
    }

    fn parse_unary(&mut self) -> Result<Node, ExpressionError> {
        self.skip_whitespace();
        if self.peek() == Some('-') {
            // Count negation toward the nesting budget so a long run of minus
            // signs cannot exhaust the stack.
            let position = self.index;
            self.enter_depth(position)?;
            self.index += 1;
            let inner = self.parse_unary()?;
            self.depth -= 1;
            return Ok(Node::Negate(Box::new(inner)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<Node, ExpressionError> {
        self.skip_whitespace();
        let Some(character) = self.peek() else {
            return Err(ExpressionError::syntax(
                "expected a number, path, or function",
                self.index,
            ));
        };
        match character {
            '(' => {
                let opened = self.index;
                self.enter_depth(opened)?;
                self.index += 1;
                let inner = self.parse_sum()?;
                self.skip_whitespace();
                if self.peek() != Some(')') {
                    return Err(ExpressionError::syntax("expected ')'", self.index));
                }
                self.index += 1;
                self.depth -= 1;
                Ok(inner)
            }
            '$' => self.parse_path(),
            character if character.is_ascii_digit() || character == '.' => {
                let position = self.index;
                let literal = self.take_number();
                literal
                    .parse::<f64>()
                    .ok()
                    .filter(|value| value.is_finite())
                    .map(Node::Number)
                    .ok_or_else(|| ExpressionError::syntax("not a valid number", position))
            }
            character if character.is_ascii_alphabetic() => {
                let position = self.index;
                let name = self.take_name();
                let function = Function::from_name(&name).ok_or_else(|| {
                    ExpressionError::syntax(
                        format!(
                            "unknown function {name:?}; the supported ones are min, max, abs, round, floor, ceil"
                        ),
                        position,
                    )
                })?;
                self.skip_whitespace();
                if self.peek() != Some('(') {
                    return Err(ExpressionError::syntax(
                        format!("{name}( needs a '(' immediately after the name"),
                        self.index,
                    ));
                }
                let opened = self.index;
                self.enter_depth(opened)?;
                self.index += 1;
                let first = self.parse_sum()?;
                let mut arguments = vec![first];
                self.skip_whitespace();
                while self.peek() == Some(',') {
                    self.index += 1;
                    arguments.push(self.parse_sum()?);
                    self.skip_whitespace();
                }
                if self.peek() != Some(')') {
                    return Err(ExpressionError::syntax("expected ')'", self.index));
                }
                self.index += 1;
                self.depth -= 1;
                let expected = function.arity();
                if arguments.len() != expected {
                    return Err(ExpressionError::syntax(
                        format!(
                            "{} takes {expected} argument{}, got {}",
                            function.name(),
                            if expected == 1 { "" } else { "s" },
                            arguments.len()
                        ),
                        opened,
                    ));
                }
                Ok(Node::Call {
                    function,
                    arguments,
                })
            }
            other => Err(ExpressionError::syntax(
                format!("unexpected {other:?}"),
                self.index,
            )),
        }
    }

    fn enter_depth(&mut self, position: usize) -> Result<(), ExpressionError> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return Err(ExpressionError::syntax(
                format!("expression nests deeper than {MAX_DEPTH} parentheses"),
                position,
            ));
        }
        Ok(())
    }

    fn take_number(&mut self) -> String {
        let start = self.index;
        while matches!(self.peek(), Some(character) if character.is_ascii_digit() || character == '.')
        {
            self.index += 1;
        }
        if matches!(self.peek(), Some('e' | 'E')) {
            let exponent = self.index;
            self.index += 1;
            if matches!(self.peek(), Some('+' | '-')) {
                self.index += 1;
            }
            let digits = self.index;
            while matches!(self.peek(), Some(character) if character.is_ascii_digit()) {
                self.index += 1;
            }
            if self.index == digits {
                self.index = exponent;
            }
        }
        self.chars[start..self.index].iter().collect()
    }

    fn take_name(&mut self) -> String {
        let start = self.index;
        while matches!(self.peek(), Some(character) if character.is_ascii_alphanumeric() || character == '_')
        {
            self.index += 1;
        }
        self.chars[start..self.index].iter().collect()
    }

    /// `$.key.key[0]`. A path must start with `$`; the root is the document
    /// itself (a catalog entry, or the whole balance response).
    fn parse_path(&mut self) -> Result<Node, ExpressionError> {
        self.index += 1; // '$'
        let mut steps = Vec::new();
        loop {
            match self.peek() {
                Some('.') => {
                    self.index += 1;
                    let key_start = self.index;
                    while let Some(character) = self.peek() {
                        if character.is_ascii_alphanumeric() || character == '_' || character == '-'
                        {
                            self.index += 1;
                        } else {
                            break;
                        }
                    }
                    if self.index == key_start {
                        return Err(ExpressionError::syntax(
                            "expected a key after '.' (letters, digits, '_' or '-')",
                            self.index,
                        ));
                    }
                    let key: String = self.chars[key_start..self.index].iter().collect();
                    steps.push(PathStep::Key(key));
                }
                Some('[') => {
                    self.index += 1;
                    let digits_start = self.index;
                    while matches!(self.peek(), Some(character) if character.is_ascii_digit()) {
                        self.index += 1;
                    }
                    if self.index == digits_start {
                        return Err(ExpressionError::syntax(
                            "expected an array index after '['",
                            self.index,
                        ));
                    }
                    if self.peek() != Some(']') {
                        return Err(ExpressionError::syntax("expected ']'", self.index));
                    }
                    let digits: String = self.chars[digits_start..self.index].iter().collect();
                    self.index += 1;
                    let index = digits.parse::<usize>().map_err(|_| {
                        ExpressionError::syntax("array index is too large", digits_start)
                    })?;
                    steps.push(PathStep::Index(index));
                }
                _ => break,
            }
        }
        Ok(Node::Path(steps))
    }
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

/// Evaluates `expression` against `document`, returning a finite number.
///
/// Failure mode: [`ExpressionError::MissingPath`],
/// [`ExpressionError::NotANumber`], or [`ExpressionError::NotFinite`].
pub fn evaluate(expression: &Expression, document: &Value) -> Result<f64, ExpressionError> {
    evaluate_node(&expression.node, document)
}

/// Resolves a **single-path** expression to the raw JSON value, without
/// requiring it to be a number. Used by fields that read a string, an array,
/// or a boolean (the model id, the modalities, the currency, the health flag).
///
/// Failure mode: [`ExpressionError::MissingPath`], or a syntax error when the
/// expression is not a lone path.
pub fn resolve_value<'a>(
    expression: &Expression,
    document: &'a Value,
) -> Result<&'a Value, ExpressionError> {
    match &expression.node {
        Node::Path(steps) => resolve(document, steps),
        _ => Err(ExpressionError::Syntax {
            message: "this field must be a single path, not a calculation".to_string(),
            position: 0,
        }),
    }
}

fn evaluate_node(node: &Node, document: &Value) -> Result<f64, ExpressionError> {
    match node {
        Node::Number(value) => Ok(*value),
        Node::Path(steps) => {
            let value = resolve(document, steps)?;
            number_from_value(value).map_err(|found| ExpressionError::NotANumber {
                path: render_path(steps),
                found,
            })
        }
        Node::Negate(inner) => Ok(-evaluate_node(inner, document)?),
        Node::Call {
            function,
            arguments,
        } => {
            let mut values = Vec::with_capacity(arguments.len());
            for argument in arguments {
                values.push(evaluate_node(argument, document)?);
            }
            // `parse` enforces `Function::arity`, so the pattern below always
            // matches; a mismatch would be a parser bug, not operator input.
            let [first, second] = match (values.as_slice(), function.arity()) {
                ([first], 1) => [*first, 0.0],
                ([first, second], 2) => [*first, *second],
                _ => {
                    return Err(ExpressionError::NotFinite {
                        expression: format!("{} with the wrong argument count", function.name()),
                    });
                }
            };
            Ok(match function {
                Function::Abs => first.abs(),
                Function::Round => first.round(),
                Function::Floor => first.floor(),
                Function::Ceil => first.ceil(),
                Function::Min => first.min(second),
                Function::Max => first.max(second),
            })
        }
        Node::Binary { op, left, right } => {
            let left = evaluate_node(left, document)?;
            let right = evaluate_node(right, document)?;
            let value = match op {
                BinaryOp::Add => left + right,
                BinaryOp::Subtract => left - right,
                BinaryOp::Multiply => left * right,
                BinaryOp::Divide => {
                    if right == 0.0 {
                        return Err(ExpressionError::NotFinite {
                            expression: "division by zero".to_string(),
                        });
                    }
                    left / right
                }
            };
            if !value.is_finite() {
                return Err(ExpressionError::NotFinite {
                    expression: format!("{:?}", op_symbol(*op)),
                });
            }
            Ok(value)
        }
    }
}

fn op_symbol(op: BinaryOp) -> &'static str {
    match op {
        BinaryOp::Add => "+",
        BinaryOp::Subtract => "-",
        BinaryOp::Multiply => "*",
        BinaryOp::Divide => "/",
    }
}

fn resolve<'a>(document: &'a Value, steps: &[PathStep]) -> Result<&'a Value, ExpressionError> {
    let mut current = document;
    for (depth, step) in steps.iter().enumerate() {
        current = match step {
            PathStep::Key(key) => current
                .get(key)
                .ok_or_else(|| ExpressionError::MissingPath {
                    path: render_path(&steps[..=depth]),
                })?,
            PathStep::Index(index) => {
                current
                    .get(*index)
                    .ok_or_else(|| ExpressionError::MissingPath {
                        path: render_path(&steps[..=depth]),
                    })?
            }
        };
    }
    Ok(current)
}

fn render_path(steps: &[PathStep]) -> String {
    let mut rendered = String::from("$");
    for step in steps {
        match step {
            PathStep::Key(key) => {
                rendered.push('.');
                rendered.push_str(key);
            }
            PathStep::Index(index) => {
                rendered.push('[');
                rendered.push_str(&index.to_string());
                rendered.push(']');
            }
        }
    }
    rendered
}

/// Reads a number out of a JSON value: a JSON number, or a string that
/// survives [`number_from_text`]. `Err` carries the value's kind for the error
/// message.
pub fn number_from_value(value: &Value) -> Result<f64, String> {
    match value {
        Value::Number(number) => number
            .as_f64()
            .filter(|value| value.is_finite())
            .ok_or_else(|| "a number outside the finite range".to_string()),
        Value::String(text) => number_from_text(text).ok_or_else(|| format!("the string {text:?}")),
        Value::Null => Err("null".to_string()),
        Value::Bool(value) => Err(format!("the boolean {value}")),
        Value::Array(_) => Err("an array".to_string()),
        Value::Object(_) => Err("an object".to_string()),
    }
}

/// Turns `"2$"`, `"$2"`, `"1,200"`, `" 3.5 "`, `"(5)"`, and `"−4"` into
/// numbers. Everything else is `None`: a currency *code* (`"USD 2"`), words,
/// empty text, or a sign in the middle.
pub fn number_from_text(text: &str) -> Option<f64> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    let (negative, body) = match trimmed
        .strip_prefix('(')
        .and_then(|rest| rest.strip_suffix(')'))
    {
        Some(inner) => (true, inner.trim()),
        None => (false, trimmed),
    };
    if body.is_empty() {
        return None;
    }
    // Drop currency symbols and grouping separators; keep digits, '.', and a
    // leading sign. `-` is the only sign char that survives.
    let mut cleaned = String::with_capacity(body.len());
    let mut seen_sign = false;
    let mut seen_digit = false;
    for (index, character) in body.chars().enumerate() {
        match character {
            '0'..='9' => {
                seen_digit = true;
                cleaned.push(character);
            }
            '.' => cleaned.push(character),
            ',' | '_' | '\'' | ' ' => {}
            '-' if index == 0 && !seen_sign => {
                seen_sign = true;
                cleaned.push('-');
            }
            character if is_currency_symbol(character) => {}
            _ => return None,
        }
    }
    if !seen_digit {
        return None;
    }
    let value = cleaned
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())?;
    Some(if negative { -value } else { value })
}

fn is_currency_symbol(character: char) -> bool {
    matches!(
        character,
        '$' | '€'
            | '£'
            | '¥'
            | '₹'
            | '₺'
            | '₽'
            | '₩'
            | '₪'
            | '₫'
            | '₴'
            | '₦'
            | '₱'
            | '฿'
            | '₡'
            | '₲'
            | '₵'
            | '₸'
            | '₭'
            | '₮'
            | '₾'
            | '₼'
            | '﷼'
            | '¢'
            | '元'
            | '円'
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(source: &str, document: &Value) -> Result<f64, ExpressionError> {
        evaluate(&parse(source)?, document)
    }

    #[test]
    fn a_bare_path_reads_the_number() {
        let document = json!({ "pricing": { "prompt": 0.5 } });
        assert_eq!(run("$.pricing.prompt", &document), Ok(0.5));
    }

    #[test]
    fn arithmetic_follows_precedence_and_parentheses() {
        let document = json!({});
        assert_eq!(run("1 + 2 * 3", &document), Ok(7.0));
        assert_eq!(run("(1 + 2) * 3", &document), Ok(9.0));
        assert_eq!(run("10 / 4 - 1", &document), Ok(1.5));
        assert_eq!(run("-(2 + 3)", &document), Ok(-5.0));
    }

    #[test]
    fn a_per_token_string_scales_to_per_million() {
        let document = json!({ "pricing": { "prompt": "0.0003" } });
        assert_eq!(run("$.pricing.prompt * 1000000", &document), Ok(300.0));
    }

    #[test]
    fn two_fields_combine() {
        let document = json!({ "credits": { "total": 100.0, "used": 40.0 } });
        assert_eq!(
            run("($.credits.total - $.credits.used) * 0.01", &document),
            Ok(0.6)
        );
    }

    #[test]
    fn array_indexes_resolve() {
        let document = json!({ "data": [{ "input": 1.0 }, { "input": 2.0 }] });
        assert_eq!(run("$.data[1].input", &document), Ok(2.0));
    }

    #[test]
    fn functions_take_their_documented_arity() {
        let document = json!({ "balance": -3.0 });
        assert_eq!(run("abs($.balance)", &document), Ok(3.0));
        assert_eq!(run("max($.balance, 0)", &document), Ok(0.0));
        assert_eq!(run("min($.balance, 0)", &document), Ok(-3.0));
        assert_eq!(run("round(2.6)", &document), Ok(3.0));
        assert_eq!(run("floor(2.6)", &document), Ok(2.0));
        assert_eq!(run("ceil(2.1)", &document), Ok(3.0));
    }

    #[test]
    fn a_currency_suffix_string_is_a_number() {
        let document = json!({ "balance": "2$" });
        assert_eq!(run("$.balance", &document), Ok(2.0));
    }

    #[test]
    fn null_is_an_error_not_zero() {
        let document = json!({ "pricing": { "prompt": null } });
        let error = run("$.pricing.prompt", &document).unwrap_err();
        assert!(matches!(error, ExpressionError::NotANumber { .. }));
    }

    #[test]
    fn text_that_is_not_a_number_is_an_error() {
        let document = json!({ "pricing": { "prompt": "call us" } });
        let error = run("$.pricing.prompt", &document).unwrap_err();
        match error {
            ExpressionError::NotANumber { path, found } => {
                assert_eq!(path, "$.pricing.prompt");
                assert!(found.contains("call us"));
            }
            other => panic!("expected NotANumber, got {other:?}"),
        }
    }

    #[test]
    fn a_missing_path_names_the_segment_that_failed() {
        let document = json!({ "pricing": {} });
        let error = run("$.pricing.prompt", &document).unwrap_err();
        assert_eq!(
            error,
            ExpressionError::MissingPath {
                path: "$.pricing.prompt".to_string()
            }
        );
    }

    #[test]
    fn division_by_zero_fails() {
        let document = json!({ "a": 1.0, "b": 0.0 });
        let error = run("$.a / $.b", &document).unwrap_err();
        assert!(matches!(error, ExpressionError::NotFinite { .. }));
    }

    #[test]
    fn syntax_errors_report_a_column() {
        let error = parse("$.pricing.prompt * ").unwrap_err();
        match error {
            ExpressionError::Syntax { position, .. } => assert_eq!(position, 19),
            other => panic!("expected a syntax error, got {other:?}"),
        }
        assert!(parse("").is_err());
        assert!(parse("$.pricing.prompt +").is_err());
        assert!(parse("(1 + 2").is_err());
        assert!(parse("$.data[]").is_err());
        assert!(parse("$.data.").is_err());
        assert!(parse("unknown(1)").is_err());
        assert!(parse("min(1)").is_err());
        assert!(parse("1 2").is_err());
    }

    #[test]
    fn the_retired_json_pointer_syntax_is_rejected() {
        let error = parse("/pricing/prompt").unwrap_err();
        assert!(matches!(error, ExpressionError::Syntax { .. }));
    }

    #[test]
    fn overly_long_and_deep_input_is_rejected() {
        assert!(matches!(
            parse(&"1".repeat(MAX_EXPRESSION_CHARS + 1)),
            Err(ExpressionError::TooLong(_))
        ));
        let deep = format!(
            "{}1{}",
            "(".repeat(MAX_DEPTH + 1),
            ")".repeat(MAX_DEPTH + 1)
        );
        assert!(matches!(parse(&deep), Err(ExpressionError::Syntax { .. })));
        let allowed = format!("{}1{}", "(".repeat(MAX_DEPTH), ")".repeat(MAX_DEPTH));
        assert!(parse(&allowed).is_ok());
    }

    #[test]
    fn a_single_path_is_distinguishable_from_an_expression() {
        assert!(is_single_path(&parse("$.id").unwrap()));
        assert!(!is_single_path(&parse("$.a + 1").unwrap()));
        assert!(!is_single_path(&parse("1").unwrap()));
    }

    #[test]
    fn number_from_text_accepts_the_documented_forms() {
        assert_eq!(number_from_text("2$"), Some(2.0));
        assert_eq!(number_from_text("$2"), Some(2.0));
        assert_eq!(number_from_text(" 1,200 "), Some(1200.0));
        assert_eq!(number_from_text("€ 3.5"), Some(3.5));
        assert_eq!(number_from_text("(5)"), Some(-5.0));
        assert_eq!(number_from_text("-4"), Some(-4.0));
        assert_eq!(number_from_text("0"), Some(0.0));
    }

    #[test]
    fn number_from_text_refuses_what_is_not_a_number() {
        assert_eq!(number_from_text(""), None);
        assert_eq!(number_from_text("   "), None);
        assert_eq!(number_from_text("call us"), None);
        assert_eq!(number_from_text("USD 2"), None);
        assert_eq!(number_from_text("2 per token"), None);
        assert_eq!(number_from_text("()"), None);
        assert_eq!(number_from_text("$"), None);
        assert_eq!(number_from_text("1.2.3"), None);
    }
}
