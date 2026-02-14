//! Redirection parsers module
//!
//! Contains parsers for I/O redirections (Tier 8)

use winnow::combinator::{dispatch, fail, repeat};
use winnow::error::ContextError;
use winnow::prelude::*;

use crate::ast;
use crate::parser::winnow_str::{char_parsers, context::ParseContext, types::PError, word_parsers};
use crate::parser::winnow_str::types::StrStream;

/// Parse an I/O file descriptor number
pub fn io_number<'a>() -> impl Parser<StrStream<'a>, i32, PError> {
    winnow::ascii::dec_uint::<_, u16, _>.map(i32::from)
}

/// Parse redirect operator and return the redirect kind
/// Corresponds to: winnow.rs `io_file()` dispatcher
fn redirect_operator<'a>() -> impl Parser<StrStream<'a>, ast::IoFileRedirectKind, PError> {
    dispatch! {super::char_parsers::peek_op2();
        ">>" => ">>".value(ast::IoFileRedirectKind::Append),
        "<>" => "<>".value(ast::IoFileRedirectKind::ReadAndWrite),
        ">|" => ">|".value(ast::IoFileRedirectKind::Clobber),
        ">&" => ">&".value(ast::IoFileRedirectKind::DuplicateOutput),
        "<&" => "<&".value(ast::IoFileRedirectKind::DuplicateInput),
        ">" => ">".value(ast::IoFileRedirectKind::Write),
        "<" => "<".value(ast::IoFileRedirectKind::Read),
        _ => fail,
    }
}

/// Parse a here-document delimiter, handling quotes
/// Returns (`delimiter_text`, `requires_expansion`)
/// Returns (`raw_delimiter`, `match_delimiter`, `requires_expansion`)
/// `raw_delimiter`: as written (includes quotes for `here_end`)
/// `match_delimiter`: stripped of quotes (for matching content)
fn here_document_delimiter<'a>() -> impl Parser<StrStream<'a>, (String, String, bool), PError> {
    move |input: &mut StrStream<'a>| {
        let mut raw_delimiter = String::new();
        let mut match_delimiter = String::new();
        let mut quoted = false;
        let mut done = false;

        while !done && !input.is_empty() {
            let checkpoint = input.checkpoint();

            // Check for whitespace or newline (end of delimiter)
            if let Ok(_ch) =
                winnow::token::one_of::<_, _, PError>([' ', '\t', '\n']).parse_next(input)
            {
                input.reset(&checkpoint);
                break;
            }
            input.reset(&checkpoint);

            // Try to parse a character
            let ch: char = winnow::token::any.parse_next(input)?;
            raw_delimiter.push(ch);

            match ch {
                '\'' | '"' => {
                    quoted = true;
                    // Don't include quotes in match delimiter
                }
                '\\' => {
                    quoted = true;
                    // Consume next character
                    if let Ok(next_ch) = winnow::token::any::<_, PError>.parse_next(input) {
                        raw_delimiter.push(next_ch);
                        match_delimiter.push(next_ch);
                    }
                }
                ' ' | '\t' | '\n' => {
                    // End of delimiter
                    done = true;
                }
                _ => {
                    match_delimiter.push(ch);
                }
            }
        }

        if match_delimiter.is_empty() {
            return Err(winnow::error::ErrMode::Backtrack(ContextError::default()));
        }

        let requires_expansion = !quoted;
        Ok((raw_delimiter, match_delimiter, requires_expansion))
    }
}

/// Parse here-document content until delimiter is found
/// Returns the content as a Word
fn here_document_content(
    input: &mut StrStream<'_>,
    delimiter: &str,
    remove_tabs: bool,
    tracker: &ParseContext<'_>,
) -> Result<ast::Word, PError> {
    let start_offset = tracker.offset_from_locating(input);
    let mut content = String::new();
    let mut at_line_start = true;

    loop {
        // Check if we're at a line that matches the delimiter
        if at_line_start {
            let checkpoint = input.checkpoint();

            // Skip leading tabs if remove_tabs is true (for both delimiter and content)
            if remove_tabs {
                let _: Result<&str, PError> =
                    winnow::token::take_while(0.., '\t').parse_next(input);
            }

            // Try to match delimiter
            if let Ok(line_content) =
                winnow::token::take_while::<_, _, PError>(0.., |c| c != '\n').parse_next(input)
            {
                if line_content == delimiter {
                    // Do NOT consume the newline after the delimiter — it serves
                    // as the command separator so that complete_command_continuation
                    // can find the next command on the following line.
                    let end_offset = tracker.offset_from_locating(input);
                    let loc = tracker.range_to_span(start_offset..end_offset);
                    return Ok(ast::Word {
                        value: content,
                        loc: Some(loc),
                    });
                }
            }

            // Not the delimiter, reset to get full line
            input.reset(&checkpoint);

            // If remove_tabs, skip leading tabs from content too
            if remove_tabs {
                let _: Result<&str, PError> =
                    winnow::token::take_while(0.., '\t').parse_next(input);
            }
        }

        // Collect this line's content
        at_line_start = false;

        if input.is_empty() {
            // Unterminated here-document
            return Err(winnow::error::ErrMode::Backtrack(ContextError::default()));
        }

        let ch: char = winnow::token::any.parse_next(input)?;
        content.push(ch);

        if ch == '\n' {
            at_line_start = true;
        }
    }
}

