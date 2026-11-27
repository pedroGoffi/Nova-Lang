# Ownership / NLL stage

This stage improves the block-local liveness used by the ownership checker.
It is intentionally not described as a complete Rust-equivalent borrow
checker: the compiler still does not build a full region graph or solve
borrow constraints over a first-class control-flow graph.

## Changes

- Backward liveness now kills the previous value of a local on `let` and
  assignment definitions. Reassigning a reference therefore ends the old
  loan instead of keeping it alive because the local is used later.
- `if` branches are joined by union for liveness.
- `while` and `loop` use monotone fixed-point iteration for live locals.
- `for` loop index locals are treated as definitions local to the loop.
- `defer` captures remain live until the end of their declaring block.
- Branch joins for loans no longer overwrite one branch with the other. If
  the same reference local may point to different owners on different paths,
  the checker rejects the join because the current `Loan` representation
  cannot safely express that union.
- Added integration regressions for reference reassignment and conditional
  reference joins.

## Remaining limitations

- No first-class CFG or region variables yet.
- No lifetime parameters or outlives constraints.
- Reborrowing and two-phase borrows are incomplete.
- Loans are tracked by root local and at most one field projection, not an
  arbitrary nested place path.
- Loop control-flow edges are conservatively approximated; this can reject
  valid programs.
- Conditional reference joins to distinct owners are rejected rather than
  represented as a set of possible origins.
