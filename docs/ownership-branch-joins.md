# Ownership branch joins — 0.20.4

At a control-flow join, the ownership checker must merge both value availability
and the provenance of outstanding loans. A borrower local currently stores one
`Loan { source, field, exclusive }`, not a set of possible origins.

The previous implementation extended the two predecessor `HashMap`s. When both
branches assigned the same borrower from different owners, one entry replaced
the other. The resulting state depended on map insertion order and could forget
an outstanding loan.

The checker now requires the loan entry for each borrower to be identical on
both incoming paths. It rejects:

- a reference borrowing `left` in one branch and `right` in the other;
- a reference borrowed on one path but not the other;
- any other difference in source, field projection, or mutability.

This is intentionally conservative. The next architectural step is to represent
loan provenance as a set of origins and propagate it over a real CFG, rather
than weakening the check. This change does not claim complete NLL or region
inference.

Unit regressions cover identical origins, differing origins, and a loan present
on only one predecessor.
