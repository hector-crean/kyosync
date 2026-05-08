//! Naive prefix-suffix diff helper for `#[crdt(sequence)]` codegen.
//!
//! Computes the [`SequenceMut`] mutations required to bring a doc-side
//! `Sequence<T>` into agreement with a component-side iterable.
//!
//! ## Algorithm
//!
//! 1. Walk both sides forward to find the common prefix length `P`.
//! 2. Walk both sides backward (skipping the prefix on each side) to
//!    find the common suffix length `S`.
//! 3. The "middle" — the slice that differs — runs from `P` to `len - S`
//!    on both sides. Emit `DeleteAt(P, doc_middle_len)` then a flat run
//!    of `InsertAt(P + i, value)` for each element in the component
//!    middle.
//!
//! Positions in the emitted mutations are relative to the visible
//! sequence at the time each mutation is applied. `mutate()` runs
//! mutations sequentially, so the inserts after the delete see the
//! shrunken intermediate state — which is what their `pos = P + i`
//! values assume.
//!
//! ## Tradeoffs
//!
//! This is RGA-friendly but doesn't satisfy *maximal non-interleaving*
//! (Fugue does — see plan doc Part I §2.1, Part V §V.5). For the
//! collaborative-text path that needs Fugue, hand-roll a `SchemaField`
//! impl over a Fugue type via `#[crdt(with = "...")]`. For ordered-list
//! fields (`Vec<NodeId>`, etc.) where concurrent insert at the same
//! position is rare or cosmetic-only, the prefix-suffix diff over RGA
//! is fine.
//!
//! Complexity: O(n + m) where n = doc len, m = component len. No LCS;
//! a strictly-shorter prefix-suffix diff might emit more mutations than
//! the optimal edit script, but each mutation is O(1) on the wire, so
//! the cost is proportional to actual edit size in practice.

use kyoso_crdt::types::SequenceMut;

/// Compute mutations to transform `doc` into `component`. Both
/// arguments are consumed into owned `Vec<T>` for indexed access.
///
/// Used by `#[crdt(sequence)]` codegen; consumers shouldn't need to
/// call this directly unless they're writing a `SchemaField` impl over
/// a `Sequence<T>`.
#[must_use]
pub fn sequence_diff<T, D, C>(doc: D, component: C) -> Vec<SequenceMut<T>>
where
    T: Clone + PartialEq,
    D: IntoIterator<Item = T>,
    C: IntoIterator<Item = T>,
{
    let doc: Vec<T> = doc.into_iter().collect();
    let component: Vec<T> = component.into_iter().collect();

    // Common prefix.
    let mut prefix = 0;
    while prefix < doc.len()
        && prefix < component.len()
        && doc[prefix] == component[prefix]
    {
        prefix += 1;
    }

    // Common suffix, scoped so prefix and suffix don't overlap.
    let doc_remaining = doc.len().saturating_sub(prefix);
    let component_remaining = component.len().saturating_sub(prefix);
    let mut suffix = 0;
    while suffix < doc_remaining
        && suffix < component_remaining
        && doc[doc.len() - 1 - suffix] == component[component.len() - 1 - suffix]
    {
        suffix += 1;
    }

    let doc_middle_len = doc.len() - prefix - suffix;
    let component_middle = &component[prefix..component.len() - suffix];

    let mut out: Vec<SequenceMut<T>> = Vec::new();
    if doc_middle_len > 0 {
        out.push(SequenceMut::DeleteAt {
            pos: prefix,
            len: doc_middle_len,
        });
    }
    for (i, value) in component_middle.iter().enumerate() {
        out.push(SequenceMut::InsertAt {
            pos: prefix + i,
            value: value.clone(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn equal_sequences_emit_nothing() {
        let muts = sequence_diff::<u32, _, _>(vec![1, 2, 3], vec![1, 2, 3]);
        assert!(muts.is_empty());
    }

    #[test]
    fn empty_to_nonempty_emits_inserts_only() {
        let muts = sequence_diff::<u32, _, _>(Vec::<u32>::new(), vec![1, 2, 3]);
        assert_eq!(muts.len(), 3);
        assert!(matches!(
            muts[0],
            SequenceMut::InsertAt { pos: 0, value: 1 },
        ));
        assert!(matches!(
            muts[1],
            SequenceMut::InsertAt { pos: 1, value: 2 },
        ));
        assert!(matches!(
            muts[2],
            SequenceMut::InsertAt { pos: 2, value: 3 },
        ));
    }

    #[test]
    fn nonempty_to_empty_emits_single_delete() {
        let muts = sequence_diff::<u32, _, _>(vec![1, 2, 3], Vec::<u32>::new());
        assert_eq!(muts.len(), 1);
        assert!(matches!(
            muts[0],
            SequenceMut::DeleteAt { pos: 0, len: 3 },
        ));
    }

    #[test]
    fn middle_replacement_combines_delete_then_inserts() {
        // doc [A, B, C, D, E] → component [A, X, Y, E]
        // common prefix: A (len 1)
        // common suffix: E (len 1)
        // doc middle: B, C, D (len 3)
        // component middle: X, Y (len 2)
        let muts = sequence_diff::<char, _, _>(
            vec!['A', 'B', 'C', 'D', 'E'],
            vec!['A', 'X', 'Y', 'E'],
        );
        assert_eq!(muts.len(), 3);
        assert!(matches!(
            muts[0],
            SequenceMut::DeleteAt { pos: 1, len: 3 },
        ));
        assert!(matches!(
            muts[1],
            SequenceMut::InsertAt { pos: 1, value: 'X' },
        ));
        assert!(matches!(
            muts[2],
            SequenceMut::InsertAt { pos: 2, value: 'Y' },
        ));
    }

    #[test]
    fn pure_appending_uses_no_delete() {
        let muts = sequence_diff::<u32, _, _>(vec![1, 2], vec![1, 2, 3, 4]);
        assert_eq!(muts.len(), 2);
        assert!(matches!(
            muts[0],
            SequenceMut::InsertAt { pos: 2, value: 3 },
        ));
        assert!(matches!(
            muts[1],
            SequenceMut::InsertAt { pos: 3, value: 4 },
        ));
    }

    #[test]
    fn pure_truncation_uses_single_delete() {
        let muts = sequence_diff::<u32, _, _>(vec![1, 2, 3, 4], vec![1, 2]);
        assert_eq!(muts.len(), 1);
        assert!(matches!(
            muts[0],
            SequenceMut::DeleteAt { pos: 2, len: 2 },
        ));
    }
}
