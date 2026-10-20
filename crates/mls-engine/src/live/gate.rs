//! When a dirty block may be formalized: the debounce, the block under the cursor, and an optional
//! "is this block finished?" oracle.
//!
//! The gate only ever delays work. It never makes a block eligible while the cursor is in it, and
//! a missing or unsure oracle leaves the debounce as the only rule.

use mls_core::BlockId;
use std::cell::Cell;
use std::collections::BTreeMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// A source of the current time in milliseconds. The engine takes the time as an argument, so a
/// server passes [`SystemClock`] and tests pass a [`ManualClock`].
pub trait Clock {
    /// The current time in milliseconds.
    fn now_ms(&self) -> u64;
}

/// The wall clock, in milliseconds since the Unix epoch.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |elapsed| {
                u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
            })
    }
}

/// A clock that moves only when told to.
#[derive(Debug, Default)]
pub struct ManualClock {
    now: Cell<u64>,
}

impl ManualClock {
    /// A clock showing `now` milliseconds.
    #[must_use]
    pub fn new(now: u64) -> Self {
        Self {
            now: Cell::new(now),
        }
    }

    /// Move the clock forward by `milliseconds`.
    pub fn advance(&self, milliseconds: u64) {
        self.now.set(self.now.get() + milliseconds);
    }
}

impl Clock for ManualClock {
    fn now_ms(&self) -> u64 {
        self.now.get()
    }
}

/// Whether a block is finished, according to an oracle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Finished {
    /// The block reads as complete.
    Yes,
    /// The block reads as unfinished, for example mid-sentence.
    No,
    /// The oracle could not tell.
    Unknown,
}

/// An oracle answer with its confidence in `[0, 1]`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct OracleAnswer {
    /// The answer.
    pub finished: Finished,
    /// The oracle's confidence in `finished`.
    pub confidence: f64,
}

impl OracleAnswer {
    /// The answer when no oracle is configured or it gave up.
    pub const UNKNOWN: Self = Self {
        finished: Finished::Unknown,
        confidence: 0.0,
    };
}

/// What the oracle is asked about: one block and its canonical text.
#[derive(Clone, Copy, Debug)]
pub struct FinishedQuery<'a> {
    /// The block.
    pub block: BlockId,
    /// Its canonical text, decoded lossily.
    pub text: &'a str,
}

/// An oracle that judges whether a block is finished. The decisions layer supplies one.
pub trait FinishedOracle {
    /// Judge one block.
    fn is_finished(&mut self, query: &FinishedQuery<'_>) -> OracleAnswer;
}

/// Gate settings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GateConfig {
    /// Milliseconds a changed block must be quiet before it is formalized.
    pub debounce_ms: u64,
    /// Minimum confidence for a "not finished" answer to hold a block back.
    pub confidence_threshold: f64,
    /// Longest time a "not finished" answer may hold a block back, measured from its last change.
    pub max_defer_ms: u64,
}

impl Default for GateConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 800,
            confidence_threshold: 0.8,
            max_defer_ms: 30_000,
        }
    }
}

/// Decides whether a dirty block may be scheduled now.
pub struct Gate {
    config: GateConfig,
    oracle: Option<Box<dyn FinishedOracle>>,
    /// The latest oracle answer per block, with the text hash it was asked about.
    memo: BTreeMap<BlockId, (String, OracleAnswer)>,
}

impl Gate {
    /// A gate without an oracle: the debounce alone decides.
    #[must_use]
    pub fn new(config: GateConfig) -> Self {
        Self {
            config,
            oracle: None,
            memo: BTreeMap::new(),
        }
    }

    /// Install or replace the oracle. Earlier answers are forgotten.
    pub fn set_oracle(&mut self, oracle: Box<dyn FinishedOracle>) {
        self.oracle = Some(oracle);
        self.memo.clear();
    }

    /// Whether a dirty block may be formalized at `now`.
    ///
    /// `touched_ms` is when the block last changed or was dirtied. `left` marks a block the cursor
    /// has just left, which skips the debounce. A "not finished" answer with enough confidence
    /// holds the block back until `max_defer_ms` has passed since `touched_ms`.
    pub fn admits(
        &mut self,
        block: BlockId,
        text_hash: &str,
        text: &str,
        touched_ms: u64,
        left: bool,
        now: u64,
    ) -> bool {
        let waited = now.saturating_sub(touched_ms);
        if !left && waited < self.config.debounce_ms {
            return false;
        }
        if waited >= self.config.max_defer_ms {
            return true;
        }
        !self.holds(block, text_hash, text)
    }

    /// Whether the oracle says, with enough confidence, that the block is not finished.
    fn holds(&mut self, block: BlockId, text_hash: &str, text: &str) -> bool {
        let answer = self.answer(block, text_hash, text);
        answer.finished == Finished::No && answer.confidence >= self.config.confidence_threshold
    }

    fn answer(&mut self, block: BlockId, text_hash: &str, text: &str) -> OracleAnswer {
        if let Some((hash, answer)) = self.memo.get(&block)
            && hash == text_hash
        {
            return *answer;
        }
        let answer = match self.oracle.as_mut() {
            Some(oracle) => oracle.is_finished(&FinishedQuery { block, text }),
            None => OracleAnswer::UNKNOWN,
        };
        self.memo.insert(block, (text_hash.to_owned(), answer));
        answer
    }
}
