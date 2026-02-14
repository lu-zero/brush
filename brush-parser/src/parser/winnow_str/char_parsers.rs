//! Character-level parsers (Tier 0) - leaf functions that parse individual characters and tokens

use winnow::combinator::{peek, repeat};
use winnow::error::ContextError;
use winnow::prelude::*;
use winnow::stream::{LocatingSlice, Offset};
use winnow::token::take_while;

/// Type alias for parser error
pub type PError = winnow::error::ErrMode<ContextError>;

/// Type alias for input stream
pub type StrStream<'a> = LocatingSlice<&'a str>;

/// Helper: Peek at next 1-2 operator characters for dispatch
pub fn peek_op2<'a>() -> impl Parser<StrStream<'a>, &'a str, PError> {
    peek(winnow::token::take_while(1..=2, |c: char| {
        matches!(c, '<' | '>' | '&' | '|')
    }))
}

/// Helper: Peek at next 2-3 operator characters for case terminators
pub fn peek_op3<'a>() -> impl Parser<StrStream<'a>, &'a str, PError> {
    peek(winnow::token::take_while(2..=3, |c: char| {
        matches!(c, ';' | '&')
    }))
}

/// Helper: Peek at first character for `word_part` dispatch
pub fn peek_char<'a>() -> impl Parser<StrStream<'a>, char, PError> {
    peek(winnow::token::any)
}

/// Parse an extended glob pattern: @(...), +(...), *(...), ?(...), !(...)
/// Returns the entire pattern including the prefix and parentheses
pub fn extglob_pattern<'a>() -> impl Parser<StrStream<'a>, &'a str, PError> {
    move |input: &mut StrStream<'a>| {
        // Save starting checkpoint to capture the prefix char too
        let start = input.checkpoint();

        // Match the prefix character (@, !, ?, +, *)
        let _prefix_char = winnow::token::one_of(['@', '!', '?', '+', '*']).parse_next(input)?;

        // Use the helper to parse balanced parens starting from the '('
        let _balanced = parse_balanced_delimiters("(", Some('('), ')', 1).parse_next(input)?;

        // Get the full pattern including prefix character
        let end = input.checkpoint();
        let consumed_len = end.offset_from(&start);

        input.reset(&start);
        let pattern = winnow::token::take(consumed_len).parse_next(input)?;

        Ok(pattern)
    }
}

/// Parse content with balanced delimiters (parentheses, braces, backticks)
/// Returns the full slice including opening and closing delimiters
///
/// # Parameters
/// - `prefix`: The opening delimiter(s) to match first (e.g., "$(", "${", backtick)
/// - `open_char`: Character that increases depth (e.g., '(' or '{'), or None for backticks
/// - `close_char`: Character that decreases depth (e.g., ')' or '}' or backtick)
/// - `initial_depth`: Starting depth (e.g., 1 for most, 2 for arithmetic `$((`)
///
/// # Examples
/// - Command substitution: `parse_balanced_delimiters("$(", Some('('), ')', 1)`
/// - Arithmetic: `parse_balanced_delimiters("$((", Some('('), ')', 2)`
/// - Braced variable: `parse_balanced_delimiters("${", Some('{'), '}', 1)`
/// - Backtick: `parse_balanced_delimiters("`", None, '`', 1)`
pub fn parse_balanced_delimiters<'a>(
    prefix: &'a str,
    open_char: Option<char>,
    close_char: char,
    initial_depth: usize,
) -> impl Parser<StrStream<'a>, &'a str, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let start = input.checkpoint();

        // Match opening prefix - use winnow's literal parser
        winnow::token::literal(prefix).parse_next(input)?;

        // Parse balanced delimiters
        let mut depth = initial_depth;

        while depth > 0 {
            match winnow::token::any::<_, PError>.parse_next(input) {
                Ok(ch) if Some(ch) == open_char => {
                    depth += 1;
                }
                Ok(ch) if ch == close_char => {
                    depth -= 1;
                }
                Ok('\\') => {
                    // Skip escaped character
                    let _ = winnow::token::any::<_, PError>.parse_next(input);
                }
                Ok(_) => {
                    // Regular character
                }
                Err(_) => {
                    // Hit end of input without closing delimiter
                    return Err(winnow::error::ErrMode::Backtrack(ContextError::default()));
                }
            }
        }

        // Get the full slice from start to current position
        let end = input.checkpoint();
        let consumed_len = end.offset_from(&start);

        input.reset(&start);
        let result = winnow::token::take(consumed_len).parse_next(input)?;

        Ok(result)
    }
}

/// Check if character is valid in a username for tilde expansion
/// POSIX portable filename characters: alphanumeric, dot, underscore, hyphen, plus
const fn is_username_char(c: char) -> bool {
    matches!(c, 'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' | '+')
}

/// Parse a tilde expansion: ~, ~user, ~+, ~-, ~+N, ~-N
/// Returns the entire tilde expression as a string
pub fn tilde_expansion<'a>() -> impl Parser<StrStream<'a>, &'a str, PError> {
    (
        '~',
        take_while(0.., is_username_char),
        peek(winnow::combinator::alt((
            winnow::combinator::eof.void(),
            winnow::token::one_of(['/', ':', ';', '}', ' ', '\t', '\n', '&', '|', '<', '>']).void(),
        ))),
    )
        .take()
}

/// Parse a newline character
/// Corresponds to: `matches_operator("\n`") in winnow.rs
#[inline]
pub fn newline<'a>() -> impl Parser<StrStream<'a>, char, PError> {
    '\n'
}

/// Parse a comment: # to end of line (not including newline)
/// Comments start with # and continue to end of line
/// The # must appear at a word boundary (start of input or after whitespace)
#[inline]
pub fn comment<'a>() -> impl Parser<StrStream<'a>, (), PError> {
    ('#', take_while(0.., |c: char| c != '\n')).void()
}

/// Parse optional whitespace and comments (spaces, tabs, and comments, but NOT newlines)
///
/// Handles both inter-token spaces, inline comments, and backslash-newline
/// continuations like: `echo hello # comment` or `cmd \<NL> arg`.
/// This is needed to separate tokens on the same line.
#[inline]
pub fn spaces<'a>() -> impl Parser<StrStream<'a>, (), PError> {
    repeat::<_, _, (), _, _>(
        0..,
        winnow::combinator::alt((
            take_while(1.., |c: char| c == ' ' || c == '\t').void(),
            ("\\", '\n').void(), // backslash-newline continuation
            comment(),
        )),
    )
    .void()
}

/// Parse required whitespace (at least one space or tab, optionally followed by comment)
#[inline]
pub fn spaces1<'a>() -> impl Parser<StrStream<'a>, (), PError> {
    (
        take_while(1.., |c: char| c == ' ' || c == '\t'), // Required spaces
        winnow::combinator::opt(comment()),               // Optional comment after spaces
    )
        .void()
}

/// Parse whitespace inside array literals `( ... )`.
/// Newlines are treated as whitespace separators, just like spaces and tabs.
/// Also handles comments and backslash-newline continuations.
#[inline]
pub fn array_spaces<'a>() -> impl Parser<StrStream<'a>, (), PError> {
    repeat::<_, _, (), _, _>(
        0..,
        winnow::combinator::alt((
            take_while(1.., |c: char| c == ' ' || c == '\t' || c == '\n').void(),
            ("\\", '\n').void(),
            comment(),
        )),
    )
    .void()
}
