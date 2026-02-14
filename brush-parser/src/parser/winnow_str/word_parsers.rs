//! Variable expansion and basic word parsers

use winnow::error::ContextError;
use winnow::prelude::*;
use winnow::stream::LocatingSlice;
use winnow::token::take_while;

/// Type alias for parser error
pub type PError = winnow::error::ErrMode<ContextError>;

/// Type alias for input stream
pub type StrStream<'a> = LocatingSlice<&'a str>;

/// Parse a bare word (literal characters only, no quotes or expansions)
/// Corresponds to the `literal_chars` part of tokenizer's word parsing
///
/// A word character is anything that's NOT:
/// - Whitespace: ' ', '\t', '\n', '\r'
/// - Operators: '|', '&', ';', '<', '>', '(', ')'
/// - Quote/expansion starters: '$', backtick, '\'', '"', '\\'
///
/// Note: '{' and '}' ARE allowed in words for brace expansion (e.g., {1..10}, {a,b,c})
/// Brace groups ({ commands; }) are distinguished by requiring whitespace after '{' and before '}'
///
/// Note: Shell keywords (if, then, fi, etc.) are NOT excluded here because they
/// can be used as regular words in non-keyword contexts (e.g., "echo done").
/// The `command()` parser tries compound commands first, so keywords in keyword
/// positions will be matched by compound command parsers before `bare_word` sees them.
pub fn bare_word<'a>() -> impl Parser<StrStream<'a>, &'a str, PError> {
    take_while(1.., |c: char| {
        !matches!(
            c,
            ' ' | '\t' | '\n' | '\r' |  // Whitespace
            '|' | '&' | ';' | '<' | '>' | '(' | ')' |  // Operators (note: { } removed to allow brace expansion)
            '$' | '`' | '\'' | '"' | '\\' // Quote/expansion starts
        )
    })
}

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
