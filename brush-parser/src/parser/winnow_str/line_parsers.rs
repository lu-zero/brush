//! Line breaks and separators (Tier 1)

use winnow::combinator::{peek, repeat};
use winnow::error::ContextError;
use winnow::prelude::*;
use winnow::stream::LocatingSlice;
use winnow::token::take_while;

use crate::ast::SeparatorOperator;

/// Type alias for parser error
pub type PError = winnow::error::ErrMode<ContextError>;

/// Type alias for input stream
pub type StrStream<'a> = LocatingSlice<&'a str>;

/// Parse linebreak (zero or more newlines, with optional comments before each newline)
/// Corresponds to: winnow.rs `linebreak()`
/// Handles blank lines, comment-only lines, and lines with inline comments
#[inline]
pub fn linebreak<'a>() -> impl Parser<StrStream<'a>, (), PError> {
    repeat::<_, _, (), _, _>(
        0..,
        (
            take_while(0.., |c: char| c == ' ' || c == '\t'), // Optional leading spaces
            winnow::combinator::opt(comment()),               // Optional comment
            newline(),                                        // Required newline
        )
            .void(),
    )
}

/// Parse newline list (one or more newlines, with optional comments before each newline)
/// Corresponds to: winnow.rs `newline_list()`
/// Handles blank lines, comment-only lines, and lines with inline comments
#[inline]
pub fn newline_list<'a>() -> impl Parser<StrStream<'a>, (), PError> {
    repeat::<_, _, (), _, _>(
        1..,
        (
            take_while(0.., |c: char| c == ' ' || c == '\t'), // Optional leading spaces
            winnow::combinator::opt(comment()),               // Optional comment
            newline(),                                        // Required newline
        )
            .void(),
    )
}

/// Parse separator operator (';' or '&')
/// Must NOT be part of a longer operator like ';;', ';&', '&&', etc.
/// Corresponds to: winnow.rs `separator_op()`
#[inline]
pub fn separator_op<'a>() -> impl Parser<StrStream<'a>, SeparatorOperator, PError> {
    winnow::combinator::alt((
        // Match ';' but not if followed by another ';' or '&' (to avoid matching ";;" or ";&")
        winnow::combinator::terminated(
            ';',
            winnow::combinator::peek(winnow::combinator::not(winnow::token::one_of([';', '&']))),
        )
        .value(SeparatorOperator::Sequence),
        // Match '&' but not if followed by another '&' (to avoid matching "&&")
        winnow::combinator::terminated('&', winnow::combinator::peek(winnow::combinator::not('&')))
            .value(SeparatorOperator::Async),
    ))
}

/// Parse separator (`separator_op` with linebreak, or `newline_list`)
/// Returns Option<SeparatorOperator> - None means it was just newlines
/// Corresponds to: winnow.rs `separator()` and peg.rs `separator()`
#[inline]
pub fn separator<'a>() -> impl Parser<StrStream<'a>, Option<SeparatorOperator>, PError> {
    winnow::combinator::alt((
        // separator_op followed by optional linebreaks
        (separator_op(), linebreak()).map(|(sep, ())| Some(sep)),
        // OR just one or more newlines (acts as sequence separator)
        newline_list().map(|()| None),
    ))
}

/// Parse sequential separator (semicolon or newlines)
/// Corresponds to: winnow.rs `sequential_sep()`
#[inline]
pub fn sequential_sep<'a>() -> impl Parser<StrStream<'a>, (), PError> {
    winnow::combinator::alt(((';', linebreak()).void(), newline_list().void()))
}

/// Helper: Peek at next 1-2 operator characters for dispatch
pub fn peek_op2<'a>() -> impl Parser<StrStream<'a>, &'a str, PError> {
    peek(winnow::token::take_while(1..=2, |c: char| {
        matches!(c, '<' | '>' | '&' | '|')
    }))
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
