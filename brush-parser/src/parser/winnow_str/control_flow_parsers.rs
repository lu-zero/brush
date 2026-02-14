//! Control flow parsers module
//!
//! Contains parsers for control flow functions (Tier 11-13)

use winnow::combinator::repeat;
use winnow::error::ContextError;
use winnow::prelude::*;

use crate::ast;
use crate::parser::winnow_str::context::PositionTracker;
use crate::parser::winnow_str::types::StrStream;
use crate::parser::winnow_str::{context::ParseContext, types::PError};

/// Parse a do group: do ... done
/// Corresponds to: winnow.rs `do_group()`
pub fn do_group<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::DoGroupCommand, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let (list, range) = winnow::combinator::delimited(
            super::keyword("do"),
            super::compound_parsers::compound_list(ctx, tracker), // compound_list handles its own leading linebreak
            super::keyword("done"),
        )
        .with_span()
        .parse_next(input)?;

        Ok(ast::DoGroupCommand {
            list,
            loc: tracker.range_to_span(range),
        })
    }
}

/// Parse an elif clause
fn elif_clause<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::ElseClause, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        super::keyword("elif").parse_next(input)?;
        let condition = super::compound_parsers::compound_list(ctx, tracker).parse_next(input)?;
        super::keyword("then").parse_next(input)?;
        let body = super::compound_parsers::compound_list(ctx, tracker).parse_next(input)?;
        Ok(ast::ElseClause {
            condition: Some(condition),
            body,
        })
    }
}

/// Parse an else clause (final, no condition)
fn else_clause<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::ElseClause, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        super::keyword("else").parse_next(input)?;
        let body = super::compound_parsers::compound_list(ctx, tracker).parse_next(input)?;
        Ok(ast::ElseClause {
            condition: None,
            body,
        })
    }
}

/// Parse an if clause: if ... then ... [elif ... then ...]* [else ...] fi
/// Corresponds to: winnow.rs `if_clause()`
pub fn if_clause<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::IfClauseCommand, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let start_offset = tracker.offset_from_locating(input);

        super::keyword("if").parse_next(input)?;
        let condition = super::compound_parsers::compound_list(ctx, tracker).parse_next(input)?;
        super::keyword("then").parse_next(input)?;
        let then_body = super::compound_parsers::compound_list(ctx, tracker).parse_next(input)?;

        // Parse elif clauses (zero or more)
        let mut elses: Vec<ast::ElseClause> =
            repeat(0.., elif_clause(ctx, tracker)).parse_next(input)?;

        // Parse optional else clause
        if let Ok(else_part) = else_clause(ctx, tracker).parse_next(input) {
            elses.push(else_part);
        }

        super::keyword("fi").parse_next(input)?;

        let end_offset = tracker.offset_from_locating(input);
        let loc = tracker.range_to_span(start_offset..end_offset);

        Ok(ast::IfClauseCommand {
            condition,
            then: then_body,
            elses: if elses.is_empty() { None } else { Some(elses) },
            loc,
        })
    }
}

/// Parse a while clause: while ... do ... done
/// Corresponds to: winnow.rs `while_clause()`
pub fn while_clause<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::WhileOrUntilClauseCommand, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let start_offset = tracker.offset_from_locating(input);

        super::keyword("while").parse_next(input)?;
        let condition = super::compound_parsers::compound_list(ctx, tracker).parse_next(input)?;
        let body = do_group(ctx, tracker).parse_next(input)?;

        let end_offset = tracker.offset_from_locating(input);
        let loc = tracker.range_to_span(start_offset..end_offset);

        Ok(ast::WhileOrUntilClauseCommand(condition, body, loc))
    }
}

/// Parse an until clause: until ... do ... done
/// Corresponds to: winnow.rs `until_clause()`
pub fn until_clause<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::WhileOrUntilClauseCommand, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let start_offset = tracker.offset_from_locating(input);

        super::keyword("until").parse_next(input)?;
        let condition = super::compound_parsers::compound_list(ctx, tracker).parse_next(input)?;
        let body = do_group(ctx, tracker).parse_next(input)?;

        let end_offset = tracker.offset_from_locating(input);
        let loc = tracker.range_to_span(start_offset..end_offset);

        Ok(ast::WhileOrUntilClauseCommand(condition, body, loc))
    }
}

