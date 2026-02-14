//! Extended test parsers module
//!
//! Contains parsers for extended test expressions [[ ]] (Tier 17)

use winnow::combinator::repeat;
use winnow::error::ContextError;
use winnow::prelude::*;
use winnow::token::take_while;

use crate::ast;
use crate::parser::winnow_str::context::PositionTracker;
use crate::parser::winnow_str::types::StrStream;
use crate::parser::winnow_str::{char_parsers::comment, context::ParseContext, types::PError};

/// Parse whitespace inside extended test [[ ]] expressions.
/// Unlike `spaces()`, this also handles newlines and backslash-newline continuations,
/// because bash allows multi-line [[ ]] expressions.
#[inline]
fn ext_test_spaces<'a>() -> impl Parser<StrStream<'a>, (), PError> {
    repeat::<_, _, (), _, _>(
        0..,
        winnow::combinator::alt((
            take_while(1.., |c: char| c == ' ' || c == '\t' || c == '\n').void(),
            ("\\", '\n').void(), // backslash-newline continuation
            comment(),           // # comments
        )),
    )
    .void()
}

/// Parse a unary test operator (-f, -z, -n, etc.)
/// Corresponds to: winnow.rs `parse_unary_operator()`
fn parse_unary_operator(op: &str) -> Option<ast::UnaryPredicate> {
    use ast::UnaryPredicate;
    match op {
        "-e" => Some(UnaryPredicate::FileExists),
        "-b" => Some(UnaryPredicate::FileExistsAndIsBlockSpecialFile),
        "-c" => Some(UnaryPredicate::FileExistsAndIsCharSpecialFile),
        "-d" => Some(UnaryPredicate::FileExistsAndIsDir),
        "-f" => Some(UnaryPredicate::FileExistsAndIsRegularFile),
        "-g" => Some(UnaryPredicate::FileExistsAndIsSetgid),
        "-h" | "-L" => Some(UnaryPredicate::FileExistsAndIsSymlink),
        "-k" => Some(UnaryPredicate::FileExistsAndHasStickyBit),
        "-p" => Some(UnaryPredicate::FileExistsAndIsFifo),
        "-r" => Some(UnaryPredicate::FileExistsAndIsReadable),
        "-s" => Some(UnaryPredicate::FileExistsAndIsNotZeroLength),
        "-t" => Some(UnaryPredicate::FdIsOpenTerminal),
        "-u" => Some(UnaryPredicate::FileExistsAndIsSetuid),
        "-w" => Some(UnaryPredicate::FileExistsAndIsWritable),
        "-x" => Some(UnaryPredicate::FileExistsAndIsExecutable),
        "-G" => Some(UnaryPredicate::FileExistsAndOwnedByEffectiveGroupId),
        "-N" => Some(UnaryPredicate::FileExistsAndModifiedSinceLastRead),
        "-O" => Some(UnaryPredicate::FileExistsAndOwnedByEffectiveUserId),
        "-S" => Some(UnaryPredicate::FileExistsAndIsSocket),
        "-o" => Some(UnaryPredicate::ShellOptionEnabled),
        "-v" => Some(UnaryPredicate::ShellVariableIsSetAndAssigned),
        "-R" => Some(UnaryPredicate::ShellVariableIsSetAndNameRef),
        "-z" => Some(UnaryPredicate::StringHasZeroLength),
        "-n" => Some(UnaryPredicate::StringHasNonZeroLength),
        _ => None,
    }
}

/// Parse a binary test operator (=, !=, -eq, -lt, etc.)
/// Corresponds to: winnow.rs `parse_binary_operator()`
fn parse_binary_operator(op: &str) -> Option<ast::BinaryPredicate> {
    use ast::BinaryPredicate;
    match op {
        "=" | "==" => Some(BinaryPredicate::StringExactlyMatchesPattern),
        "!=" => Some(BinaryPredicate::StringDoesNotExactlyMatchPattern),
        "<" => Some(BinaryPredicate::LeftSortsBeforeRight),
        ">" => Some(BinaryPredicate::LeftSortsAfterRight),
        "-eq" => Some(BinaryPredicate::ArithmeticEqualTo),
        "-ne" => Some(BinaryPredicate::ArithmeticNotEqualTo),
        "-lt" => Some(BinaryPredicate::ArithmeticLessThan),
        "-le" => Some(BinaryPredicate::ArithmeticLessThanOrEqualTo),
        "-gt" => Some(BinaryPredicate::ArithmeticGreaterThan),
        "-ge" => Some(BinaryPredicate::ArithmeticGreaterThanOrEqualTo),
        "-nt" => Some(BinaryPredicate::LeftFileIsNewerOrExistsWhenRightDoesNot),
        "-ot" => Some(BinaryPredicate::LeftFileIsOlderOrDoesNotExistWhenRightDoes),
        "-ef" => Some(BinaryPredicate::FilesReferToSameDeviceAndInodeNumbers),
        "=~" => Some(BinaryPredicate::StringMatchesRegex),
        _ => None,
    }
}

