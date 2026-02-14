# Winnow String Parser Refactoring Checklist

## Overview
Refactor `winnow_str.rs` to improve organization, maintainability, and performance by splitting it into logical modules.

## Key Principles
- Rust modules can be either `foo/mod.rs` OR `foo.rs` but NOT both
- Since `winnow_str.rs` already exists, we must keep it as the main file
- Each module should have a clear, focused responsibility
- Maintain backward compatibility with existing API
- Run `cargo fmt` before each commit
- Commit at each logical step

## Refactoring Plan

### Step 1: Create Module Structure
- [ ] Create `winnow_str/` directory for submodules
- [ ] Keep `winnow_str.rs` as the main entry point
- [ ] Update `winnow_str.rs` to declare and re-export submodules
- [ ] Move `parse_program` function to stay in `winnow_str.rs`

### Step 2: Split Core Components
- [ ] Create `winnow_str/context.rs` for `ParseContext` and `PositionTracker`
- [ ] Create `winnow_str/types.rs` for type aliases and basic types
- [ ] Create `winnow_str/char_parsers.rs` for character-level parsers (Tier 0)
- [ ] Create `winnow_str/line_parsers.rs` for line breaks and separators (Tier 1)

### Step 3: Split Word Parsing
- [ ] Create `winnow_str/word_parsers.rs` for word-related functions (Tier 2)
- [ ] Create `winnow_str/expansion_parsers.rs` for variable expansions (Tier 9)
- [ ] Create `winnow_str/quote_parsers.rs` for quoted string parsing (Tier 7)

### Step 4: Split Command Parsing
- [ ] Create `winnow_str/command_parsers.rs` for command parsing (Tier 3)
- [x] Create `winnow_str/redirect_parsers.rs` for I/O redirections (Tier 8)
- [ ] Create `winnow_str/pipeline_parsers.rs` for pipelines (Tier 4)
- [ ] Create `winnow_str/and_or_parsers.rs` for and/or lists (Tier 5)

### Step 5: Split Compound Commands
- [ ] Create `winnow_str/compound_parsers.rs` for subshells and groups (Tier 10)
- [ ] Create `winnow_str/control_flow_parsers.rs` for if/while/until/for/case (Tier 11-13)
- [ ] Create `winnow_str/arithmetic_parsers.rs` for arithmetic expressions (Tier 15-16)
- [ ] Create `winnow_str/extended_test_parsers.rs` for [[ ]] expressions (Tier 17)
- [ ] Create `winnow_str/function_parsers.rs` for function definitions (Tier 14)

### Step 6: Split Complete Programs
- [ ] Create `winnow_str/program_parsers.rs` for complete commands and programs (Tier 6)

### Step 7: Update Main Module
- [ ] Update `winnow_str.rs` to declare submodules with `mod` declarations
- [ ] Update `winnow_str.rs` to re-export all public items
- [ ] Ensure all dependencies between modules are properly declared
- [ ] Verify all public APIs are accessible

### Step 8: Testing and Validation
- [ ] Run existing tests to ensure no regressions
- [ ] Fix any import issues in test files
- [ ] Update any documentation that references the old structure
- [ ] Run `cargo fmt` on all new files
- [ ] Run `cargo clippy` to catch any issues

## Implementation Notes

### Module Organization
```
winnow_str.rs          # Main entry point (kept as-is)
winnow_str/
├── context.rs         # ParseContext, PositionTracker
├── types.rs           # Type aliases, basic types
├── char_parsers.rs    # Character-level parsers
├── line_parsers.rs    # Line breaks and separators
├── word_parsers.rs    # Word parsing
├── expansion_parsers.rs # Variable expansions
├── quote_parsers.rs  # Quoted strings
├── command_parsers.rs # Command parsing
├── redirect_parsers.rs # I/O redirections
├── pipeline_parsers.rs # Pipelines
├── and_or_parsers.rs # And/Or lists
├── compound_parsers.rs # Compound commands
├── control_flow_parsers.rs # Control flow
├── arithmetic_parsers.rs # Arithmetic
├── extended_test_parsers.rs # Extended test
├── function_parsers.rs # Functions
└── program_parsers.rs # Complete programs
```

### Dependency Management
- Keep module dependencies acyclic
- Use `super::` or `crate::` paths for imports between modules
- Group related functions together in each module
- Maintain consistent naming conventions
- All submodules must be declared in `winnow_str.rs` with `mod module_name;`

### Commit Strategy
1. Create initial module structure
2. Move core types and context
3. Move character and line parsers
4. Move word and expansion parsers
5. Move command and redirection parsers
6. Move pipeline and and/or parsers
7. Move compound command parsers
8. Move control flow parsers
9. Move arithmetic and extended test parsers
10. Move function and program parsers
11. Final cleanup and testing

Each commit should be followed by `cargo fmt` and basic testing.

## Current Refactoring Status

### ✅ Completed (13/18 tasks)
- [x] Create `winnow_str/` directory for submodules
- [x] Add module declarations to `winnow_str.rs`
- [x] Create context.rs and types.rs submodules
- [x] Move core types to submodules with re-exports
- [x] Move character parsers to char_parsers.rs (10+ functions)
- [x] Move line parsers to line_parsers.rs (8+ functions)
- [x] Move variable expansions to word_parsers.rs (6 functions)
- [x] Move bare_word to word_parsers.rs
- [x] Move quoted strings and escape to word_parsers.rs (3 functions)
- [x] Move is_reserved_word and word_part to word_parsers.rs (2 functions)
- [x] Move remaining complex word parsers (word_as_ast, wordlist, non_reserved_word)
- [x] Create redirection_parsers.rs module for I/O redirections (Tier 8)

### ⏳ Not Started
- [ ] Move command and redirection parsers
- [ ] Move pipeline and and/or parsers
- [ ] Move compound command parsers
- [ ] Move control flow parsers
- [ ] Move arithmetic and extended test parsers
- [ ] Move function and program parsers
- [ ] Final validation and cleanup

### 📊 Progress Metrics
- **Functions moved**: ~40/50+ functions (80%)
- **Modules created**: 6/12 planned modules (50%)
- **Test status**: ⚠️ Partial compilation (redirection parsers working, command parsers pending)
- **Lines reduced**: `winnow_str.rs` significantly reduced from 2000+ lines

### 🎯 Next Steps
1. ✅ Completed complex word parsers refactoring
2. ✅ Created redirection_parsers.rs module (Tier 8)
3. ⏳ Complete command parsers refactoring (Tier 3)
4. Continue with pipeline and and/or parsers (Tier 4, 5)
5. Finalize documentation and cleanup