/// Parse a for clause: for var in list; do ... done
/// Corresponds to: winnow.rs `for_clause()`
pub fn for_clause<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::ForClauseCommand, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let start_offset = tracker.offset_from_locating(input);

        super::keyword("for").parse_next(input)?;
        let var_name = super::compound_parsers::name().parse_next(input)?;

        super::line_parsers::linebreak().parse_next(input)?;

        // Optional "in" wordlist
        let values = if super::keyword("in").parse_next(input).is_ok() {
            // Parse space-separated words (preceded by spaces to consume leading space after "in")
            winnow::combinator::opt(winnow::combinator::preceded(
                super::char_parsers::spaces(),
                winnow::combinator::separated(1.., super::word_parsers::word_as_ast(ctx, tracker), super::char_parsers::spaces1()),
            ))
            .parse_next(input)?
        } else {
            None
        };

        super::compound_parsers::sequential_sep().parse_next(input)?;
        let body = do_group(ctx, tracker).parse_next(input)?;

        let end_offset = tracker.offset_from_locating(input);
        let loc = tracker.range_to_span(start_offset..end_offset);

        Ok(ast::ForClauseCommand {
            variable_name: var_name,
            values,
            body,
            loc,
        })
    }
}

/// Parse arithmetic for body (`do_group` or `brace_group`)
/// Corresponds to: winnow.rs `arithmetic_for_body()` and peg.rs `arithmetic_for_body()`
/// Accepts: "; do", "\n do", or just " do" (spaces are consumed by keyword("do"))
fn arithmetic_for_body<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::DoGroupCommand, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        winnow::combinator::alt((
            // Try sequential_sep followed by do_group (for "; do" or "\n do")
            winnow::combinator::preceded(super::compound_parsers::sequential_sep(), do_group(ctx, tracker)),
            // Try do_group directly (for " do" - spaces consumed by keyword)
            do_group(ctx, tracker),
            // Try brace_group (convert to DoGroupCommand)
            super::compound_parsers::brace_group(ctx, tracker).map(|bg| ast::DoGroupCommand {
                list: bg.list,
                loc: bg.loc,
            }),
        ))
        .parse_next(input)
    }
}

/// Parse arithmetic for clause: for (( init; cond; update )) body
/// Corresponds to: winnow.rs `arithmetic_for_clause()`
pub fn arithmetic_for_clause<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::ArithmeticForClauseCommand, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let start_offset = tracker.offset_from_locating(input);

        // Parse "for (("
        super::keyword("for").parse_next(input)?;
        super::char_parsers::spaces().parse_next(input)?;
        '('.parse_next(input)?;
        super::char_parsers::spaces().parse_next(input)?;
        '('.parse_next(input)?;

        // Parse three arithmetic expressions separated by ;
        let initializer = winnow::combinator::opt(arithmetic_expression()).parse_next(input)?;
        super::char_parsers::spaces().parse_next(input)?;
        ';'.parse_next(input)?;

        let condition = winnow::combinator::opt(arithmetic_expression()).parse_next(input)?;
        super::char_parsers::spaces().parse_next(input)?;
        ';'.parse_next(input)?;

        let updater = winnow::combinator::opt(arithmetic_expression()).parse_next(input)?;

        // Parse "))"
        super::char_parsers::spaces().parse_next(input)?;
        ')'.parse_next(input)?;
        ')'.parse_next(input)?;

        // Parse body (arithmetic_for_body handles the sequential_sep)
        let body = arithmetic_for_body(ctx, tracker).parse_next(input)?;

        let end_offset = tracker.offset_from_locating(input);
        let loc = tracker.range_to_span(start_offset..end_offset);

        Ok(ast::ArithmeticForClauseCommand {
            initializer,
            condition,
            updater,
            body,
            loc,
        })
    }
}

/// Parse commands starting with 'for' - either regular for or arithmetic for
/// Corresponds to: winnow.rs `for_or_arithmetic_for()`
pub fn for_or_arithmetic_for<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::CompoundCommand, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        // In POSIX or SH mode, only allow regular for loops
        if ctx.options.posix_mode || ctx.options.sh_mode {
            for_clause(ctx, tracker)
                .map(ast::CompoundCommand::ForClause)
                .parse_next(input)
        } else {
            // In Bash mode, try arithmetic for first, then fall back to regular for
            winnow::combinator::alt((
                // Try arithmetic for first: for ((
                arithmetic_for_clause(ctx, tracker).map(ast::CompoundCommand::ArithmeticForClause),
                // Fall back to regular for
                for_clause(ctx, tracker).map(ast::CompoundCommand::ForClause),
            ))
            .parse_next(input)
        }
    }
}

