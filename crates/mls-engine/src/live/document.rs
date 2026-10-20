//! The live document: the latest source and its analysis, diffed by block identity.
//!
//! An update never mutates the state unless the new source parses and carries valid identities,
//! so a refused edit leaves the previous document in force.

use crate::{review::lacks_persisted_ids, source_kind};
use mls_core::{Block, BlockId, BlockKind};
use mls_tm::{Analysis, ParseError, analyze, parse};
use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap};

/// Why a source could not become the live document. The previous document stays in force.
#[derive(Debug, thiserror::Error)]
pub enum DocumentError {
    /// The source is not valid TeXmacs.
    #[error(transparent)]
    Parse(#[from] ParseError),
    /// Identity metadata is invalid or duplicated.
    #[error("identity metadata is invalid or duplicated; fix it before live review")]
    InvalidIds,
    /// Formal blocks have no persisted identity yet.
    #[error("formal blocks lack persisted IDs; run mls --write-ids on the document first")]
    MissingIds,
}

/// Derived state of one block in the latest analysis.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockState {
    /// Source kind.
    pub kind: BlockKind,
    /// Hash of the block's canonical text.
    pub text_hash: String,
    /// Primary source label, when present.
    pub label: Option<String>,
    /// Explicitly referenced blocks, in reference order.
    pub deps: Vec<BlockId>,
    /// The nearest section heading at or before this block. A heading is its own section.
    pub section: Option<BlockId>,
    /// Index of the block in document order.
    pub position: usize,
    /// The revision at which this block's content last changed, or at which it was added.
    pub changed_at: u64,
}

/// What one accepted update changed, by block identity.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangeSet {
    /// The document revision after the update.
    pub revision: u64,
    /// Blocks new to the document, in document order.
    pub added: Vec<BlockId>,
    /// Blocks that left the document, in the order they had before the update.
    pub removed: Vec<BlockId>,
    /// Blocks whose kind, label, dependencies or canonical text changed, in document order.
    pub changed: Vec<BlockId>,
    /// Blocks with unchanged content whose position among the other surviving blocks or whose
    /// section changed. A move never dirties a block.
    pub moved: Vec<BlockId>,
    /// Blocks with unchanged content, position and section.
    pub unchanged: Vec<BlockId>,
    /// Formal blocks that transitively depended, before the update, on a changed or removed block.
    /// They may no longer depend on it, so `Document::dirty` includes them.
    pub previous_dependents: BTreeSet<BlockId>,
}

/// The latest source of a live document, its analysis, and the derived block graph.
#[derive(Clone, Debug)]
pub struct Document {
    source: Vec<u8>,
    analysis: Analysis,
    revision: u64,
    blocks: BTreeMap<BlockId, BlockState>,
    /// Index of each block in `analysis.blocks`.
    index: HashMap<BlockId, usize>,
    order: Vec<BlockId>,
    /// Formal dependents of every referenced block, in document order.
    dependents: BTreeMap<BlockId, Vec<BlockId>>,
    dependency_order: Vec<BlockId>,
    /// Formal blocks on, or transitively dependent on, a dependency cycle.
    cycle_blocked: BTreeSet<BlockId>,
    /// Rank of each formal block in `dependency_order`.
    rank: HashMap<BlockId, usize>,
}

impl Default for Document {
    fn default() -> Self {
        Self {
            source: Vec::new(),
            analysis: Analysis {
                blocks: Vec::new(),
                diagnostics: Vec::new(),
                insertions: Vec::new(),
                canonical: HashMap::new(),
            },
            revision: 0,
            blocks: BTreeMap::new(),
            index: HashMap::new(),
            order: Vec::new(),
            dependents: BTreeMap::new(),
            dependency_order: Vec::new(),
            cycle_blocked: BTreeSet::new(),
            rank: HashMap::new(),
        }
    }
}

