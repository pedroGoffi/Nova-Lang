# Nova ownership and RAII

## Current ownership model

Nova checks ownership on typed HIR and lowers cleanup into MIR. Owning locals
have root drop flags. Fixed-array elements and eligible direct struct fields
have independent flags, so a moved element is not destroyed again while its
siblings remain live.

## Partial struct-field moves

A direct field of a local struct can be moved when all of the following hold:

- the field is an owning struct value;
- the parent has no user-defined `drop(self)` implementation;
- the parent has no owning array fields;
- the field is selected directly from a local (nested projections are not yet
  supported);
- no active borrow or deferred cleanup protects the parent.

After the move, the parent remains usable through its other fields. Reading or
borrowing the moved field is rejected. Borrowing the whole parent, or moving the
whole parent, is rejected while any field remains moved. Moving a sibling is
still allowed. Reinitializing a moved field restores its field flag.

The MIR snapshots the field value and clears its field flag. Scope cleanup
checks each field flag and invokes only the destructors for still-live fields,
in reverse declaration order. The root flag still protects the aggregate as a
whole. User-defined destructors retain the Rust-like restriction: partial moves
out of a type with a custom destructor are rejected, because the custom body
must observe a complete `self` value.

## Deliberate limitations

This is not yet full Rust ownership semantics. Partial moves from nested field
projections, owning arrays inside partially movable structs, enum payload
partial moves, precise non-lexical lifetimes, explicit lifetime parameters,
and generic `Copy`/`Clone`/`Drop` trait resolution remain unsupported. These
cases must be rejected or handled by the existing whole-value path; they must
not silently bypass cleanup.

## C interoperability

`#[repr(C)]` structs can be passed to C through pointers when their fields have
C-compatible layouts. Passing structs by value still requires target-specific
ABI classification and is not implied by `repr(C)` alone.

## Non-lexical loan release (initial stage)

The ownership pass now computes block-local liveness for reference locals. A
loan is released at a statement boundary after the borrower's final use in
that block, rather than being held until the end of the lexical scope. Uses in
nested statements and deferred cleanup expressions are included by the local
collector. A reference declared by an enclosing block is not prematurely
released while visiting a nested block; its declaring block decides when it
becomes dead.

This is an initial NLL improvement, not the complete Rust borrow checker. The
analysis is statement-boundary based and does not yet model precise expression
regions, reborrows, closure captures, async suspension points, or a complete
control-flow graph region solver. In ambiguous control-flow joins, loans are
kept conservatively.
