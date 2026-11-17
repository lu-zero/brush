//! Writing AST nodes back out as shell source text, with control over indentation.

use std::fmt::{Display, Write};

use super::IoHereDocument;

/// The number of spaces per indentation level when none is requested.
pub(super) const DEFAULT_INDENT_WIDTH: usize = 4;

/// Writes AST nodes back out as shell source text, carrying the state of one write in
/// progress: how far new lines are indented, and the here-documents whose bodies are still
/// to be written.
///
/// Text written through [`std::fmt::Write`] goes out unchanged. Only [`Self::newline`]
/// starts a new line, indents it, and writes the pending here-documents before it. A line
/// break inside a word (such as in a multi-line quoted string) is thus never followed by
/// indentation, and a here-document's body and closing delimiter start at the beginning of
/// the line after the command that redirects from it, where the shell reads them back.
#[allow(unnameable_types, reason = "only named in a sealed trait")]
pub struct SourceWriter<'w, 'n> {
    out: &'w mut dyn Write,
    indent: usize,
    indent_width: usize,
    pending_here_docs: Vec<&'n IoHereDocument>,
}

impl<'w, 'n> SourceWriter<'w, 'n> {
    /// Writes `node` as source text to `out`, along with any here-document bodies it still
    /// owes at the end. New lines start `indent_level` levels deep, of `indent_width` spaces
    /// each.
    pub(super) fn write_node(
        out: &'w mut dyn Write,
        node: &'n impl WriteSource,
        indent_level: usize,
        indent_width: usize,
    ) -> std::fmt::Result {
        let mut w = Self {
            out,
            indent: indent_level,
            indent_width,
            pending_here_docs: vec![],
        };
        node.write_source(&mut w)?;
        w.write_pending_here_docs()
    }

    /// Ends the current line and starts the next one at the current indentation, writing
    /// any pending here-document bodies in between.
    pub(super) fn newline(&mut self) -> std::fmt::Result {
        self.write_pending_here_docs()?;
        self.out.write_char('\n')?;
        for _ in 0..self.indent * self.indent_width {
            self.out.write_char(' ')?;
        }
        Ok(())
    }

    /// Whether here-documents are waiting for the current line to end.
    pub(super) const fn has_pending_here_docs(&self) -> bool {
        !self.pending_here_docs.is_empty()
    }

    /// Queues a here-document to be written after the current line.
    pub(super) fn defer_here_doc(&mut self, here_doc: &'n IoHereDocument) {
        self.pending_here_docs.push(here_doc);
    }

    /// Writes the bodies and closing delimiters of the pending here-documents, each on the
    /// lines that follow the current one, in the order they were redirected from.
    pub(super) fn write_pending_here_docs(&mut self) -> std::fmt::Result {
        if self.pending_here_docs.is_empty() {
            return Ok(());
        }

        self.out.write_char('\n')?;
        for here_doc in std::mem::take(&mut self.pending_here_docs) {
            self.out.write_str(&here_doc.doc.value)?;
            self.out.write_str(&here_doc.closing_delimiter())?;
            self.out.write_char('\n')?;
        }
        Ok(())
    }

    /// Calls `f` with new lines indented one more level than they are now.
    pub(super) fn indented(
        &mut self,
        f: impl FnOnce(&mut Self) -> std::fmt::Result,
    ) -> std::fmt::Result {
        self.indent += 1;
        let result = f(self);
        self.indent -= 1;
        result
    }
}

impl Write for SourceWriter<'_, '_> {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.out.write_str(s)
    }
}

mod sealed {
    use super::SourceWriter;

    /// Implemented by AST nodes that write themselves as source text through a
    /// [`SourceWriter`], so that the nodes inside them share its state.
    #[allow(unnameable_types, reason = "sealing `Formattable`")]
    pub trait WriteSource {
        fn write_source<'n>(&'n self, w: &mut SourceWriter<'_, 'n>) -> std::fmt::Result;
    }
}

pub(super) use sealed::WriteSource;

/// Implemented by the AST nodes that can be written back out as shell source text.
///
/// `Display` writes a node with no indentation at the start and four spaces per level of
/// nesting. [`Self::formatted`] adjusts that:
///
/// ```
/// use brush_parser::ast::Formattable;
///
/// # fn example(node: &brush_parser::ast::CompoundList) {
/// let text = node.formatted().indent_level(1).indent_width(2).to_string();
/// # }
/// ```
pub trait Formattable: sealed::WriteSource + Sized {
    /// Starts formatting this node as source text. The result implements [`Display`], so
    /// `to_string()` can follow directly. Options not set keep their defaults: indentation
    /// level 0, with 4 spaces per level.
    fn formatted(&self) -> FormattedBuilder<'_, Self> {
        Formatted::builder(self)
    }
}

impl<T: sealed::WriteSource> Formattable for T {}

/// Namespace for the builder that [`Formattable::formatted`] returns.
pub struct Formatted<'a, T>(std::marker::PhantomData<&'a T>);

#[bon::bon]
impl<'a, T: Formattable> Formatted<'a, T> {
    /// Writes the node as source text with the options given.
    #[builder(
        start_fn = builder,
        finish_fn = render,
        builder_type(name = FormattedBuilder, vis = "pub"),
        derive(Clone(bounds(T: Formattable)))
    )]
    fn render_to_string(
        #[builder(start_fn)] node: &'a T,
        /// How many levels deep new lines start.
        #[builder(default = 0)]
        indent_level: usize,
        /// How many spaces make up one level.
        #[builder(default = DEFAULT_INDENT_WIDTH)]
        indent_width: usize,
    ) -> String {
        let mut text = String::new();
        // Writing to a `String` does not fail.
        let _ = SourceWriter::write_node(&mut text, node, indent_level, indent_width);
        text
    }
}

impl<T: Formattable, S: formatted_builder::IsComplete> Display for FormattedBuilder<'_, T, S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.clone().render())
    }
}

/// Implements [`Display`] for nodes that implement [`WriteSource`], with the default
/// formatting.
macro_rules! display_via_source_writer {
    ($($node:ty),+ $(,)?) => {
        $(
            impl Display for $node {
                fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                    SourceWriter::write_node(f, self, 0, DEFAULT_INDENT_WIDTH)
                }
            }
        )+
    };
}

pub(super) use display_via_source_writer;
