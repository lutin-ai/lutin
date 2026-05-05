use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};

use crate::loop_control::LoopDetection;

pub struct LoopDetector {
    mode: LoopDetection,
    last_sig: Option<u64>,
    streak: u32,
}

impl LoopDetector {
    pub fn new(mode: LoopDetection) -> Self {
        Self { mode, last_sig: None, streak: 0 }
    }

    pub fn check(&mut self, tool_calls: &[lutin_llm::ToolCall]) -> Option<String> {
        let threshold = match self.mode {
            LoopDetection::Disabled => return None,
            LoopDetection::SameToolCallRepeated { threshold } => threshold,
        };
        if tool_calls.is_empty() {
            self.last_sig = None;
            self.streak = 0;
            return None;
        }
        let sig = signature(tool_calls);
        match self.last_sig {
            Some(prev) if prev == sig => {
                self.streak += 1;
                if self.streak >= threshold {
                    return Some(format!("same tool call repeated {} times", self.streak));
                }
            }
            _ => {
                self.last_sig = Some(sig);
                self.streak = 1;
            }
        }
        None
    }
}

// Why: provider argument formatting (whitespace, key order) varies across rounds even when
// the semantic call is identical. Hash a structural normalization so detection isn't fooled.
fn signature(calls: &[lutin_llm::ToolCall]) -> u64 {
    // Sort by id so per-round emission order doesn't perturb the hash.
    let mut idx: Vec<usize> = (0..calls.len()).collect();
    idx.sort_by(|&a, &b| calls[a].id.as_str().cmp(calls[b].id.as_str()));

    let mut hasher = DefaultHasher::new();
    (idx.len() as u64).hash(&mut hasher);
    for i in idx {
        let c = &calls[i];
        c.name.as_str().hash(&mut hasher);
        hash_value(&c.arguments, &mut hasher);
    }
    hasher.finish()
}

fn hash_value(v: &serde_json::Value, h: &mut DefaultHasher) {
    use serde_json::Value;
    match v {
        Value::Null => 0u8.hash(h),
        Value::Bool(b) => {
            1u8.hash(h);
            b.hash(h);
        }
        Value::Number(n) => {
            2u8.hash(h);
            // Why: JSON Number compares by lexical form via to_string; sufficient and stable.
            n.to_string().hash(h);
        }
        Value::String(s) => {
            3u8.hash(h);
            s.hash(h);
        }
        Value::Array(arr) => {
            4u8.hash(h);
            (arr.len() as u64).hash(h);
            for item in arr {
                hash_value(item, h);
            }
        }
        Value::Object(map) => {
            5u8.hash(h);
            // Why: serde_json's default map preserves insertion order; sort keys for canonicality.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            (keys.len() as u64).hash(h);
            for k in keys {
                k.hash(h);
                hash_value(&map[k], h);
            }
        }
    }
}
