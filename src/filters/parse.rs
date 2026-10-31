//! Hand-written recursive-descent parser for the filter-query grammar
//! subset fixed in `docs/manual/epic-10-filter-support.md` (T-903,
//! EPIC-10).
//!
//! [`parse`] is the only entry point. It either returns a fully-built
//! [`FilterAst`] or an [`Unsupported`] verdict — there is no partial
//! result: per the spike's fallback rule, a query containing even one
//! construct this client does not understand is rejected wholesale. That
//! rule is enforced structurally here: the lexer itself refuses to
//! tokenize any construct the grammar does not define (an unsupported
//! keyword, a `:` sub-language, an unbalanced paren at the token level is
//! instead caught by the parser), so there is never a tree to partially
//! build from.
//!
//! Operator precedence, low to high: `|` (or) < `&` (and) < `!` (not) <
//! parenthesised/leaf atom. Leaves are: `today`, `overdue`, `#project`,
//! `@label`, `p1`..`p4`, `no date` / `no due date`, `no labels` / `no
//! label`.
//!
//! This module only parses: no evaluation (T-905) and no cache access
//! (EPIC-04) happen here. `Unsupported` is intentionally a standalone
//! error type, not a new `AppError` variant — the parser is not yet
//! called from any fallible app path (see task context).

use std::fmt;

use thiserror::Error;

/// A fully-parsed filter query: boolean structure over leaf predicates.
///
/// Every node here corresponds to a `supported-locally` construct in the
/// spike's support matrix. There is no "unknown" or "unsupported" variant
/// by design — any construct the grammar does not define never makes it
/// into an `Ast` node; [`parse`] returns [`Unsupported`] instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterAst {
    /// `left & right`.
    And(Box<FilterAst>, Box<FilterAst>),
    /// `left | right`.
    Or(Box<FilterAst>, Box<FilterAst>),
    /// `!inner`.
    Not(Box<FilterAst>),
    /// A single leaf predicate (no further boolean structure).
    Leaf(Leaf),
}

/// A single supported leaf predicate.
///
/// Field strings (`Project`, `Label`) are the raw name as written in the
/// query, case and all — the evaluator (T-905) decides how to match it
/// against cached project/label names, not this parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leaf {
    /// `today`.
    Today,
    /// `overdue`.
    Overdue,
    /// `#project-name`.
    Project(String),
    /// `@label-name`.
    Label(String),
    /// `p1`..`p4`; always in `1..=4`.
    Priority(u8),
    /// `no date` / `no due date`.
    NoDate,
    /// `no labels` / `no label`.
    NoLabels,
}

/// A query (or a sub-construct of one) this client refuses to evaluate.
///
/// Carries a short, human-readable `reason` fit for display in the UI
/// (the sidebar renders saved filters whose query does not parse as
/// "server-defined / unsupported", per the spike's fallback rule). The
/// `Display` impl renders `unsupported: {reason}`, matching the examples
/// in the spike doc verbatim (e.g. `unsupported: assigned to:`).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("unsupported: {reason}")]
pub struct Unsupported {
    /// Short description of the offending construct or parse failure,
    /// without the `unsupported:` prefix (added by `Display`).
    pub reason: String,
}

impl Unsupported {
    fn new(reason: impl Into<String>) -> Self {
        Unsupported {
            reason: reason.into(),
        }
    }
}

