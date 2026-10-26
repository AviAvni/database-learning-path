//! An object store with the four operations table formats are built on, and
//! a meter on every one of them.
//!
//! The model is S3-shaped and deliberately minimal:
//!
//! - `put` overwrites (an object is replaced whole, never patched — Delta
//!   VLDB'20 §2.1, "Updating objects usually requires rewriting the whole
//!   object at once");
//! - `put_if_absent` and `put_if_match` are the two **conditional puts** the
//!   commit protocols need: create-only (Delta's `NNN.json`, PROTOCOL.md
//!   "Commit Protocol", option 3) and compare-and-swap on a version tag
//!   (Iceberg's metastore "check-and-put", spec "Metastore Tables");
//! - `get` returns the whole object;
//! - `list` is lexicographic from a start key and returns at most
//!   [`LIST_PAGE`] keys per call (Delta VLDB'20 §2.1: "S3's LIST only returns
//!   up to 1000 keys per call"), so listing k keys costs ⌈k/1000⌉ calls that
//!   must run one after another — each page's continuation is the last key of
//!   the previous page.
//!
//! The store is a `BTreeMap`, so `list` is ordered by construction and every
//! count below is a deterministic function of the operations issued. No
//! latency is modelled here: the meter counts requests, bytes and dependent
//! rounds, and the guides convert rounds to time only with a cited figure.

use std::collections::BTreeMap;

/// Maximum keys a single LIST call returns (S3; Delta VLDB'20 §2.1).
pub const LIST_PAGE: usize = 1000;

/// What a sequence of requests cost.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Meter {
    pub puts: u64,
    pub put_bytes: u64,
    /// conditional puts that lost (object existed / tag moved)
    pub failed_puts: u64,
    pub gets: u64,
    pub get_bytes: u64,
    /// LIST *calls* (pages), not keys
    pub lists: u64,
    pub listed_keys: u64,
    /// Requests on the critical path that had to wait for an earlier
    /// response: the minimum number of sequential round trips. A batch of
    /// GETs whose keys are all known counts once; each LIST page counts once.
    pub rounds: u64,
}

