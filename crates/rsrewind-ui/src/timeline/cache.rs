//! A least-recently-used cache bounded by bytes, not entries.
//!
//! Frames vary in size, so a count limit would bound nothing. Used twice: decoded thumbnails in
//! memory (UI thread) and their GPU textures (renderer). Eviction can be told to spare entries the
//! current frame needs, so a texture is never dropped while it is on screen; the budget may then be
//! exceeded until those entries leave the screen.

use std::collections::HashMap;
use std::hash::Hash;

pub struct ByteLru<K, V> {
    entries: HashMap<K, Entry<V>>,
    clock: u64,
    bytes: usize,
}

struct Entry<V> {
    value: V,
    bytes: usize,
    used: u64,
}

impl<K: Eq + Hash + Clone, V> Default for ByteLru<K, V> {
    fn default() -> Self {
        Self::new()
    }
}

impl<K: Eq + Hash + Clone, V> ByteLru<K, V> {
    pub fn new() -> Self {
        Self {
            entries: HashMap::new(),
            clock: 0,
            bytes: 0,
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Total bytes of every entry.
    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn contains(&self, key: &K) -> bool {
        self.entries.contains_key(key)
    }

    /// Every key, in no particular order.
    pub fn keys(&self) -> impl Iterator<Item = &K> {
        self.entries.keys()
    }

    /// The value, marking it most recently used.
    pub fn get(&mut self, key: &K) -> Option<&V> {
        self.clock += 1;
        let clock = self.clock;
        self.entries.get_mut(key).map(|e| {
            e.used = clock;
            &e.value
        })
    }

    /// Looks without changing the order.
    pub fn peek(&self, key: &K) -> Option<&V> {
        self.entries.get(key).map(|e| &e.value)
    }

    /// Inserts or replaces, as most recently used. Does not evict; call [`ByteLru::evict_to`].
    pub fn insert(&mut self, key: K, value: V, bytes: usize) {
        self.clock += 1;
        let entry = Entry {
            value,
            bytes,
            used: self.clock,
        };
        if let Some(old) = self.entries.insert(key, entry) {
            self.bytes -= old.bytes;
        }
        self.bytes += bytes;
    }

    pub fn remove(&mut self, key: &K) -> Option<V> {
        let entry = self.entries.remove(key)?;
        self.bytes -= entry.bytes;
        Some(entry.value)
    }

    /// Evicts least recently used entries until at most `budget` bytes remain, never evicting an
    /// entry for which `keep` is true. Returns what was evicted, oldest first.
    pub fn evict_to(&mut self, budget: usize, keep: impl Fn(&K) -> bool) -> Vec<(K, V)> {
        let mut evicted = Vec::new();
        if self.bytes <= budget {
            return evicted;
        }
        let mut candidates: Vec<(u64, K)> = self
            .entries
            .iter()
            .filter(|(k, _)| !keep(k))
            .map(|(k, e)| (e.used, k.clone()))
            .collect();
        candidates.sort_by_key(|(used, _)| *used);
        for (_, key) in candidates {
            if self.bytes <= budget {
                break;
            }
            if let Some(value) = self.remove(&key) {
                evicted.push((key, value));
            }
        }
        evicted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evicts_least_recently_used_first_by_bytes() {
        let mut lru = ByteLru::new();
        lru.insert("a", 1, 40);
        lru.insert("b", 2, 40);
        lru.insert("c", 3, 40);
        assert_eq!(lru.bytes(), 120);
        assert_eq!(lru.get(&"a"), Some(&1)); // a is now the newest
        let evicted: Vec<&str> = lru
            .evict_to(80, |_| false)
            .into_iter()
            .map(|(k, _)| k)
            .collect();
        assert_eq!(evicted, ["b"]);
        assert_eq!(lru.bytes(), 80);
        assert!(lru.contains(&"a") && lru.contains(&"c"));
    }

    #[test]
    fn kept_entries_survive_even_over_budget() {
        let mut lru = ByteLru::new();
        for (i, k) in ["a", "b", "c"].into_iter().enumerate() {
            lru.insert(k, i, 50);
        }
        let evicted = lru.evict_to(0, |k| *k != "b");
        assert_eq!(evicted.len(), 1);
        assert_eq!(lru.len(), 2);
        assert_eq!(lru.bytes(), 100, "over budget: the rest is on screen");
    }

    #[test]
    fn replacing_an_entry_keeps_the_byte_count_right() {
        let mut lru = ByteLru::new();
        lru.insert(1, "x", 10);
        lru.insert(1, "y", 25);
        assert_eq!((lru.len(), lru.bytes()), (1, 25));
        assert_eq!(lru.peek(&1), Some(&"y"));
        assert_eq!(lru.remove(&1), Some("y"));
        assert_eq!(lru.bytes(), 0);
        assert!(lru.is_empty());
        assert!(lru.evict_to(0, |_| false).is_empty());
    }
}
