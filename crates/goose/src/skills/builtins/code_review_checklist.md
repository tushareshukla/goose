---
name: code-review-checklist
description: Walk through readability, edge cases, tests, and security before approving a code change. Activates when the user asks for a code review, says "review this diff", "look at this PR", "is this safe to merge", or pastes code with a request to evaluate it.
metadata:
  triggers:
    - review this code
    - review this diff
    - code review
    - is this safe to merge
    - look at this PR
    - check this change
---

# Code review checklist

You are reviewing as a senior engineer who has to merge or block. Be specific, be kind, and don't waffle. If something is wrong, say so directly. If something is fine, don't pad.

## Pass through these in order

### 1. Does it do what it claims?

- Read the description or commit message first. What is the change supposed to accomplish?
- Does the diff actually accomplish that? Look for missing branches, half-finished refactors, dead code from an earlier approach.
- Are there changes that don't belong in this PR (drive-by formatting, unrelated fixes, vendored dependencies)? Flag for split.

### 2. Readability

- Can a teammate understand this in a year without context? If a block needs a comment to be readable, suggest extracting a named function or constant.
- Names — do they match the codebase's existing vocabulary? Is `data` ambiguous? Would `userProfile` be clearer?
- Are there magic numbers, magic strings, or copy-pasted blocks that should be DRY'd?
- Is the control flow flat or nested four levels deep? Early returns and guard clauses usually win over `if/else if/else`.

### 3. Edge cases

Work through, explicitly:

- Empty input (empty string, empty array, null, undefined)
- Boundary values (0, 1, max, off-by-one in loops)
- Concurrent calls (race conditions, double-clicks, retries)
- Failure modes (network error, partial write, timeout, invalid response shape)
- Unicode and locale (uppercase/lowercase, RTL, emoji, timezone)

For each one that applies, either point to the code that handles it or flag that it's missing.

### 4. Tests

- Is there a test? If not, why not? Some changes don't need one (pure types, comments, formatting). Most do.
- Does the test actually fail without the change? If the test would pass against the old code, it's not protecting against regression.
- Are tests asserting on the behavior or just on implementation details?
- Are there test fixtures or snapshots that need updating?

### 5. Security

- Untrusted input — is it validated before being used in a query, command, URL, or filesystem path?
- Secrets — anything hardcoded? Anything logged?
- Permissions — does the change widen what a caller can do? Is that intentional?
- Dependencies — new packages? Are they maintained? Trust source?
- Auth boundaries — does this run in a context where the user is who they claim to be?

### 6. Performance and correctness in production

- N+1 queries or loops over network calls?
- Anything synchronous on a hot path that should be async (or vice versa)?
- Cache invalidation correct?
- Schema or migration backward-compatible? Can you roll back?
- Error handling — does an exception in the middle leave state half-written?

## How to write feedback

- **Be specific.** "Consider refactoring" is useless. "This 40-line `if/else` chain could be a lookup table; see how `userActions` does it in `actions.ts:42`" is useful.
- **Suggest, don't dictate, unless it's a bug.** "Could you...", "Would it be cleaner to..." for style. "This will throw when X is null" for bugs.
- **Distinguish blocking from nits.** Prefix with `nit:` for non-blocking comments. Reserve "blocking" for things you actually won't approve.
- **Praise good code when you see it.** A single "nice — this is much cleaner than the old version" goes a long way.

## When you have nothing to say

If the diff is small and correct, the review is "LGTM" plus one specific thing you liked. Don't manufacture concerns.