impl Meter {
    pub fn requests(&self) -> u64 {
        self.puts + self.failed_puts + self.gets + self.lists
    }
    pub fn minus(&self, before: &Meter) -> Meter {
        Meter {
            puts: self.puts - before.puts,
            put_bytes: self.put_bytes - before.put_bytes,
            failed_puts: self.failed_puts - before.failed_puts,
            gets: self.gets - before.gets,
            get_bytes: self.get_bytes - before.get_bytes,
            lists: self.lists - before.lists,
            listed_keys: self.listed_keys - before.listed_keys,
            rounds: self.rounds - before.rounds,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum PutError {
    /// `put_if_absent` on a key that exists
    Exists,
    /// `put_if_match` whose expected tag is not the current one
    TagMismatch { current: Option<u64> },
}

#[derive(Default)]
pub struct Store {
    objects: BTreeMap<String, (u64, Vec<u8>)>,
    next_tag: u64,
    pub meter: Meter,
}

impl Store {
    pub fn new() -> Self {
        Self::default()
    }

    fn insert(&mut self, key: &str, body: Vec<u8>) -> u64 {
        self.next_tag += 1;
        self.meter.puts += 1;
        self.meter.put_bytes += body.len() as u64;
        self.objects.insert(key.to_string(), (self.next_tag, body));
        self.next_tag
    }

    /// Unconditional whole-object write. Returns the object's new tag.
    pub fn put(&mut self, key: &str, body: Vec<u8>) -> u64 {
        self.insert(key, body)
    }

    /// Create-only write: fails if `key` exists. The primitive Delta's
    /// commit protocol is built on (one writer wins version N+1).
    pub fn put_if_absent(&mut self, key: &str, body: Vec<u8>) -> Result<u64, PutError> {
        if self.objects.contains_key(key) {
            self.meter.failed_puts += 1;
            return Err(PutError::Exists);
        }
        Ok(self.insert(key, body))
    }

    /// Compare-and-swap: succeeds only if the object's current tag is
    /// `expected` (`None` = must not exist). Iceberg's pointer swap.
    pub fn put_if_match(
        &mut self,
        key: &str,
        expected: Option<u64>,
        body: Vec<u8>,
    ) -> Result<u64, PutError> {
        let current = self.objects.get(key).map(|(t, _)| *t);
        if current != expected {
            self.meter.failed_puts += 1;
            return Err(PutError::TagMismatch { current });
        }
        Ok(self.insert(key, body))
    }

    /// Whole-object read; returns the body and its tag.
    pub fn get(&mut self, key: &str) -> Option<(u64, Vec<u8>)> {
        self.meter.gets += 1;
        let hit = self.objects.get(key).cloned();
        if let Some((_, b)) = &hit {
            self.meter.get_bytes += b.len() as u64;
        }
        hit
    }

    /// Keys with `prefix`, strictly after `start_after`, in lexicographic
    /// order, stopping early once `stop` returns true for a key (the caller
    /// has seen enough). Pays one LIST call — and one round — per page of
    /// [`LIST_PAGE`] keys actually returned.
    pub fn list(
        &mut self,
        prefix: &str,
        start_after: Option<&str>,
        mut stop: impl FnMut(&str) -> bool,
    ) -> Vec<String> {
        let mut out = Vec::new();
        let lo = match start_after {
            Some(s) => std::ops::Bound::Excluded(s.to_string()),
            None => std::ops::Bound::Included(prefix.to_string()),
        };
        let mut in_page = 0usize;
        let mut pages = 0u64;
        for (k, _) in self.objects.range((lo, std::ops::Bound::Unbounded)) {
            if !k.starts_with(prefix) {
                break;
            }
            if in_page == 0 {
                pages += 1;
            }
            in_page = (in_page + 1) % LIST_PAGE;
            let done = stop(k);
            out.push(k.clone());
            if done {
                break;
            }
        }
        // an empty listing still costs one call
        let pages = pages.max(1);
        self.meter.lists += pages;
        self.meter.rounds += pages;
        self.meter.listed_keys += out.len() as u64;
        out
    }

    /// Record that the next batch of requests had to wait for the previous
    /// one: one more sequential round trip on the critical path.
    pub fn round(&mut self) {
        self.meter.rounds += 1;
    }

    pub fn object_count(&self, prefix: &str) -> usize {
        self.objects.keys().filter(|k| k.starts_with(prefix)).count()
    }

    pub fn bytes_under(&self, prefix: &str) -> u64 {
        self.objects
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(_, (_, b))| b.len() as u64)
            .sum()
    }

    pub fn bytes_with_suffix(&self, prefix: &str, suffix: &str) -> u64 {
        self.objects
            .iter()
            .filter(|(k, _)| k.starts_with(prefix) && k.ends_with(suffix))
            .map(|(_, (_, b))| b.len() as u64)
            .sum()
    }

    /// Delete (used by snapshot expiry / metadata cleanup). Not metered as a
    /// read or write; the lanes report what is *left*, not what GC cost.
    pub fn delete(&mut self, key: &str) {
        self.objects.remove(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_if_absent_is_create_only() {
        let mut s = Store::new();
        assert!(s.put_if_absent("a", b"1".to_vec()).is_ok());
        assert_eq!(s.put_if_absent("a", b"2".to_vec()), Err(PutError::Exists));
        assert_eq!(s.get("a").unwrap().1, b"1");
        assert_eq!(s.meter.failed_puts, 1);
    }

    #[test]
    fn put_if_match_is_cas() {
        let mut s = Store::new();
        let t1 = s.put_if_match("p", None, b"v1".to_vec()).unwrap();
        assert!(s.put_if_match("p", None, b"x".to_vec()).is_err());
        let t2 = s.put_if_match("p", Some(t1), b"v2".to_vec()).unwrap();
        assert!(s.put_if_match("p", Some(t1), b"v3".to_vec()).is_err());
        assert_eq!(s.get("p").unwrap(), (t2, b"v2".to_vec()));
    }

    #[test]
    fn list_pages_at_1000() {
        let mut s = Store::new();
        for i in 0..2500 {
            s.put(&format!("log/{i:05}"), vec![]);
        }
        s.put("other/x", vec![]);
        let before = s.meter;
        let keys = s.list("log/", None, |_| false);
        let m = s.meter.minus(&before);
        assert_eq!(keys.len(), 2500);
        assert_eq!(m.lists, 3);
        assert_eq!(m.rounds, 3);
        let keys = s.list("log/", Some("log/02399"), |_| false);
        assert_eq!(keys.len(), 100);
        assert_eq!(keys[0], "log/02400");
    }
}
