//! Command parsers module
//!
//! Contains parsers for command parsing (Tier 3)

use winnow::combinator::repeat;
use winnow::error::ContextError;
use winnow::prelude::*;

use crate::ast;
use crate::parser::winnow_str::context::PositionTracker;
use crate::parser::winnow_str::{
    char_parsers, context::ParseContext, redirection_parsers, types::PError, word_parsers,
};

use crate::parser::winnow_str::types::StrStream;

/// Parse an array element value (handles quotes properly, stops at ')' or whitespace)
fn array_element_value<'a>(
    ctx: &'a ParseContext<'a>,
) -> impl Parser<StrStream<'a>, String, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let mut value = String::new();

        loop {
            // Check if we should stop (at ')' or unquoted whitespace)
            let Ok(ch) = char_parsers::peek_char().parse_next(input) else {
                break; // EOF
            };

            if ch == ')' || ch.is_whitespace() {
                break;
            }

            // Parse the next word part
            let part = word_parsers::word_part(ctx, value.chars().last()).parse_next(input)?;
            value.push_str(&part);
        }

        if value.is_empty() {
            return Err(winnow::error::ErrMode::Backtrack(ContextError::default()));
        }

        Ok(value)
    }
}

/// Parse an array element: either "value" or "[index]=value"
fn array_element<'a>(
    ctx: &'a ParseContext<'a>,
) -> impl Parser<StrStream<'a>, (Option<ast::Word>, ast::Word), PError> + 'a {
    move |input: &mut StrStream<'a>| {
        // Skip whitespace before element (newlines are whitespace inside arrays)
        char_parsers::array_spaces().parse_next(input)?;

        // Try to parse indexed element: [index]=value
        let checkpoint = input.checkpoint();
        let has_bracket = winnow::combinator::opt::<_, _, PError, _>('[')
            .parse_next(input)?
            .is_some();

        if has_bracket {
            // Parse index (everything until ])
            let index_str = winnow::token::take_while::<_, _, PError>(0.., |c: char| c != ']')
                .parse_next(input)?;

            let has_close = winnow::combinator::opt::<_, _, PError, _>(']')
                .parse_next(input)?
                .is_some();
            let has_equals = winnow::combinator::opt::<_, _, PError, _>('=')
                .parse_next(input)?
                .is_some();

            if has_close && has_equals {
                // Parse value using proper word parsing that handles quotes
                let value_str = winnow::combinator::opt(array_element_value(ctx))
                    .parse_next(input)?
                    .unwrap_or_default();

                return Ok((Some(ast::Word::new(index_str)), ast::Word::new(&value_str)));
            }
        }

        // Reset and try simple value
        input.reset(&checkpoint);

        // Parse simple value using proper word parsing that handles quotes
        let value_str = array_element_value(ctx).parse_next(input)?;

        Ok((None, ast::Word::new(&value_str)))
    }
}

