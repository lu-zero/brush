//! Function parsers module
//!
//! Contains parsers for function definitions (Tier 14)

use winnow::error::ContextError;
use winnow::prelude::*;

use crate::ast;
use crate::parser::winnow_str::context::PositionTracker;
use crate::parser::winnow_str::types::StrStream;
use crate::parser::winnow_str::{context::ParseContext, types::PError};

/// Check if a string is a valid bash function name.
///
/// Bash function names may contain any characters that are valid in a word
/// (including hyphens and dots), unlike variable names which are restricted
/// to `[a-zA-Z_][a-zA-Z0-9_]*`.  The only restrictions are that the name
/// must not be empty, must start with a letter or underscore, and must not
/// end with `=` (to avoid ambiguity with assignments).
fn is_valid_fname(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }
    if s.ends_with('=') {
        return false;
    }
    let first = s.chars().next().unwrap();
    first.is_ascii_alphabetic() || first == '_'
}

/// Parse a function name.
fn fname<'a>() -> impl Parser<StrStream<'a>, String, PError> {
    winnow::combinator::preceded(super::char_parsers::spaces(), super::word_parsers::bare_word())
        .verify(|s: &str| is_valid_fname(s))
        .map(|s: &str| s.to_string())
}

/// Parse function body (compound command with optional redirects)
/// Corresponds to: winnow.rs `function_body()`
pub fn function_body<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::FunctionBody, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let cmd = super::compound_parsers::compound_command(ctx, tracker).parse_next(input)?;
        let redirects = winnow::combinator::opt(winnow::combinator::preceded(
            super::char_parsers::spaces(),
            super::redirection_parsers::redirect_list(ctx, tracker),
        ))
        .parse_next(input)?;

        Ok(ast::FunctionBody(cmd, redirects))
    }
}

/// Parse function definition
/// Corresponds to: winnow.rs `function_definition()`
pub fn function_definition<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::FunctionDefinition, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        // Try "function name () body" or "function name body" format
        let has_function_keyword = super::keyword("function").parse_next(input).is_ok();

        // Track location of the function name
        let fname_start = tracker.offset_from_locating(input);
        let func_name = fname().parse_next(input)?;
        let fname_end = tracker.offset_from_locating(input);

        // Function names cannot be reserved words (unless preceded by `function` keyword)
        if !has_function_keyword && super::word_parsers::is_reserved_word(&func_name) {
            return Err(winnow::error::ErrMode::Backtrack(ContextError::default()));
        }

        // Parse optional ()
        super::char_parsers::spaces().parse_next(input)?;
        let has_parens = if winnow::combinator::opt::<_, _, PError, _>('(')
            .parse_next(input)?
            .is_some()
        {
            super::char_parsers::spaces().parse_next(input)?;
            ')'.parse_next(input)?;
            true
        } else {
            false
        };

        // Must have either "function" keyword or parens
        if !has_function_keyword && !has_parens {
            return Err(winnow::error::ErrMode::Backtrack(ContextError::default()));
        }

        super::line_parsers::linebreak().parse_next(input)?;

        let body = function_body(ctx, tracker).parse_next(input)?;

        // Create the fname Word with location
        let fname_loc = tracker.range_to_span(fname_start..fname_end);
        let fname_word = ast::Word {
            value: func_name,
            loc: Some(fname_loc),
        };

        Ok(ast::FunctionDefinition {
            fname: fname_word,
            body,
        })
    }
}