/// Parse `query` against the grammar subset fixed in
/// `docs/manual/epic-10-filter-support.md`:
///
/// ```text
/// query     = or_expr ;
/// or_expr   = and_expr , { "|" , and_expr } ;
/// and_expr  = not_expr , { "&" , not_expr } ;
/// not_expr  = [ "!" ] , atom ;
/// atom      = "(" , or_expr , ")" | leaf ;
/// leaf      = "today" | "overdue" | "no date" | "no due date"
///           | "no labels" | "no label" | "p1" | "p2" | "p3" | "p4"
///           | "#" name | "@" name ;
/// ```
///
/// Any construct the spike marks `unsupported`, any lexical error (stray
/// `:` sub-language, unknown word) and any structural parse error
/// (unbalanced parentheses, trailing tokens, missing operand) all yield
/// `Err(Unsupported)` — never a partially-built [`FilterAst`].
pub fn parse(query: &str) -> Result<FilterAst, Unsupported> {
    let tokens = lex(query)?;
    let mut parser = Parser { tokens, pos: 0 };
    let ast = parser.parse_or()?;
    parser.expect_eof()?;
    Ok(ast)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    LParen,
    RParen,
    Amp,
    Pipe,
    Bang,
    Hash(String),
    At(String),
    Priority(u8),
    Today,
    Overdue,
    NoDate,
    NoLabels,
}

impl fmt::Display for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Token::LParen => write!(f, "("),
            Token::RParen => write!(f, ")"),
            Token::Amp => write!(f, "&"),
            Token::Pipe => write!(f, "|"),
            Token::Bang => write!(f, "!"),
            Token::Hash(name) => write!(f, "#{name}"),
            Token::At(name) => write!(f, "@{name}"),
            Token::Priority(n) => write!(f, "p{n}"),
            Token::Today => write!(f, "today"),
            Token::Overdue => write!(f, "overdue"),
            Token::NoDate => write!(f, "no date"),
            Token::NoLabels => write!(f, "no labels"),
        }
    }
}

/// Raw (pre-keyword-resolution) lexical unit: either a single-character
/// symbol the grammar gives meaning to, or a whitespace-delimited word
/// (which may still need combining with its neighbours, e.g. `no` +
/// `date`, before it resolves to a [`Token`]).
enum RawToken {
    Symbol(char),
    Word(String),
}

/// Split `query` into symbols and whitespace-delimited words, then
/// resolve that raw stream into grammar [`Token`]s, merging multi-word
/// leaves (`no date`, `no due date`, `no labels`, `no label`) and
/// rejecting (as [`Unsupported`]) anything the grammar does not define.
fn lex(query: &str) -> Result<Vec<Token>, Unsupported> {
    let raw = lex_raw(query);
    resolve_words(&raw)
}

/// First pass: character-level scan into symbols and raw words. Does not
/// yet know which words are keywords — that needs look-ahead across word
/// boundaries (`no date` is two words, one leaf).
fn lex_raw(query: &str) -> Vec<RawToken> {
    let mut out = Vec::new();
    let mut chars = query.chars().peekable();
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
            continue;
        }
        if matches!(c, '(' | ')' | '&' | '|' | '!') {
            chars.next();
            out.push(RawToken::Symbol(c));
            continue;
        }
        // Word: run of characters up to the next whitespace or symbol.
        // `:` is kept inside the word (it is never a symbol on its own)
        // so later resolution can report which keyword it followed.
        let mut word = String::new();
        while let Some(&c) = chars.peek() {
            if c.is_whitespace() || matches!(c, '(' | ')' | '&' | '|' | '!') {
                break;
            }
            word.push(c);
            chars.next();
        }
        out.push(RawToken::Word(word));
    }
    out
}

/// Second pass: resolve the raw symbol/word stream into grammar tokens.
///
/// Any word that is not one of the defined leaf keywords — including
/// every construct the spike doc marks `unsupported` (`assigned to:`,
/// `search:`, date ranges, `shared`, `recurring`, `##project`,
/// `/section`, `subtask`, `view all`, ...) and any genuinely unknown
/// word — rejects the whole query immediately.
fn resolve_words(raw: &[RawToken]) -> Result<Vec<Token>, Unsupported> {
    let mut tokens = Vec::with_capacity(raw.len());
    let mut i = 0;
    while i < raw.len() {
        match &raw[i] {
            RawToken::Symbol('(') => {
                tokens.push(Token::LParen);
                i += 1;
            }
            RawToken::Symbol(')') => {
                tokens.push(Token::RParen);
                i += 1;
            }
            RawToken::Symbol('&') => {
                tokens.push(Token::Amp);
                i += 1;
            }
            RawToken::Symbol('|') => {
                tokens.push(Token::Pipe);
                i += 1;
            }
            RawToken::Symbol('!') => {
                tokens.push(Token::Bang);
                i += 1;
            }
            RawToken::Symbol(other) => {
                // Unreachable with the symbol set `lex_raw` produces, but
                // kept exhaustive rather than panicking on future symbols.
                return Err(Unsupported::new(format!("unknown token '{other}'")));
            }
            RawToken::Word(word) => {
                let (token, consumed) = resolve_word(raw, i, word)?;
                tokens.push(token);
                i += consumed;
            }
        }
    }
    Ok(tokens)
}