/// Parse case item terminator (;;, ;&, or ;;&)
fn case_item_terminator<'a>() -> impl Parser<StrStream<'a>, ast::CaseItemPostAction, PError> {
    winnow::combinator::preceded(
        super::char_parsers::spaces(),
        dispatch! {super::char_parsers::peek_op3();
            ";;&" => ";;&".value(ast::CaseItemPostAction::ContinueEvaluatingCases),
            ";&" => ";&".value(ast::CaseItemPostAction::UnconditionallyExecuteNextCaseItem),
            ";;" => ";;".value(ast::CaseItemPostAction::ExitCase),
            _ => fail,
        },
    )
}

/// Parse a case item: pattern) commands ;;
fn case_item<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::CaseItem, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        super::char_parsers::spaces().parse_next(input)?;
        let start_offset = tracker.offset_from_locating(input);

        // Optional leading (
        let _ = winnow::combinator::opt::<_, _, PError, _>('(').parse_next(input)?;
        super::char_parsers::spaces().parse_next(input)?;

        // Parse patterns: word separated by |
        let patterns: Vec<ast::Word> = winnow::combinator::separated(
            1..,
            super::word_parsers::word_as_ast(ctx, tracker),
            winnow::combinator::preceded(super::char_parsers::spaces(), winnow::combinator::terminated('|', super::char_parsers::spaces())),
        )
        .parse_next(input)?;

        super::char_parsers::spaces().parse_next(input)?;
        ')'.parse_next(input)?;

        super::line_parsers::linebreak().parse_next(input)?;

        // Parse body (optional)
        let cmd = winnow::combinator::opt(super::compound_parsers::compound_list(ctx, tracker)).parse_next(input)?;

        // Parse case item terminator (optional - default to ExitCase)
        let post_action = winnow::combinator::opt(case_item_terminator())
            .parse_next(input)?
            .unwrap_or(ast::CaseItemPostAction::ExitCase);

        let end_offset = tracker.offset_from_locating(input);
        let loc = tracker.range_to_span(start_offset..end_offset);

        super::line_parsers::linebreak().parse_next(input)?;

        Ok(ast::CaseItem {
            patterns,
            cmd,
            post_action,
            loc: Some(loc),
        })
    }
}

/// Parse case list (multiple case items until "esac")
fn case_list<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, Vec<ast::CaseItem>, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let mut items = vec![];

        loop {
            // Peek ahead to see if we have "esac"
            let checkpoint = input.checkpoint();
            super::char_parsers::spaces().parse_next(input)?;
            if super::keyword("esac").parse_next(input).is_ok() {
                // Found esac, restore and break
                input.reset(&checkpoint);
                break;
            }
            input.reset(&checkpoint);

            // Parse case item
            match case_item(ctx, tracker).parse_next(input) {
                Ok(item) => items.push(item),
                Err(_) => break,
            }
        }

        if items.is_empty() {
            return Err(winnow::error::ErrMode::Backtrack(ContextError::default()));
        }

        Ok(items)
    }
}

/// Parse a case clause: case word in patterns) commands ;; esac
/// Corresponds to: winnow.rs `case_clause()`
pub fn case_clause<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::CaseClauseCommand, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let start_offset = tracker.offset_from_locating(input);

        super::keyword("case").parse_next(input)?;
        super::char_parsers::spaces().parse_next(input)?;
        let target = super::word_parsers::word_as_ast(ctx, tracker).parse_next(input)?;

        super::line_parsers::linebreak().parse_next(input)?;
        super::keyword("in").parse_next(input)?;
        super::line_parsers::linebreak().parse_next(input)?;

        // Use opt() for optional case list
        let items = winnow::combinator::opt(case_list(ctx, tracker)).parse_next(input)?;

        super::char_parsers::spaces().parse_next(input)?;
        super::keyword("esac").parse_next(input)?;

        let end_offset = tracker.offset_from_locating(input);
        let loc = tracker.range_to_span(start_offset..end_offset);

        Ok(ast::CaseClauseCommand {
            value: target,
            cases: items.unwrap_or_default(),
            loc,
        })
    }
}

