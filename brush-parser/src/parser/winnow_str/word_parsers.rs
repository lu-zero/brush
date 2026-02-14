//! Variable expansion parsers - minimal set of independent functions

use winnow::error::ContextError;
use winnow::prelude::*;
use winnow::stream::LocatingSlice;

/// Type alias for parser error
pub type PError = winnow::error::ErrMode<ContextError>;

/// Type alias for input stream
pub type StrStream<'a> = LocatingSlice<&'a str>;

/// Parse a simple variable reference: $VAR
/// Returns the expansion text including the $
pub fn simple_variable<'a>() -> impl Parser<StrStream<'a>, &'a str, PError> {
    (
        '$',
        winnow::token::take_while(1.., |c: char| c.is_alphanumeric() || c == '_'),
    )
        .take()
}

/// Parse a braced variable reference: ${VAR}
/// Returns the expansion text including ${ }
pub fn braced_variable<'a>() -> impl Parser<StrStream<'a>, &'a str, PError> {
    super::char_parsers::parse_balanced_delimiters("${", Some('{'), '}', 1)
}

/// Parse an arithmetic expansion: $((expr))
/// Returns the expansion text including $(( ))
pub fn arithmetic_expansion<'a>() -> impl Parser<StrStream<'a>, &'a str, PError> {
    super::char_parsers::parse_balanced_delimiters("$((", Some('('), ')', 2)
}

/// Parse a command substitution: $(cmd)
/// Returns the expansion text including $( )
pub fn command_substitution<'a>() -> impl Parser<StrStream<'a>, &'a str, PError> {
    // Need to be careful: $(( is arithmetic, $( is command substitution
    winnow::combinator::preceded(
        winnow::combinator::peek(winnow::combinator::not("$((")),
        super::char_parsers::parse_balanced_delimiters("$(", Some('('), ')', 1),
    )
}

/// Parse a backtick command substitution: `cmd`
/// Returns the expansion text including backticks
pub fn backtick_substitution<'a>() -> impl Parser<StrStream<'a>, &'a str, PError> {
    super::char_parsers::parse_balanced_delimiters("`", None, '`', 1)
}

/// Parse special parameter: $0, $1, $?, $@, etc.
/// Returns the expansion text including the $
pub fn special_parameter<'a>() -> impl Parser<StrStream<'a>, &'a str, PError> {
    (
        '$',
        winnow::combinator::alt((
            winnow::token::one_of(['0', '1', '2', '3', '4', '5', '6', '7', '8', '9']),
            winnow::token::one_of(['?', '@', '*', '#', '$', '!', '-', '_']),
        )),
    )
        .take()
}