/// Resolve the word at `raw[i]` to a single [`Token`], consuming one or
/// more raw entries (`no date` / `no due date` / `no labels` / `no
/// label` span two or three words). Returns the token plus how many raw
/// entries it consumed.
fn resolve_word(raw: &[RawToken], i: usize, word: &str) -> Result<(Token, usize), Unsupported> {
    let lower = word.to_ascii_lowercase();

    if word.contains(':') {
        return Err(unsupported_colon(raw, i, word));
    }

    match lower.as_str() {
        "today" => return Ok((Token::Today, 1)),
        "overdue" => return Ok((Token::Overdue, 1)),
        "p1" => return Ok((Token::Priority(1), 1)),
        "p2" => return Ok((Token::Priority(2), 1)),
        "p3" => return Ok((Token::Priority(3), 1)),
        "p4" => return Ok((Token::Priority(4), 1)),
        "no" => return resolve_no(raw, i),
        "shared" => return Err(Unsupported::new("shared")),
        "recurring" => return Err(Unsupported::new("recurring")),
        "subtask" => return Err(Unsupported::new("subtask")),
        "due" => return Err(Unsupported::new("date range")),
        "assigned" => {
            return match word_at(raw, i + 1).map(str::to_ascii_lowercase).as_deref() {
                Some("to:") => Err(Unsupported::new("assigned to:")),
                Some("by:") => Err(Unsupported::new("assigned by:")),
                _ => Err(Unsupported::new("unknown token")),
            };
        }
        "view" => {
            if word_at(raw, i + 1).is_some_and(|w| w.eq_ignore_ascii_case("all")) {
                return Err(Unsupported::new("view all"));
            }
            return Err(Unsupported::new("unknown token"));
        }
        _ => {}
    }

    if let Some(name) = word.strip_prefix('#') {
        if name.starts_with('#') || name.is_empty() {
            return Err(Unsupported::new(word.to_string()));
        }
        return Ok((Token::Hash(name.to_string()), 1));
    }
    if let Some(name) = word.strip_prefix('@') {
        if name.is_empty() {
            return Err(Unsupported::new("unknown token"));
        }
        return Ok((Token::At(name.to_string()), 1));
    }
    if word.starts_with('/') {
        return Err(Unsupported::new(word.to_string()));
    }

    Err(Unsupported::new("unknown token"))
}

/// Resolve the two/three-word `no ...` leaves: `no date`, `no due date`,
/// `no labels`, `no label`.
fn resolve_no(raw: &[RawToken], i: usize) -> Result<(Token, usize), Unsupported> {
    match word_at(raw, i + 1).map(str::to_ascii_lowercase).as_deref() {
        Some("date") => Ok((Token::NoDate, 2)),
        Some("labels" | "label") => Ok((Token::NoLabels, 2)),
        Some("due") => match word_at(raw, i + 2).map(str::to_ascii_lowercase).as_deref() {
            Some("date") => Ok((Token::NoDate, 3)),
            _ => Err(Unsupported::new("date range")),
        },
        _ => Err(Unsupported::new("unknown token")),
    }
}