/// Parse arithmetic expression inside (( ))
/// Collects all content until ")) or ; (at depth 0) is found, tracking paren depth
/// Corresponds to: winnow.rs `arithmetic_expression()`
fn arithmetic_expression<'a>() -> impl Parser<StrStream<'a>, ast::UnexpandedArithmeticExpr, PError>
{
    move |input: &mut StrStream<'a>| {
        let mut expr_str = String::new();
        let mut paren_depth = 0;

        loop {
            // Check for end at depth 0
            if paren_depth == 0 {
                let checkpoint = input.checkpoint();
                // Skip optional spaces to peek ahead
                super::char_parsers::spaces().parse_next(input)?;

                // Check for "))" - allow optional space between to match peg tokenizer behavior
                if winnow::combinator::opt::<_, _, PError, _>((')', super::char_parsers::spaces(), ')'))
                    .parse_next(input)?
                    .is_some()
                {
                    input.reset(&checkpoint);
                    break;
                }

                // Check for ";" (for arithmetic for loops)
                if winnow::combinator::opt::<_, _, PError, _>(';')
                    .parse_next(input)?
                    .is_some()
                {
                    input.reset(&checkpoint);
                    break;
                }

                input.reset(&checkpoint);
            }

            // Get next character
            let checkpoint = input.checkpoint();

            // Try to match '('
            if winnow::combinator::opt::<_, _, PError, _>('(')
                .parse_next(input)?
                .is_some()
            {
                paren_depth += 1;
                expr_str.push('(');
                continue;
            }
            input.reset(&checkpoint);

            // Try to match ')'
            if winnow::combinator::opt::<_, _, PError, _>(')')
                .parse_next(input)?
                .is_some()
            {
                paren_depth -= 1;
                expr_str.push(')');
                continue;
            }
            input.reset(&checkpoint);

            // Match any other character that's not )) or ;
            let c_opt: Result<char, PError> = winnow::token::any.parse_next(input);
            if let Ok(c) = c_opt {
                expr_str.push(c);
            } else {
                break;
            }
        }

        Ok(ast::UnexpandedArithmeticExpr {
            value: normalize_arithmetic_expr(&expr_str),
        })
    }
}

/// Normalize an arithmetic expression string to match peg parser output.
/// The peg parser uses tokenizer which treats some characters as operators (like <, >, |, &)
/// and others as word characters (like =, +, -, *, /).
/// Spaces are only preserved between adjacent word tokens.
fn normalize_arithmetic_expr(s: &str) -> String {
    // Shell operators in arithmetic context (matches tokenizer's is_operator list)
    const SHELL_OPERATORS: &[char] = &['<', '>', '|', '&', '(', ')', ';'];

    let s = s.trim();
    let mut result = String::with_capacity(s.len());
    let mut last_was_word = false;
    let mut pending_space = false;

    for c in s.chars() {
        if c.is_whitespace() {
            // Mark that we saw a space, but don't emit yet
            pending_space = true;
            continue;
        }

        // Shell operators suppress spaces around them
        let is_shell_op = SHELL_OPERATORS.contains(&c);

        if is_shell_op {
            // Operators don't get spaces around them
            pending_space = false;
            last_was_word = false;
        } else {
            // Non-operator: emit pending space if last was also non-operator
            if pending_space && last_was_word {
                result.push(' ');
            }
            pending_space = false;
            last_was_word = true;
        }

        result.push(c);
    }

    result
}

/// Parse arithmetic command (( expr ))
/// Corresponds to: winnow.rs `arithmetic_command()`
pub fn arithmetic_command<'a>(
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::ArithmeticCommand, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let start_offset = tracker.offset_from_locating(input);

        // Parse (( - allow optional whitespace between them to match peg tokenizer behavior
        // (the tokenizer produces separate ( tokens even with spaces)
        '('.parse_next(input)?;
        super::char_parsers::spaces().parse_next(input)?;
        '('.parse_next(input)?;

        // Parse expression
        let expr = arithmetic_expression().parse_next(input)?;

        // Parse )) - allow optional whitespace between them
        super::char_parsers::spaces().parse_next(input)?;
        ')'.parse_next(input)?;
        super::char_parsers::spaces().parse_next(input)?;
        ')'.parse_next(input)?;

        let end_offset = tracker.offset_from_locating(input);
        let loc = tracker.range_to_span(start_offset..end_offset);

        Ok(ast::ArithmeticCommand { expr, loc })
    }
}