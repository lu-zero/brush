# Bug: `declare` does not handle single-quoted array assignments

## Summary

When `declare` receives a single-quoted argument containing an array assignment
like `'arr=(${X})'`, brush rejects it with `not a valid variable name` instead
of parsing it as an array assignment with deferred expansion.

## Minimal Reproduction

```bash
X="a b"; IFS=, declare -a 'arr=(${X})'; echo "${arr[@]}"
```

- **Expected (bash):** `a b`
- **Actual (brush):** `declare: arr=(${X}): not a valid variable name`

## Explanation

In bash, single quotes around a `declare` argument like `'types=(${X})'` are
stripped by the parser. The resulting `types=(${X})` is then recognized as an
array assignment. Because the original text was single-quoted, `${X}` is **not**
expanded at parse time — it is preserved as a literal string and expanded at
runtime when `declare` evaluates the assignment. This is the standard
"single-quoted deferred expansion" pattern used with `declare`.

In other words, the following are equivalent:

```bash
declare -a 'arr=(${X})'     # single-quoted: deferred expansion
declare -a "arr=(${X})"     # double-quoted: immediate expansion
```

Brush currently treats the entire string `arr=(${X})` as a variable name,
failing to parse the `=` as the assignment separator.

## Real-World Impact

This affects the Gentoo `rpm.eclass` (lines 46 and 86):

```bash
IFS=, declare -a 'types=(${RPM_COMPRESS_TYPE})'
```

This causes 12 BDEPEND mismatches when regenerating the Gentoo metadata cache —
the `rpm` dependency with USE-conditional flags is silently dropped, producing
only `app-arch/rpm2targz` instead of the full `|| ( app-arch/rpm2targz >=app-arch/rpm-4.19.0[lzma(+)] )`.

## Affected Ebuilds (examples)

- `app-office/freeoffice-2252`
- `sys-block/perccli-7.2313.0`
- `sys-block/storcli-7.2405`
- `sys-boot/shim-15.8`
- `sci-misc/jupyterlab-desktop-bin-4.2.5.1`

## Where to Fix

The fix should be in the `declare` builtin implementation (likely in
`brush-builtins` or `brush-core`). The argument parsing for `declare` needs to:

1. Strip surrounding quotes from each argument (this may already happen at the
   parser level).
2. Detect the `=` separator to split into variable name and value.
3. If the value looks like an array `(…)` and `-a` was passed, parse it as an
   array assignment.
4. If the original argument was single-quoted, perform expansion of the value
   **at runtime** (deferred), not at parse time.

Key files to investigate:
- `brush-builtins/` — the `declare` builtin implementation
- `brush-core/` — variable/parameter expansion logic
- `brush-parser/` — argument parsing and quote handling

## Testing

The fix can be verified with:

```bash
# Basic single-quoted array assignment
X="a b"; IFS=, declare -a 'arr=(${X})'; echo "${arr[@]}"
# Expected: a b

# Array with multiple elements via IFS splitting
RPM_COMPRESS_TYPE="lzma zstd"; IFS=, declare -a 'types=(${RPM_COMPRESS_TYPE})'; echo "${#types[@]}"
# Expected: 2

# Compare with double-quoted (immediate expansion)
X="hello"; declare -a "arr2=(${X})"; echo "${arr2[0]}"
# Expected: hello
```

Also run the existing brush test suite to ensure no regressions.

## Environment

- brush version: 0.3.0 (git: 6b4d28ab)
- brush-upstream commit: af1ede8b
- Tested on: macOS (aarch64)