/// Peek the word at raw index `idx`, if any and if it is a word (not a
/// symbol).
fn word_at(raw: &[RawToken], idx: usize) -> Option<&str> {
    match raw.get(idx) {
        Some(RawToken::Word(w)) => Some(w.as_str()),
        _ => None,
    }
}

/// Build the `Unsupported` reason for a word containing `:`, recognising
/// the specific `unsupported` constructs the spike names (`assigned
/// to:`, `assigned by:`, `search:`, date-range keywords) and falling
/// back to a generic reason for anything else.
fn unsupported_colon(raw: &[RawToken], i: usize, word: &str) -> Unsupported {
    let lower = word.to_ascii_lowercase();
    if lower == "to:" || lower == "by:" {
        if let Some(prev) = i.checked_sub(1).and_then(|p| word_at(raw, p)) {
            if prev.eq_ignore_ascii_case("assigned") {
                return Unsupported::new(format!("assigned {lower}"));
            }
        }
    }
    match lower.as_str() {
        "search:" => Unsupported::new("search:"),
        "due:" | "date:" | "created:" | "before:" | "after:" => Unsupported::new("date range"),
        _ => Unsupported::new("unknown token"),
    }
}

struct Parser {
    tokens: Vec<Token>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn advance(&mut self) -> Option<&Token> {
        let token = self.tokens.get(self.pos);
        self.pos += 1;
        token
    }

    fn expect_eof(&self) -> Result<(), Unsupported> {
        if self.pos < self.tokens.len() {
            return Err(Unsupported::new("unknown token"));
        }
        Ok(())
    }

    /// `or_expr = and_expr , { "|" , and_expr } ;`
    fn parse_or(&mut self) -> Result<FilterAst, Unsupported> {
        let mut left = self.parse_and()?;
        while matches!(self.peek(), Some(Token::Pipe)) {
            self.advance();
            let right = self.parse_and()?;
            left = FilterAst::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// `and_expr = not_expr , { "&" , not_expr } ;`
    fn parse_and(&mut self) -> Result<FilterAst, Unsupported> {
        let mut left = self.parse_not()?;
        while matches!(self.peek(), Some(Token::Amp)) {
            self.advance();
            let right = self.parse_not()?;
            left = FilterAst::And(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// `not_expr = [ "!" ] , atom ;`
    fn parse_not(&mut self) -> Result<FilterAst, Unsupported> {
        if matches!(self.peek(), Some(Token::Bang)) {
            self.advance();
            let inner = self.parse_atom()?;
            return Ok(FilterAst::Not(Box::new(inner)));
        }
        self.parse_atom()
    }

    /// `atom = "(" , or_expr , ")" | leaf ;`
    fn parse_atom(&mut self) -> Result<FilterAst, Unsupported> {
        match self.advance() {
            Some(Token::LParen) => {
                let inner = self.parse_or()?;
                match self.advance() {
                    Some(Token::RParen) => Ok(inner),
                    _ => Err(Unsupported::new("unbalanced parentheses")),
                }
            }
            Some(Token::Today) => Ok(FilterAst::Leaf(Leaf::Today)),
            Some(Token::Overdue) => Ok(FilterAst::Leaf(Leaf::Overdue)),
            Some(Token::NoDate) => Ok(FilterAst::Leaf(Leaf::NoDate)),
            Some(Token::NoLabels) => Ok(FilterAst::Leaf(Leaf::NoLabels)),
            Some(Token::Priority(n)) => Ok(FilterAst::Leaf(Leaf::Priority(*n))),
            Some(Token::Hash(name)) => Ok(FilterAst::Leaf(Leaf::Project(name.clone()))),
            Some(Token::At(name)) => Ok(FilterAst::Leaf(Leaf::Label(name.clone()))),
            Some(Token::RParen | Token::Amp | Token::Pipe | Token::Bang) => {
                Err(Unsupported::new("unbalanced parentheses"))
            }
            None => Err(Unsupported::new("unbalanced parentheses")),
        }
    }
}