/// A pending here-document that has been parsed but content not yet resolved
#[derive(Debug)]
struct PendingHereDoc {
    fd: Option<i32>,
    remove_tabs: bool,
    requires_expansion: bool,
    raw_delimiter: String,
    match_delimiter: String,
}

/// Parse just the here-document marker (operator and delimiter), without consuming content.
/// This is used to collect all markers on a line before resolving content.
fn here_document_marker<'a>() -> impl Parser<StrStream<'a>, PendingHereDoc, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        // Optional fd number
        let fd = winnow::combinator::opt(io_number()).parse_next(input)?;

        // Parse operator (<<- or <<)
        let remove_tabs = if winnow::combinator::opt("<<-").parse_next(input)?.is_some() {
            true
        } else if winnow::combinator::opt("<<").parse_next(input)?.is_some() {
            false
        } else {
            return Err(winnow::error::ErrMode::Backtrack(ContextError::default()));
        };

        // Skip optional spaces between operator and delimiter (e.g., <<- EOF)
        let _: &str =
            winnow::token::take_while(0.., |c: char| c == ' ' || c == '\t').parse_next(input)?;

        // Parse delimiter - raw_delimiter preserves quotes, match_delimiter is stripped
        let (raw_delimiter, match_delimiter, requires_expansion) =
            here_document_delimiter().parse_next(input)?;

        Ok(PendingHereDoc {
            fd,
            remove_tabs,
            requires_expansion,
            raw_delimiter,
            match_delimiter,
        })
    }
}

/// Resolve a pending here-document by parsing its content from the input.
fn resolve_here_document(
    input: &mut StrStream<'_>,
    pending: PendingHereDoc,
    tracker: &ParseContext<'_>,
) -> Result<(Option<i32>, ast::IoHereDocument), winnow::error::ErrMode<ContextError>> {
    let doc = here_document_content(
        input,
        &pending.match_delimiter,
        pending.remove_tabs,
        tracker,
    )?;

    Ok((
        pending.fd,
        ast::IoHereDocument {
            remove_tabs: pending.remove_tabs,
            requires_expansion: pending.requires_expansion,
            here_end: ast::Word::from(pending.raw_delimiter),
            doc,
        },
    ))
}

/// Parse one or more here-documents on the same line.
/// Returns a vector of resolved here-documents and optional trailing content.
#[allow(clippy::type_complexity)]
pub(crate) fn here_documents<'a>(
    tracker: &'a ParseContext<'a>,
) -> impl Parser<StrStream<'a>, (Vec<(Option<i32>, ast::IoHereDocument)>, Option<&'a str>), PError> + 'a
{
    move |input: &mut StrStream<'a>| {
        // Collect all here-doc markers on this line
        let mut markers: Vec<PendingHereDoc> = Vec::new();

        // Parse the first marker
        let first_marker = here_document_marker().parse_next(input)?;
        markers.push(first_marker);

        // Skip optional whitespace after delimiter
        let _: &str =
            winnow::token::take_while(0.., |c| c == ' ' || c == '\t').parse_next(input)?;

        // Check if there are more here-doc markers on this line
        while winnow::combinator::peek::<_, _, PError, _>("<<")
            .parse_next(input)
            .is_ok()
        {
            let marker = here_document_marker().parse_next(input)?;
            markers.push(marker);
            // Skip whitespace after this marker
            let _: &str =
                winnow::token::take_while(0.., |c| c == ' ' || c == '\t').parse_next(input)?;
        }

        // Capture remaining content until newline (for pipeline continuations like "| grep x")
        let rest: &str = winnow::token::take_while(0.., |c| c != '\n').parse_next(input)?;
        let remaining_line = {
            let trimmed = rest.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed)
            }
        };

        // Consume the newline
        '\n'.parse_next(input)?;

        // Now resolve content for each here-doc in order.
        // Each heredoc's content parser stops WITHOUT consuming the newline
        // after the delimiter.  Between consecutive heredocs we must skip
        // that newline so the next heredoc's content starts on a fresh line.
        let mut resolved: Vec<(Option<i32>, ast::IoHereDocument)> = Vec::new();
        for (i, marker) in markers.into_iter().enumerate() {
            if i > 0 {
                // Skip the newline left after the previous delimiter
                let _: Result<char, PError> = '\n'.parse_next(input);
            }
            let doc = resolve_here_document(input, marker, tracker)?;
            resolved.push(doc);
        }

        Ok((resolved, remaining_line))
    }
}

