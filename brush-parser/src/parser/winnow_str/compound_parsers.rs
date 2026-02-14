//! Compound parsers module
//!
//! Contains parsers for compound command functions (Tier 10)

use winnow::combinator::{dispatch, fail, repeat};
use winnow::error::ContextError;
use winnow::prelude::*;

use crate::ast;
use crate::parser::winnow_str::context::PositionTracker;
use crate::parser::winnow_str::types::StrStream;
use crate::parser::winnow_str::{context::ParseContext, types::PError};

/// Parse a compound list (used inside subshells, brace groups, etc.)
///
/// Similar to `complete_command` but with optional leading linebreaks and more flexible separators
/// Corresponds to: winnow.rs `compound_list()`
pub fn compound_list<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::CompoundList, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        // Optional leading linebreaks
        super::line_parsers::linebreak().parse_next(input)?;

        // Parse first and_or (required)
        let mut current_ao = super::and_or_parsers::and_or(ctx, tracker).parse_next(input)?;
        let mut items: Vec<ast::CompoundListItem> = vec![];

        // Try to parse (separator + and_or) pairs
        // Note: Manual loop is faster than repeat() combinator here due to early break optimization
        loop {
            // Try to get separator after current and_or (handles both ; & and newlines)
            super::char_parsers::spaces().parse_next(input)?;

            let sep_opt = if let Ok(sep_opt) = super::line_parsers::separator().parse_next(input) {
                super::char_parsers::spaces().parse_next(input)?;
                sep_opt
            } else {
                // No separator - add current and_or with default separator and we're done
                items.push(ast::CompoundListItem(
                    current_ao,
                    ast::SeparatorOperator::Sequence,
                ));
                break;
            };

            // Convert Option<SeparatorOperator> to SeparatorOperator (None means newline, treat as
            // Sequence)
            let sep = sep_opt.unwrap_or(ast::SeparatorOperator::Sequence);

            // Push current and_or with its separator
            items.push(ast::CompoundListItem(current_ao, sep));

            // We have a separator, check if there's another and_or after it
            if let Ok(next_ao) = super::and_or_parsers::and_or(ctx, tracker).parse_next(input) {
                // Move to next
                current_ao = next_ao;
            } else {
                // Trailing separator
                break;
            }
        }

        Ok(ast::CompoundList(items))
    }
}

/// Parse a subshell: ( commands )
/// Corresponds to: winnow.rs `subshell()`
pub fn subshell<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::SubshellCommand, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let (list, range) = winnow::combinator::delimited(
            ('(', super::char_parsers::spaces(), super::line_parsers::linebreak()),
            compound_list(ctx, tracker),
            (super::line_parsers::linebreak(), super::char_parsers::spaces(), ')'),
        )
        .with_span()
        .parse_next(input)?;

        Ok(ast::SubshellCommand {
            list,
            loc: tracker.range_to_span(range),
        })
    }
}

/// Parse a brace group: { commands; }
/// Corresponds to: winnow.rs `brace_group()`
pub fn brace_group<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::BraceGroupCommand, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let (list, range) = winnow::combinator::delimited(
            // IMPORTANT: Require at least one space OR newline after '{'
            // This distinguishes brace groups from brace expansion:
            // - Brace group: { echo hello; } (requires space after {)
            // - Brace expansion: {1..10} (no space, part of word)
            (
                '{',
                winnow::combinator::alt((
                    super::char_parsers::spaces1(),        // At least one space/tab
                    super::line_parsers::newline().void(), // Or a newline
                )),
            ),
            compound_list(ctx, tracker),
            // Before '}': optional linebreak and spaces
            // Note: A separator (;/&) or newline is required before }, but that's
            // handled by compound_list. We just allow optional additional whitespace.
            (super::line_parsers::linebreak(), super::char_parsers::spaces(), '}'),
        )
        .with_span()
        .parse_next(input)?;

        Ok(ast::BraceGroupCommand {
            list,
            loc: tracker.range_to_span(range),
        })
    }
}

