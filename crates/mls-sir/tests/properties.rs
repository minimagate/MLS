//! Property tests for canonical JSON-independent SIR semantics.

use mls_sir::Sir;
use proptest::prelude::*;
use serde_json::Value;

const FIXTURE: &str = include_str!("fixtures/continuous_bounded.json");

proptest! {
    #[test]
    fn object_key_order_does_not_change_hash(reverse in any::<bool>()) {
        let original = Sir::from_json(FIXTURE).unwrap();
        let Value::Object(map) = serde_json::from_str::<Value>(FIXTURE).unwrap() else { unreachable!() };
        let mut entries: Vec<_> = map.into_iter().collect();
        if reverse { entries.reverse(); } else { entries.rotate_left(3); }
        let fields = entries.into_iter().map(|(key, value)| {
            format!("{}:{}", serde_json::to_string(&key).unwrap(), serde_json::to_string(&value).unwrap())
        }).collect::<Vec<_>>().join(",");
        let json = format!("{{{fields}}}");
        let decoded = Sir::from_json(&json).unwrap();
        prop_assert_eq!(original.semantic_hash(), decoded.semantic_hash());
    }

    #[test]
    fn surrounding_whitespace_is_cosmetic(left in " {0,8}", right in " {0,8}") {
        let original = Sir::from_json(FIXTURE).unwrap();
        let changed = FIXTURE.replace("a ≤ b", &format!("{left}a ≤ b{right}"));
        let decoded = Sir::from_json(&changed).unwrap();
        prop_assert!(original.diff(&decoded).is_cosmetic());
        prop_assert_eq!(original.semantic_hash(), decoded.semantic_hash());
    }
}