fn here_document<'a>(
    tracker: &'a ParseContext<'a>,
) -> impl Parser<StrStream<'a>, (Option<i32>, ast::IoHereDocument, Option<&'a str>), PError> + 'a {
    move |input: &mut StrStream<'a>| {
        // Use the multi-heredoc parser but only return the first one
        // This maintains backwards compatibility with existing code that expects a single here-doc
        let (mut docs, remaining) = here_documents(tracker).parse_next(input)?;

        if docs.is_empty() {
            return Err(winnow::error::ErrMode::Backtrack(ContextError::default()));
        }

        let (fd, doc) = docs.remove(0);
        // Note: additional docs are discarded here - callers should use here_documents() directly
        // for proper multi-heredoc support
        Ok((fd, doc, remaining))
    }
}

/// Result of parsing an I/O redirect - may include trailing content for here-docs
pub struct IoRedirectResult<'a> {
    /// The parsed redirect
    pub redirect: ast::IoRedirect,
    /// For here-docs, any content after the delimiter on the same line (e.g., "| grep x")
    pub trailing_content: Option<&'a str>,
}

/// Parse a file redirect (e.g., "> file", "2>&1", "< input")
/// Corresponds to: winnow.rs `io_file()` + `io_redirect()`
pub fn io_redirect<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a ParseContext<'a>,
) -> impl Parser<StrStream<'a>, IoRedirectResult<'a>, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        winnow::combinator::alt((
            // Try OutputAndError redirects first (&>> and &>)
            (
                "&>>",
                winnow::combinator::preceded(super::char_parsers::spaces(), word_parsers::word_as_ast(ctx, tracker)),
            )
                .map(|(_, target)| IoRedirectResult {
                    redirect: ast::IoRedirect::OutputAndError(target, true),
                    trailing_content: None,
                }),
            (
                "&>",
                winnow::combinator::preceded(super::char_parsers::spaces(), word_parsers::word_as_ast(ctx, tracker)),
            )
                .map(|(_, target)| IoRedirectResult {
                    redirect: ast::IoRedirect::OutputAndError(target, false),
                    trailing_content: None,
                }),
            // Try here-string (<<<)
            (
                winnow::combinator::opt(io_number()),
                "<<<",
                winnow::combinator::preceded(super::char_parsers::spaces(), word_parsers::word_as_ast(ctx, tracker)),
            )
                .map(|(fd, _, word)| IoRedirectResult {
                    redirect: ast::IoRedirect::HereString(fd, word),
                    trailing_content: None,
                }),
            // Try here-document
            here_document(tracker).map(|(fd, here_doc, remaining)| {
                // Store trailing content in context for later processing by pipe_sequence
                if let Some(trailing) = remaining {
                    *ctx.pending_heredoc_trailing.borrow_mut() = Some(trailing);
                }
                IoRedirectResult {
                    redirect: ast::IoRedirect::HereDocument(fd, here_doc),
                    trailing_content: remaining,
                }
            }),
            // Then try regular file redirects (including process substitution as target)
            move |input: &mut StrStream<'a>| {
                let fd = winnow::combinator::opt(io_number()).parse_next(input)?;
                let kind = redirect_operator().parse_next(input)?;
                super::char_parsers::spaces().parse_next(input)?;

                // Try process substitution as redirect target first (e.g., < <(cmd))
                let redirect_target = if let Ok((ps_kind, ps_cmd)) =
                    process_substitution(ctx, tracker).parse_next(input)
                {
                    ast::IoFileRedirectTarget::ProcessSubstitution(ps_kind, ps_cmd)
                } else {
                    let target = word_parsers::word_as_ast(ctx, tracker).parse_next(input)?;
                    match kind {
                        ast::IoFileRedirectKind::DuplicateOutput
                        | ast::IoFileRedirectKind::DuplicateInput => {
                            ast::IoFileRedirectTarget::Duplicate(target)
                        }
                        _ => ast::IoFileRedirectTarget::Filename(target),
                    }
                };

                Ok(IoRedirectResult {
                    redirect: ast::IoRedirect::File(fd, kind, redirect_target),
                    trailing_content: None,
                })
            },
        ))
        .parse_next(input)
    }
}

/// Parse a redirect list (one or more redirects)
/// Corresponds to: winnow.rs `redirect_list()`
pub fn redirect_list<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a ParseContext<'a>,
) -> impl Parser<StrStream<'a>, ast::RedirectList, PError> + 'a {
    move |input: &mut StrStream<'a>| {
        repeat::<_, _, Vec<_>, _, _>(
            1..,
            winnow::combinator::preceded(super::char_parsers::spaces(), io_redirect(ctx, tracker)).map(|r| r.redirect), // Extract just the redirect, ignore trailing content
        )
        .map(ast::RedirectList)
        .parse_next(input)
    }
}

// Reference process_substitution from the main module
fn process_substitution<'a>(
    ctx: &'a ParseContext<'a>,
    tracker: &'a ParseContext<'a>,
) -> impl Parser<StrStream<'a>, (ast::ProcessSubstitutionKind, ast::SubshellCommand), PError> + 'a {
    super::process_substitution(ctx, tracker)
}