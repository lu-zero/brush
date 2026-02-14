//! Type aliases and basic types for the winnow string parser

use winnow::error::ContextError;
use winnow::stream::LocatingSlice;

/// Type alias for parser error
pub type PError = winnow::error::ErrMode<ContextError>;

/// Type alias for input stream
pub type StrStream<'a> = LocatingSlice<&'a str>;