/// Whether a source kind is formalized (a statement with a SIR and a Lean declaration).
pub(crate) fn is_formal(kind: BlockKind) -> bool {
    source_kind(kind).is_some()
}

impl Document {
    /// An empty document. The first accepted update fills it.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Re-analyze `source` and diff it against the current document by block identity.
    ///
    /// The source must parse, carry valid identities, and persist the identities of every formal
    /// block. Otherwise the document is left unchanged.
    ///
    /// # Errors
    ///
    /// Returns [`DocumentError`] when the source is refused.
    pub fn update(&mut self, source: &[u8]) -> Result<ChangeSet, DocumentError> {
        let analysis = analyze(&parse(source)?);
        if analysis
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "invalid_id" || diagnostic.code == "duplicate_id")
        {
            return Err(DocumentError::InvalidIds);
        }
        if lacks_persisted_ids(&analysis) {
            return Err(DocumentError::MissingIds);
        }
        let revision = self.revision + 1;
        let order: Vec<BlockId> = analysis.blocks.iter().map(|block| block.id).collect();
        let mut next = BTreeMap::new();
        let mut current_section = None;
        for (position, block) in analysis.blocks.iter().enumerate() {
            if block.kind == BlockKind::Section {
                current_section = Some(block.id);
            }
            next.insert(
                block.id,
                BlockState {
                    kind: block.kind,
                    text_hash: block.text_hash.clone(),
                    label: block.label.clone(),
                    deps: block.deps.clone(),
                    section: current_section,
                    position,
                    changed_at: revision,
                },
            );
        }

        let mut change = ChangeSet {
            revision,
            ..ChangeSet::default()
        };
        for id in &order {
            match self.blocks.get(id) {
                None => change.added.push(*id),
                Some(old) if content_differs(old, &next[id]) => change.changed.push(*id),
                Some(_) => {}
            }
        }
        change.removed = self
            .order
            .iter()
            .filter(|id| !next.contains_key(*id))
            .copied()
            .collect();

        // Blocks present in both documents keep their relative order unless they are moved. The
        // longest increasing run of old positions, read in the new order, is the unmoved set.
        let common: Vec<(usize, BlockId)> = order
            .iter()
            .filter_map(|id| self.blocks.get(id).map(|old| (old.position, *id)))
            .collect();
        let old_positions: Vec<usize> = common.iter().map(|(position, _)| *position).collect();
        let in_order = longest_increasing(&old_positions);
        for (slot, (_, id)) in common.iter().enumerate() {
            if change.changed.contains(id) {
                continue;
            }
            let old_section = self.blocks[id].section;
            if in_order.contains(&slot) && old_section == next[id].section {
                change.unchanged.push(*id);
            } else {
                change.moved.push(*id);
            }
        }
        for id in &order {
            if let Some(state) = next.get_mut(id)
                && let Some(old) = self.blocks.get(id)
                && !change.added.contains(id)
                && !change.changed.contains(id)
            {
                state.changed_at = old.changed_at;
            }
        }

        let seeds: BTreeSet<BlockId> = change
            .changed
            .iter()
            .chain(&change.removed)
            .copied()
            .collect();
        change.previous_dependents = self.downstream(seeds);