/// Parse a word in extended test context (bare word or quoted string with quotes preserved)
fn ext_test_word<'a>(
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::Word, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let start_offset = tracker.offset_from_locating(input);
        // Collect a word that may consist of multiple adjacent segments:
        // quoted strings, bare characters, and expansions — without any
        // whitespace in between.  For example: "declare -a"* is one word
        // with a double-quoted segment followed by a bare glob character.
        let mut word = String::new();

        while let Ok(ch) = super::char_parsers::peek_char().parse_next(input) {
            match ch {
                // Single-quoted segment: capture with quotes
                '\'' => {
                    let quote: char = '\''.parse_next(input)?;
                    let content: &str = take_while(0.., |c: char| c != '\'').parse_next(input)?;
                    let end_quote: char = '\''.parse_next(input)?;
                    word.push(quote);
                    word.push_str(content);
                    word.push(end_quote);
                }
                // Double-quoted segment: capture with quotes, handling escapes
                '"' => {
                    let s = super::word_parsers::double_quoted_string().parse_next(input)?;
                    word.push_str(&s);
                }
                // Stop on whitespace
                ' ' | '\t' | '\n' => break,
                // Backslash escape: consume \ and next char
                '\\' => {
                    winnow::token::any.parse_next(input)?;
                    let escaped: Result<char, PError> = winnow::token::any.parse_next(input);
                    if let Ok(c) = escaped {
                        if c == '\n' {
                            // Backslash-newline is line continuation — skip both
                        } else {
                            word.push('\\');
                            word.push(c);
                        }
                    } else {
                        word.push('\\');
                    }
                }
                // $ starts expansions that may contain parentheses
                '$' => {
                    word.push('$');
                    winnow::token::any.parse_next(input)?;
                    match super::char_parsers::peek_char().parse_next(input).ok() {
                        Some('(') => {
                            winnow::token::any.parse_next(input)?;
                            // Check for $(( arithmetic )) vs $( command )
                            if super::char_parsers::peek_char().parse_next(input).ok() == Some('(') {
                                // $(( ... )) — arithmetic expansion
                                word.push('(');
                                word.push('(');
                                winnow::token::any.parse_next(input)?;
                                // Consume balanced parentheses
                                let mut depth: u32 = 1;
                                while depth > 0 {
                                    let ch = winnow::token::any.parse_next(input)?;
                                    word.push(ch);
                                    match ch {
                                        '(' => depth += 1,
                                        ')' => depth -= 1,
                                        '\\' => {
                                            // Escape: consume next char too
                                            let escaped: Result<char, PError> = winnow::token::any.parse_next(input);
                                            if let Ok(c) = escaped {
                                                word.push(c);
                                            }
                                        }
                                        '\'' => {
                                            // Single-quoted string: consume until closing quote
                                            loop {
                                                let c = winnow::token::any.parse_next(input)?;
                                                word.push(c);
                                                if c == '\'' {
                                                    break;
                                                }
                                            }
                                        }
                                        '"' => {
                                            // Double-quoted string: consume until closing quote, handling escapes
                                            loop {
                                                let c = winnow::token::any.parse_next(input)?;
                                                word.push(c);
                                                if c == '"' {
                                                    break;
                                                }
                                                if c == '\\' {
                                                    let escaped: Result<char, PError> = winnow::token::any.parse_next(input);
                                                    if let Ok(ec) = escaped {
                                                        word.push(ec);
                                                    }
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                                // Consume the second closing )
                                if super::char_parsers::peek_char().parse_next(input).ok() == Some(')') {
                                    word.push(')');
                                    winnow::token::any.parse_next(input)?;
                                }
                            } else {
                                // $( ... ) — command substitution
                                word.push('(');
                                // Consume balanced parentheses
                                let mut depth: u32 = 1;
                                while depth > 0 {
                                    let ch = winnow::token::any.parse_next(input)?;
                                    word.push(ch);
                                    match ch {
                                        '(' => depth += 1,
                                        ')' => depth -= 1,
                                        '\\' => {
                                            // Escape: consume next char too
                                            let escaped: Result<char, PError> = winnow::token::any.parse_next(input);
                                            if let Ok(c) = escaped {
                                                word.push(c);
                                            }
                                        }
                                        '\'' => {
                                            // Single-quoted string: consume until closing quote
                                            loop {
                                                let c = winnow::token::any.parse_next(input)?;
                                                word.push(c);
                                                if c == '\'' {
                                                    break;
                                                }
                                            }
                                        }
                                        '"' => {
                                            // Double-quoted string: consume until closing quote, handling escapes
                                            loop {
                                                let c = winnow::token::any.parse_next(input)?;
                                                word.push(c);
                                                if c == '"' {
                                                    break;
                                                }
                                                if c == '\\' {
                                                    let escaped: Result<char, PError> = winnow::token::any.parse_next(input);
                                                    if let Ok(ec) = escaped {
                                                        word.push(ec);
                                                    }
                                                }
                                            }
                                        }
                                        _ => {}
                                    }
                                }
                            }
                        }
                        Some('{') => {
                            // ${ ... } — braced variable
                            word.push('{');
                            winnow::token::any.parse_next(input)?;
                            // Consume balanced braces
                            let mut depth: u32 = 1;
                            while depth > 0 {
                                let ch = winnow::token::any.parse_next(input)?;
                                word.push(ch);
                                match ch {
                                    '{' => depth += 1,
                                    '}' => depth -= 1,
                                    '\\' => {
                                        // Escape: consume next char too
                                        let escaped: Result<char, PError> = winnow::token::any.parse_next(input);
                                        if let Ok(c) = escaped {
                                            word.push(c);
                                        }
                                    }
                                    '\'' => {
                                        // Single-quoted string: consume until closing quote
                                        loop {
                                            let c = winnow::token::any.parse_next(input)?;
                                            word.push(c);
                                            if c == '\'' {
                                                break;
                                            }
                                        }
                                    }
                                    '"' => {
                                        // Double-quoted string: consume until closing quote, handling escapes
                                        loop {
                                            let c = winnow::token::any.parse_next(input)?;
                                            word.push(c);
                                            if c == '"' {
                                                break;
                                            }
                                            if c == '\\' {
                                                let escaped: Result<char, PError> = winnow::token::any.parse_next(input);
                                                if let Ok(ec) = escaped {
                                                    word.push(ec);
                                                }
                                            }
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        _ => {
                            // $var or $!, $?, etc. — already consumed $
                        }
                    }
                }
                // Stop on bare parentheses (used for grouping in [[ ]])
                '(' | ')' => break,
                // &, |, ! are special characters
                '&' | '|' | '!' => {
                    let checkpoint = input.checkpoint();
                    winnow::token::any.parse_next(input)?;
                    if super::char_parsers::peek_char().parse_next(input).ok() == Some(ch) {
                        // This is &&, ||, or !!
                        input.reset(&checkpoint);
                        break;
                    } else {
                        // Single &, |, or !
                        input.reset(&checkpoint);
                        word.push(ch);
                        winnow::token::any.parse_next(input)?;
                    }
                }
                // Any other character is part of the word
                _ => {
                    word.push(ch);
                    winnow::token::any.parse_next(input)?;
                }
            }
        }

        if word.is_empty() {
            Err(winnow::error::ErrMode::Backtrack(ContextError::default()))
        } else {
            let end_offset = tracker.offset_from_locating(input);
            let loc = tracker.range_to_span(start_offset..end_offset);
            Ok(ast::Word {
                value: word,
                loc: Some(loc),
            })
        }
    }
}

/// Parse extended test command: [[ expression ]]
/// Corresponds to: winnow.rs `extended_test_command()`
pub fn extended_test_command<'a>(
    _ctx: &'a ParseContext<'a>,
    tracker: &'a PositionTracker,
) -> impl Parser<StrStream<'a>, ast::ExtendedTestExprCommand, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        let start_offset = tracker.offset_from_locating(input);

        // Parse [[
        '['.parse_next(input)?;
        '['.parse_next(input)?;

        ext_test_spaces().parse_next(input)?;

        // For now, parse the expression using a simple approach
        // TODO: Implement full extended test expression parsing
        let word = ext_test_word(tracker).parse_next(input)?;

        ext_test_spaces().parse_next(input)?;

        // Parse ]]
        ']'.parse_next(input)?;
        ']'.parse_next(input)?;

        let end_offset = tracker.offset_from_locating(input);
        let loc = tracker.range_to_span(start_offset..end_offset);

        // Create a simple unary test expression for now
        let expr = ast::ExtendedTestExpr::UnaryTest(
            ast::UnaryPredicate::StringHasNonZeroLength,
            word
        );

        Ok(ast::ExtendedTestExprCommand { expr, loc })
    }
}