/// Parse process substitution: <(command) or >(command)
/// Corresponds to: peg.rs `process_substitution()`
pub fn process_substitution<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, (ast::ProcessSubstitutionKind, ast::SubshellCommand), PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let start_offset = tracker.offset_from_locating(input);

        // Parse < or > to determine the kind
        let kind = winnow::combinator::alt((
            "<".value(ast::ProcessSubstitutionKind::Read),
            ">".value(ast::ProcessSubstitutionKind::Write),
        ))
        .parse_next(input)?;

        // Then parse the subshell-like content: ( compound_list )
        let list = winnow::combinator::delimited(
            ('(', super::char_parsers::spaces(), super::line_parsers::linebreak()),
            compound_list(ctx, tracker),
            (super::line_parsers::linebreak(), super::char_parsers::spaces(), ')'),
        )
        .parse_next(input)?;

        let end_offset = tracker.offset_from_locating(input);
        let loc = tracker.range_to_span(start_offset..end_offset);

        Ok((kind, ast::SubshellCommand { list, loc }))
    }
}

/// Parse a sequential separator (semicolon or newlines)
/// Corresponds to: winnow.rs `sequential_sep()`
#[inline]
pub fn sequential_sep<'a>() -> impl Parser<StrStream<'a>, (), PError> {
    winnow::combinator::alt(((';', super::line_parsers::linebreak()).void(), super::line_parsers::newline_list().void()))
}

/// Check if a string is a valid shell variable name
/// Names must start with [a-zA-Z_] and contain only [a-zA-Z0-9_]
fn is_valid_name(s: &str) -> bool {
    if s.is_empty() {
        return false;
    }

    let mut chars = s.chars();
    let first = chars.next().unwrap();

    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }

    chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Parse a valid variable name
/// Corresponds to: winnow.rs `name()`
pub fn name<'a>() -> impl Parser<StrStream<'a>, String, PError> {
    winnow::combinator::preceded(super::char_parsers::spaces(), super::word_parsers::bare_word())
        .verify(|s: &str| is_valid_name(s))
        .map(|s: &str| s.to_string())
}

/// Parse a do group: do ... done
/// Corresponds to: winnow.rs `do_group()`
pub fn do_group<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::DoGroupCommand, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let (list, range) = winnow::combinator::delimited(
            super::super::keyword("do"),
            compound_list(ctx, tracker), // compound_list handles its own leading linebreak
            super::super::keyword("done"),
        )
        .with_span()
        .parse_next(input)?;

        Ok(ast::DoGroupCommand {
            list,
            loc: tracker.range_to_span(range),
        })
    }
}

/// Parse a compound command - tries all compound command types
/// Corresponds to: winnow.rs `compound_command()`
pub fn compound_command<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::CompoundCommand, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        winnow::combinator::preceded(
            super::char_parsers::spaces(),
            dispatch! {super::char_parsers::peek_char();
                '{' => brace_group(ctx, tracker).map(ast::CompoundCommand::BraceGroup),
                '(' => paren_compound(ctx, tracker),  // Handles both (( )) arithmetic and ( ) subshell
                'f' => for_or_arithmetic_for(ctx, tracker),  // Handles both for (( )) and for name in
                'c' => case_clause(ctx, tracker).map(ast::CompoundCommand::CaseClause),
                'i' => if_clause(ctx, tracker).map(ast::CompoundCommand::IfClause),
                'w' => while_clause(ctx, tracker).map(ast::CompoundCommand::WhileClause),
                'u' => until_clause(ctx, tracker).map(ast::CompoundCommand::UntilClause),
                _ => fail,
            },
        )
        .parse_next(input)
    }
}

/// Parse commands starting with '(' - either arithmetic (( )) or subshell ( )
/// Corresponds to: winnow.rs `paren_compound()`
pub fn paren_compound<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::CompoundCommand, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        // In POSIX or SH mode, only allow subshells (no arithmetic commands)
        if ctx.options.posix_mode || ctx.options.sh_mode {
            subshell(ctx, tracker)
                .map(ast::CompoundCommand::Subshell)
                .parse_next(input)
        } else {
            // In Bash mode, try arithmetic command (( first, then fall back to subshell
            winnow::combinator::alt((
                // Try (( first for arithmetic
                arithmetic_command(tracker).map(ast::CompoundCommand::Arithmetic),
                // Fall back to subshell
                subshell(ctx, tracker).map(ast::CompoundCommand::Subshell),
            ))
            .parse_next(input)
        }
    }
}