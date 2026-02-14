//! Test file to verify submodules work correctly

// Test imports
use super::context::{ParseContext, PositionTracker};
use super::types::{PError, StrStream};

#[test]
fn test_submodules_compile() {
    // This test just verifies that the submodules can be imported
    // and the basic types are accessible
    let _: PError;
    let _: StrStream;
    let _: ParseContext;
    let _: PositionTracker;
}