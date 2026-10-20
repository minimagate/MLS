//! Content-addressed caches for live formalization.
//!
//! Both caches are plain serializable data with deterministic iteration and a size bound. An
//! entry is reusable only under its key, and caching never confers confirmation: a cached SIR is
//! still a draft, and a cached Lean declaration is still re-checked by Lean before it is accepted.

use crate::SIR_SCHEMA_VERSION;
use mls_core::BlockId;
use mls_sir::Sir;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The key of a SIR cache entry: the generation model, the SIR schema, the block's canonical text
/// hash, and the SIR hashes of its in-effect formal dependencies in reference order.
#[must_use]
pub fn sir_cache_key(model: &str, text_hash: &str, dependencies: &[(BlockId, String)]) -> String {
    let mut lines = vec![
        format!("model {model}"),
        format!("schema {SIR_SCHEMA_VERSION}"),
        format!("text {text_hash}"),
    ];
    lines.extend(
        dependencies
            .iter()
            .map(|(dependency, hash)| format!("dependency {dependency} {hash}")),
    );
    mls_core::source_hash(lines.join("\n").as_bytes())
}

/// Where a cached SIR came from.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SirProvenance {
    /// Generation model ID that produced the SIR.
    pub model: String,
    /// SIR schema version that the SIR was validated under.
    pub schema_version: String,
    /// Canonical text hash of the block when the SIR was generated.
    pub text_hash: String,
    /// In-effect dependency SIR hashes when the SIR was generated.
    pub dependencies: Vec<(BlockId, String)>,
    /// Document revision at which the entry was recorded.
    pub revision: u64,
}

/// A generated SIR and its provenance.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SirCacheEntry {
    /// The cached SIR.
    pub sir: Sir,
    /// Where the SIR came from.
    pub provenance: SirProvenance,
}

/// A bounded cache of generated SIRs keyed by [`sir_cache_key`].
///
/// When the cache is full, recording a new key evicts the entry with the lowest insertion
/// revision, ties broken by key. Lookups do not change the order, so eviction is deterministic.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SirCache {
    capacity: usize,
    entries: BTreeMap<String, SirCacheEntry>,
}

impl SirCache {
    /// An empty cache holding at most `capacity` entries (at least one).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: BTreeMap::new(),
        }
    }

    /// The entry recorded under `key`, if any.
    #[must_use]
    pub fn lookup(&self, key: &str) -> Option<&SirCacheEntry> {
        self.entries.get(key)
    }

    /// Record a generated SIR under `key`, evicting the oldest entry when the cache is full.
    pub fn record(&mut self, key: String, sir: Sir, provenance: SirProvenance) {
        if !self.entries.contains_key(&key) {
            make_room(&mut self.entries, self.capacity, |entry| {
                entry.provenance.revision
            });
        }
        self.entries.insert(key, SirCacheEntry { sir, provenance });
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The maximum number of entries.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

/// A Lean declaration accepted for one block under one section 4 cache key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeanCacheEntry {
    /// The block whose declaration this is. Its name is derived from the block identity.
    pub block: BlockId,
    /// The section 4 cache key the declaration was accepted under.
    pub cache_key: String,
    /// The accepted Lean declaration source.
    pub declaration: String,
    /// Document revision at which the entry was recorded.
    pub revision: u64,
}

/// A bounded cache of accepted Lean declarations keyed by the section 4 cache key.
///
/// The key is scoped to the block, because a declaration embeds its block's name. Eviction works
/// as in [`SirCache`].
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LeanCache {
    capacity: usize,
    entries: BTreeMap<String, LeanCacheEntry>,
}

impl LeanCache {
    /// An empty cache holding at most `capacity` entries (at least one).
    #[must_use]
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            entries: BTreeMap::new(),
        }
    }

    /// The declaration accepted for `block` under `cache_key`, if any. The caller re-checks it.
    #[must_use]
    pub fn lookup(&self, block: BlockId, cache_key: &str) -> Option<&str> {
        self.entries
            .get(&lean_slot(block, cache_key))
            .map(|entry| entry.declaration.as_str())
    }

    /// Record an accepted declaration, evicting the oldest entry when the cache is full.
    pub fn record(
        &mut self,
        block: BlockId,
        cache_key: String,
        declaration: String,
        revision: u64,
    ) {
        let slot = lean_slot(block, &cache_key);
        if !self.entries.contains_key(&slot) {
            make_room(&mut self.entries, self.capacity, |entry| entry.revision);
        }
        self.entries.insert(
            slot,
            LeanCacheEntry {
                block,
                cache_key,
                declaration,
                revision,
            },
        );
    }

    /// The number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache holds no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The maximum number of entries.
    #[must_use]
    pub fn capacity(&self) -> usize {
        self.capacity
    }
}

fn lean_slot(block: BlockId, cache_key: &str) -> String {
    format!("{block} {cache_key}")
}

/// Remove the entries with the lowest revision, ties broken by key, until one more fits.
fn make_room<T>(entries: &mut BTreeMap<String, T>, capacity: usize, revision: impl Fn(&T) -> u64) {
    while entries.len() >= capacity {
        let oldest = entries
            .iter()
            .map(|(key, entry)| (revision(entry), key))
            .min()
            .map(|(_, key)| key.clone());
        match oldest {
            Some(key) => {
                entries.remove(&key);
            }
            None => break,
        }
    }
}