        let (dependents, dependency_order, cycle_blocked) = graph(&analysis, &next, &order);
        let rank = dependency_order
            .iter()
            .enumerate()
            .map(|(rank, id)| (*id, rank))
            .collect();
        let index = order
            .iter()
            .enumerate()
            .map(|(position, id)| (*id, position))
            .collect();
        self.source = source.to_vec();
        self.analysis = analysis;
        self.revision = revision;
        self.blocks = next;
        self.index = index;
        self.order = order;
        self.dependents = dependents;
        self.dependency_order = dependency_order;
        self.cycle_blocked = cycle_blocked;
        self.rank = rank;
        Ok(change)
    }

    /// Formal blocks that must be formalized again after `change`, in ID order.
    ///
    /// These are the added and changed formal blocks, every formal block downstream of any changed
    /// target (including non-formal label targets), and every formal block that depended on a
    /// changed or removed block before the update. Moves dirty nothing. Layout-only edits change
    /// no canonical text and so dirty nothing.
    #[must_use]
    pub fn dirty(&self, change: &ChangeSet) -> BTreeSet<BlockId> {
        let seeds: BTreeSet<BlockId> = change
            .added
            .iter()
            .chain(&change.changed)
            .copied()
            .collect();
        let mut dirty: BTreeSet<_> = seeds
            .iter()
            .copied()
            .filter(|id| self.is_formal_block(*id))
            .collect();
        dirty.extend(self.downstream(seeds));
        dirty.extend(
            change
                .previous_dependents
                .iter()
                .copied()
                .filter(|id| self.is_formal_block(*id)),
        );
        dirty
    }

    /// Every formal block reachable from `seeds` through at least one formal dependency edge.
    #[must_use]
    pub fn downstream(&self, seeds: impl IntoIterator<Item = BlockId>) -> BTreeSet<BlockId> {
        let mut reached = BTreeSet::new();
        let mut pending: Vec<BlockId> = seeds.into_iter().collect();
        while let Some(id) = pending.pop() {
            for dependent in self.dependents(id) {
                if reached.insert(*dependent) {
                    pending.push(*dependent);
                }
            }
        }
        reached
    }

    /// The revision of the latest accepted update. The empty document is revision 0.
    #[must_use]
    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// The latest source bytes.
    #[must_use]
    pub fn source(&self) -> &[u8] {
        &self.source
    }

    /// The analysis of the latest source.
    #[must_use]
    pub fn analysis(&self) -> &Analysis {
        &self.analysis
    }

    /// The derived state of a block, when the document has it.
    #[must_use]
    pub fn block(&self, id: BlockId) -> Option<&BlockState> {
        self.blocks.get(&id)
    }

    /// The parsed block of the latest source, with its source span.
    #[must_use]
    pub fn block_info(&self, id: BlockId) -> Option<&Block> {
        self.index
            .get(&id)
            .map(|index| &self.analysis.blocks[*index])
    }

    /// Whether the document has a block with this identity.
    #[must_use]
    pub fn contains(&self, id: BlockId) -> bool {
        self.blocks.contains_key(&id)
    }

    /// Every block in document order.
    #[must_use]
    pub fn order(&self) -> &[BlockId] {
        &self.order
    }

    /// The formal blocks in dependency order. Blocks on a dependency cycle come last, in document
    /// order.
    #[must_use]
    pub fn dependency_order(&self) -> &[BlockId] {
        &self.dependency_order
    }

    /// The position of a formal block in [`Document::dependency_order`]. Non-formal blocks have none.
    #[must_use]
    pub fn dependency_rank(&self, id: BlockId) -> Option<usize> {
        self.rank.get(&id).copied()
    }

    /// The formal dependencies of a block, in its reference order.
    #[must_use]
    pub fn formal_dependencies(&self, id: BlockId) -> Vec<BlockId> {
        self.block_info(id)
            .map(|block| {
                block
                    .deps
                    .iter()
                    .copied()
                    .filter(|dependency| self.is_formal_block(*dependency))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The formal blocks that directly depend on `id`, in document order.
    #[must_use]
    pub fn dependents(&self, id: BlockId) -> &[BlockId] {
        self.dependents.get(&id).map_or(&[][..], Vec::as_slice)
    }

    pub(crate) fn cycle_blocked(&self) -> &BTreeSet<BlockId> {
        &self.cycle_blocked
    }

    /// The canonical text of a block, when it has one.
    #[must_use]
    pub fn canonical(&self, id: BlockId) -> Option<&[u8]> {
        self.analysis.canonical_text(id)
    }

    fn is_formal_block(&self, id: BlockId) -> bool {
        self.blocks
            .get(&id)
            .is_some_and(|block| is_formal(block.kind))
    }
}

/// Whether two states of one block differ in anything a formalization depends on.
fn content_differs(old: &BlockState, new: &BlockState) -> bool {
    old.kind != new.kind
        || old.text_hash != new.text_hash
        || old.label != new.label
        || old.deps != new.deps
}

/// The formal dependents of every target, dependency order, and blocks blocked by cycles.
fn graph(
    analysis: &Analysis,
    blocks: &BTreeMap<BlockId, BlockState>,
    order: &[BlockId],
) -> (
    BTreeMap<BlockId, Vec<BlockId>>,
    Vec<BlockId>,
    BTreeSet<BlockId>,
) {
    let mut dependents: BTreeMap<BlockId, Vec<BlockId>> = BTreeMap::new();
    let formal: Vec<BlockId> = analysis
        .blocks
        .iter()
        .filter(|block| is_formal(block.kind))
        .map(|block| block.id)
        .collect();
    let mut remaining: HashMap<BlockId, usize> = HashMap::new();
    for block in analysis.blocks.iter().filter(|block| is_formal(block.kind)) {
        let mut count = 0;
        for dependency in &block.deps {
            dependents.entry(*dependency).or_default().push(block.id);
            if blocks
                .get(dependency)
                .is_some_and(|state| is_formal(state.kind))
            {
                count += 1;
            }
        }
        remaining.insert(block.id, count);
    }
    let position: HashMap<BlockId, usize> = order
        .iter()
        .enumerate()
        .map(|(index, id)| (*id, index))
        .collect();
    let mut ready: BinaryHeap<Reverse<(usize, BlockId)>> = formal
        .iter()
        .filter(|id| remaining[*id] == 0)
        .map(|id| Reverse((position[id], *id)))
        .collect();
    let mut done = BTreeSet::new();
    let mut sorted = Vec::with_capacity(formal.len());
    while let Some(Reverse((_, id))) = ready.pop() {
        done.insert(id);
        sorted.push(id);
        for dependent in dependents.get(&id).map_or(&[][..], Vec::as_slice) {
            let count = remaining
                .get_mut(dependent)
                .expect("formal dependents are counted");
            *count -= 1;
            if *count == 0 {
                ready.push(Reverse((position[dependent], *dependent)));
            }
        }
    }
    // The unresolved suffix contains cycles and every block downstream of them.
    let cycle_blocked = formal
        .iter()
        .filter(|id| !done.contains(*id))
        .copied()
        .collect();
    sorted.extend(formal.iter().filter(|id| !done.contains(*id)).copied());
    (dependents, sorted, cycle_blocked)
}

/// Indices of one longest strictly increasing subsequence of `values`, which must be distinct.
fn longest_increasing(values: &[usize]) -> BTreeSet<usize> {
    let mut tails: Vec<usize> = Vec::new();
    let mut previous: Vec<Option<usize>> = vec![None; values.len()];
    for (index, value) in values.iter().enumerate() {
        let slot = tails.partition_point(|tail| values[*tail] < *value);
        if slot > 0 {
            previous[index] = Some(tails[slot - 1]);
        }
        if slot == tails.len() {
            tails.push(index);
        } else {
            tails[slot] = index;
        }
    }
    let mut result = BTreeSet::new();
    let mut cursor = tails.last().copied();
    while let Some(index) = cursor {
        result.insert(index);
        cursor = previous[index];
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_increasing_run_is_maximal() {
        assert_eq!(
            longest_increasing(&[0, 1, 2, 3]),
            BTreeSet::from([0, 1, 2, 3])
        );
        assert_eq!(longest_increasing(&[3, 0, 1, 2]), BTreeSet::from([1, 2, 3]));
        assert_eq!(longest_increasing(&[2, 0, 1]), BTreeSet::from([1, 2]));
        assert!(longest_increasing(&[]).is_empty());
    }
}