/// Parse an assignment word (VAR=value or VAR+=value or VAR[idx]=value or VAR=(array elements))
/// Returns (Assignment, original word as `ast::Word`)
pub fn assignment_word<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, (ast::Assignment, ast::Word), PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let start_offset = tracker.offset_from_locating(input);

        // Parse variable name (must start with letter or underscore)
        let var_name = (
            winnow::token::one_of(|c: char| c.is_ascii_alphabetic() || c == '_'),
            winnow::token::take_while(0.., |c: char| c.is_ascii_alphanumeric() || c == '_'),
        )
            .take()
            .parse_next(input)?;

        // Check for array element syntax: var[index]
        let array_index = if winnow::combinator::opt::<_, _, PError, _>('[')
            .parse_next(input)?
            .is_some()
        {
            // Parse the index (everything until ']')
            let index = winnow::token::take_while(1.., |c: char| c != ']').parse_next(input)?;
            ']'.parse_next(input)?;
            Some(index.to_string())
        } else {
            None
        };

        // Check for optional '+' (append assignment)
        let append = winnow::combinator::opt('+').parse_next(input)?.is_some();

        // Must have '='
        '='.parse_next(input)?;

        // Check if it's an array assignment
        let checkpoint = input.checkpoint();
        let has_paren = winnow::combinator::opt::<_, _, PError, _>('(')
            .parse_next(input)?
            .is_some();

        if has_paren {
            // Parse array elements
            let mut elements = Vec::new();
            let mut full_word = String::with_capacity(var_name.len() + 16);
            full_word.push_str(var_name);
            if append {
                full_word.push_str("+=(");
            } else {
                full_word.push_str("=(");
            }

            loop {
                // Inside array literals, newlines act as whitespace separators
                // (just like spaces/tabs). Consume all whitespace including newlines.
                char_parsers::array_spaces().parse_next(input)?;

                // Check for closing paren
                if winnow::combinator::opt::<_, _, PError, _>(')')
                    .parse_next(input)?
                    .is_some()
                {
                    full_word.push(')');
                    break;
                }

                // Parse element
                let elem = array_element(ctx).parse_next(input)?;

                // Add to full_word
                if !elements.is_empty() {
                    full_word.push(' ');
                }
                if let Some(ref index) = elem.0 {
                    full_word.push('[');
                    full_word.push_str(&index.value);
                    full_word.push_str("]=");
                }
                full_word.push_str(&elem.1.value);

                elements.push(elem);
            }

            let end_offset = tracker.offset_from_locating(input);
            let loc = tracker.range_to_span(start_offset..end_offset);

            let assignment = ast::Assignment {
                name: ast::AssignmentName::VariableName(var_name.to_string()),
                value: ast::AssignmentValue::Array(elements),
                append,
                loc,
            };

            return Ok((assignment, ast::Word::new(&full_word)));
        }

        // Not an array, reset and parse scalar value
        input.reset(&checkpoint);

        // Parse the value using proper word parsing that handles quotes, escapes, etc.
        // The value can be empty (e.g., x=), so use opt
        let value_word =
            winnow::combinator::opt(word_parsers::word_as_ast(ctx, tracker)).parse_next(input)?;
        let value_str = value_word.as_ref().map_or("", |w| w.value.as_str());

        // Construct the full assignment word for AST
        let mut full_word = String::with_capacity(var_name.len() + value_str.len() + 10);
        full_word.push_str(var_name);
        if let Some(ref idx) = array_index {
            full_word.push('[');
            full_word.push_str(idx);
            full_word.push(']');
        }
        if append {
            full_word.push_str("+=");
        } else {
            full_word.push('=');
        }
        full_word.push_str(value_str);

        let end_offset = tracker.offset_from_locating(input);
        let loc = tracker.range_to_span(start_offset..end_offset);

        // Use ArrayElementName if we have an index, otherwise VariableName
        let name = if let Some(idx) = array_index {
            ast::AssignmentName::ArrayElementName(var_name.to_string(), idx)
        } else {
            ast::AssignmentName::VariableName(var_name.to_string())
        };

        let assignment = ast::Assignment {
            name,
            value: ast::AssignmentValue::Scalar(ast::Word::new(value_str)),
            append,
            loc,
        };

        let word = ast::Word::new(&full_word);

        Ok((assignment, word))
    }
}

/// Parse `cmd_prefix` (assignments and redirects before command name)
/// Corresponds to: peg.rs `cmd_prefix()`
pub fn cmd_prefix<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::CommandPrefix, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        repeat::<_, _, Vec<_>, _, _>(
            1..,
            winnow::combinator::terminated(
                winnow::combinator::alt((
                    redirection_parsers::io_redirect(ctx, tracker)
                        .map(|r| ast::CommandPrefixOrSuffixItem::IoRedirect(r.redirect)),
                    assignment_word(ctx, tracker).map(|(assignment, word)| {
                        ast::CommandPrefixOrSuffixItem::AssignmentWord(assignment, word)
                    }),
                )),
                char_parsers::spaces(),
            ),
        )
        .map(ast::CommandPrefix)
        .parse_next(input)
    }
}
