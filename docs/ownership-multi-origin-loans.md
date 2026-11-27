# Multi-origin loans at control-flow joins

A reference local can point to different places depending on which branch was
executed. Keeping only one origin at a join is unsound: later mutation checks
could forget a live borrow from the other predecessor.

Nova now represents a borrow as a set of `LoanOrigin` values. Each origin
contains:

- the root local;
- whether that origin is exclusive;
- the direct struct-field projection, or `None` for a root-wide/unknown place.

When both predecessors contain a loan for the same borrower, the join takes
the deduplicated union. When only one predecessor contains the borrower loan,
the join is rejected because the current HIR state does not yet encode
conditional initialization of reference locals.

Conflict detection compares origin pairs. Two origins conflict only when
their root and projections overlap and at least one member of that overlapping
pair is exclusive. This avoids accidentally applying exclusivity from an
unrelated possible referent to every origin in the set.

This is a points-to-set improvement, not a complete borrow checker. The
analysis remains based on the existing HIR visitor and block-local liveness;
a dedicated CFG, region inference, and explicit lifetime constraints are
still required for Rust-like NLL and reborrowing.
