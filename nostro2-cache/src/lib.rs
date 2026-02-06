use std::sync::{Arc, Mutex};

/// Event ID deduplication cache using std::sync::Mutex with LRU eviction
///
/// This is the winning strategy from benchmarks - fastest under realistic
/// multi-threaded relay pool scenarios (10-20 concurrent connections).
///
/// Pros:
/// - Automatic LRU eviction, bounded memory
/// - Excellent performance under realistic concurrency
/// - Zero external dependencies beyond lru crate
/// - Simple, predictable behavior
pub struct Cache {
    cache: Arc<Mutex<lru::LruCache<String, ()>>>,
}

impl Cache {
    /// Create a new cache with the specified capacity
    ///
    /// # Arguments
    /// * `capacity` - Maximum number of event IDs to cache
    ///
    /// # Example
    /// ```
    /// use nostro2_cache::Cache;
    ///
    /// let cache = Cache::new(10_000);
    /// ```
    pub fn new(capacity: usize) -> Self {
        Self {
            cache: Arc::new(Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(capacity).unwrap(),
            ))),
        }
    }

    /// Insert an event ID into the cache
    ///
    /// Returns `true` if this is a new event (not seen before),
    /// `false` if the event was already in the cache (duplicate).
    ///
    /// # Example
    /// ```
    /// use nostro2_cache::Cache;
    ///
    /// let cache = Cache::new(10_000);
    ///
    /// if cache.insert("event_id_123".to_string()) {
    ///     println!("New event!");
    /// } else {
    ///     println!("Duplicate, skip");
    /// }
    /// ```
    pub fn insert(&self, id: String) -> bool {
        let mut cache = self.cache.lock().unwrap();
        cache.put(id, ()).is_none()
    }

    /// Check if the cache contains an event ID
    pub fn contains(&self, id: &str) -> bool {
        let mut cache = self.cache.lock().unwrap();
        cache.get(id).is_some()
    }

    /// Get the current number of cached event IDs
    pub fn len(&self) -> usize {
        self.cache.lock().unwrap().len()
    }

    /// Check if the cache is empty
    pub fn is_empty(&self) -> bool {
        self.cache.lock().unwrap().is_empty()
    }
}

impl Clone for Cache {
    fn clone(&self) -> Self {
        Self {
            cache: Arc::clone(&self.cache),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_basic() {
        let cache = Cache::new(10);
        assert!(cache.insert("id1".to_string()));
        assert!(!cache.insert("id1".to_string())); // Duplicate
        assert!(cache.contains("id1"));
    }

    #[test]
    fn test_cache_lru_eviction() {
        let cache = Cache::new(3);

        // Fill cache
        cache.insert("id1".to_string());
        cache.insert("id2".to_string());
        cache.insert("id3".to_string());

        // Insert 4th item, should evict oldest (id1)
        cache.insert("id4".to_string());

        assert!(!cache.contains("id1")); // Evicted
        assert!(cache.contains("id2"));
        assert!(cache.contains("id3"));
        assert!(cache.contains("id4"));
    }

    #[test]
    fn test_cache_len() {
        let cache = Cache::new(10);
        assert_eq!(cache.len(), 0);

        cache.insert("id1".to_string());
        assert_eq!(cache.len(), 1);

        cache.insert("id1".to_string()); // Duplicate
        assert_eq!(cache.len(), 1);

        cache.insert("id2".to_string());
        assert_eq!(cache.len(), 2);
    }
}
