# Projection-aware loans

Nova tracks statically-known struct field paths for borrows. A path is represented
as field indices from the root local. Two paths overlap when their common prefix
is equal; this captures parent/child overlap while allowing sibling fields to be
borrowed independently.

Examples:

- `&mut value.left` conflicts with `&value.left.child`.
- `&mut value.left.child` conflicts with `&value.left.other` only when their
  projections share the same prefix through the selected place; distinct final
  fields are disjoint.
- `&mut value.left` and `&value.right` are disjoint.

A dereference or index projection currently collapses to a whole-root loan.
This is intentional: raw pointers may alias, and dynamic indices are not proven
disjoint by this analysis. A future MIR/CFG place analysis can refine those cases.

This change does not relax control-flow join requirements. A reference local
whose referent differs between branches is still rejected until the compiler
has a points-to set / region representation for multiple possible origins.

## Projection-aware writes and moves (0.20.6)

Direct writes to a struct field and supported direct partial moves now check
loan overlap against that field projection instead of rejecting every loan on
the root object. Thus a loan of `value.left` does not prevent mutation of
`value.right`, while a loan of `value` or `value.left.child` still conflicts
with mutation of `value.left`. Deferred cleanup protection remains root-wide
because defer captures are not yet represented as place projections.
