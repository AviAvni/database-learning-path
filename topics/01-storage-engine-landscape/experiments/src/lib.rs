//! Common Engine trait over fjall (LSM) and redb (B-tree) so the shootout
//! benches both through identical code paths.
//!
//! Durability parity (Fair Benchmarking §3.2): both engines run in buffered
//! mode — fjall `PersistMode::Buffer`, redb `Durability::None` — so neither
//! pays fsync per batch while the other doesn't. Flip both to durable mode
//! together if you want the fsync-bound comparison.

use redb::ReadableTable;
use std::path::Path;

pub const VALUE_SIZE: usize = 100;

pub fn key_bytes(i: u64) -> [u8; 8] {
    i.to_be_bytes()
}

pub fn value_for(i: u64) -> Vec<u8> {
    let mut v = vec![0u8; VALUE_SIZE];
    v[..8].copy_from_slice(&i.to_le_bytes());
    v
}

pub trait Engine {
    fn name(&self) -> &'static str;
    fn put_batch(&mut self, items: &[(u64, Vec<u8>)]);
    fn get(&self, key: u64) -> Option<Vec<u8>>;
    fn scan_count(&self) -> usize;
    /// Force everything to disk (used before measuring on-disk size).
    fn sync(&mut self);
}

pub struct FjallEngine {
    keyspace: fjall::Keyspace,
    part: fjall::PartitionHandle,
}

impl FjallEngine {
    pub fn open(path: &Path) -> Self {
        let keyspace = fjall::Config::new(path).open().unwrap();
        let part = keyspace
            .open_partition("data", fjall::PartitionCreateOptions::default())
            .unwrap();
        Self { keyspace, part }
    }
}

impl Engine for FjallEngine {
    fn name(&self) -> &'static str {
        "fjall"
    }

    fn put_batch(&mut self, items: &[(u64, Vec<u8>)]) {
        for (k, v) in items {
            self.part.insert(key_bytes(*k), v.as_slice()).unwrap();
        }
        self.keyspace.persist(fjall::PersistMode::Buffer).unwrap();
    }

    fn get(&self, key: u64) -> Option<Vec<u8>> {
        self.part.get(key_bytes(key)).unwrap().map(|s| s.to_vec())
    }

    fn scan_count(&self) -> usize {
        self.part.iter().count()
    }

    fn sync(&mut self) {
        self.keyspace.persist(fjall::PersistMode::SyncAll).unwrap();
    }
}

const TABLE: redb::TableDefinition<&[u8], &[u8]> = redb::TableDefinition::new("data");

pub struct RedbEngine {
    db: redb::Database,
}

impl RedbEngine {
    pub fn open(path: &Path) -> Self {
        let db = redb::Database::create(path.join("db.redb")).unwrap();
        Self { db }
    }
}

impl Engine for RedbEngine {
    fn name(&self) -> &'static str {
        "redb"
    }

    fn put_batch(&mut self, items: &[(u64, Vec<u8>)]) {
        let mut txn = self.db.begin_write().unwrap();
        txn.set_durability(redb::Durability::None);
        {
            let mut table = txn.open_table(TABLE).unwrap();
            for (k, v) in items {
                table
                    .insert(key_bytes(*k).as_slice(), v.as_slice())
                    .unwrap();
            }
        }
        txn.commit().unwrap();
    }

    fn get(&self, key: u64) -> Option<Vec<u8>> {
        let txn = self.db.begin_read().unwrap();
        let table = txn.open_table(TABLE).unwrap();
        table
            .get(key_bytes(key).as_slice())
            .unwrap()
            .map(|g| g.value().to_vec())
    }

    fn scan_count(&self) -> usize {
        let txn = self.db.begin_read().unwrap();
        let table = txn.open_table(TABLE).unwrap();
        table.iter().unwrap().count()
    }

    fn sync(&mut self) {
        let mut txn = self.db.begin_write().unwrap();
        txn.set_durability(redb::Durability::Immediate);
        txn.open_table(TABLE).unwrap();
        txn.commit().unwrap();
    }
}
