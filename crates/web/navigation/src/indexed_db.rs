//! Minimal IndexedDB object-store model with pluggable storage.
//!
//! The model exposes the small, deterministic subset needed by the browser
//! shell: [`open`], `createObjectStore`, `put`, `get`, `delete` and transactions.
//! All persistence goes through the [`IdbBackend`] trait; [`MemoryIdbBackend`]
//! is the default implementation used for tests and ephemeral profiles, and a
//! real backend can be supplied later via [`open_with_backend`].
//!
//! Transactions buffer writes and apply them atomically on [`IdbTransaction::commit`].
//! Dropping a transaction without committing discards its writes (an implicit
//! abort), and reads inside a transaction observe its own pending writes.
//!
//! ```
//! use browser_navigation::{open, IdbKey, IdbValue};
//!
//! let mut db = open("demo", 1);
//! db.create_object_store("notes").unwrap();
//! db.put("notes", IdbKey::string("a"), IdbValue::String("hello".into()))
//!     .unwrap();
//! assert_eq!(
//!     db.get("notes", &IdbKey::string("a")).unwrap(),
//!     Some(IdbValue::String("hello".into()))
//! );
//! ```

use std::collections::BTreeMap;
use std::fmt;

/// Identifier of one transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct IdbTransactionId(pub u64);

impl fmt::Display for IdbTransactionId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "idb-tx-{}", self.0)
    }
}

/// A structured key used by an object store.
///
/// Number and string keys keep their natural ordering, which makes iteration
/// deterministic for the in-memory backend.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum IdbKey {
    /// Integer key.
    Number(i64),
    /// String key.
    String(String),
}

impl IdbKey {
    /// Creates a number key.
    pub const fn number(value: i64) -> Self {
        Self::Number(value)
    }

    /// Creates a string key.
    pub fn string(value: impl Into<String>) -> Self {
        Self::String(value.into())
    }

    /// The key as a string slice, when it is a string key.
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(value) => Some(value),
            Self::Number(_) => None,
        }
    }

    /// The key as an integer, when it is a number key.
    pub const fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Number(value) => Some(*value),
            Self::String(_) => None,
        }
    }
}

/// A structured-cloneable value stored in an object store.
#[derive(Debug, Clone, PartialEq)]
pub enum IdbValue {
    /// The JavaScript `null`/`undefined` placeholder.
    Null,
    /// Boolean value.
    Bool(bool),
    /// IEEE-754 number.
    Number(f64),
    /// String value.
    String(String),
    /// Opaque binary value.
    Bytes(Vec<u8>),
}

impl From<String> for IdbValue {
    fn from(value: String) -> Self {
        Self::String(value)
    }
}

impl From<Vec<u8>> for IdbValue {
    fn from(value: Vec<u8>) -> Self {
        Self::Bytes(value)
    }
}

/// Access mode of a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdbTransactionMode {
    /// Only `get` is allowed.
    ReadOnly,
    /// `put` and `delete` are allowed in addition to `get`.
    ReadWrite,
}

impl IdbTransactionMode {
    /// Stable lowercase name of the mode.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "readonly",
            Self::ReadWrite => "readwrite",
        }
    }

    /// True for a read-only transaction.
    pub const fn is_read_only(self) -> bool {
        matches!(self, Self::ReadOnly)
    }
}

/// Lifecycle state of a transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IdbTransactionState {
    /// The transaction accepts operations.
    Active,
    /// The transaction committed its writes.
    Committed,
    /// The transaction was aborted or dropped without committing.
    Aborted,
}

/// An operation recorded in a transaction's log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdbOperation {
    /// A value was staged for writing.
    Put {
        /// Target object store.
        store: String,
        /// Key that was written.
        key: IdbKey,
    },
    /// A value was read (or found missing).
    Get {
        /// Source object store.
        store: String,
        /// Key that was read.
        key: IdbKey,
        /// Whether a value existed.
        found: bool,
    },
    /// A key was staged for deletion.
    Delete {
        /// Target object store.
        store: String,
        /// Key that was deleted.
        key: IdbKey,
        /// Whether a value existed before the delete.
        existed: bool,
    },
}

/// Errors from database and transaction operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdbError {
    /// The database was closed.
    DatabaseClosed,
    /// A referenced object store does not exist.
    StoreNotFound(String),
    /// An object store with that name already exists.
    StoreAlreadyExists(String),
    /// A transaction was requested without any object stores.
    NoStores,
    /// A write was attempted in a read-only transaction.
    ReadOnly {
        /// Store that was targeted.
        store: String,
    },
    /// The transaction is no longer active.
    InactiveTransaction,
    /// The backend rejected an operation.
    Backend(String),
}

impl fmt::Display for IdbError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DatabaseClosed => f.write_str("IndexedDB database is closed"),
            Self::StoreNotFound(name) => write!(f, "object store '{name}' does not exist"),
            Self::StoreAlreadyExists(name) => write!(f, "object store '{name}' already exists"),
            Self::NoStores => f.write_str("a transaction needs at least one object store"),
            Self::ReadOnly { store } => {
                write!(f, "object store '{store}' is read-only in this transaction")
            }
            Self::InactiveTransaction => f.write_str("transaction is no longer active"),
            Self::Backend(message) => write!(f, "IndexedDB backend error: {message}"),
        }
    }
}

impl std::error::Error for IdbError {}

/// Storage backend for an [`IdbDatabase`].
///
/// Implement this trait to add a persistent backend later; the database,
/// transactions and value model do not change.
pub trait IdbBackend: Send {
    /// Creates an object store.
    fn create_object_store(&mut self, name: &str) -> Result<(), IdbError>;

    /// Names of all object stores, in backend-defined order.
    fn object_store_names(&self) -> Vec<String>;

    /// Whether an object store exists.
    fn has_object_store(&self, name: &str) -> bool;

    /// Stores or replaces a value.
    fn put(&mut self, store: &str, key: IdbKey, value: IdbValue) -> Result<(), IdbError>;

    /// Reads a value without removing it.
    fn get(&self, store: &str, key: &IdbKey) -> Result<Option<IdbValue>, IdbError>;

    /// Removes a value, returning whether it existed.
    fn delete(&mut self, store: &str, key: &IdbKey) -> Result<bool, IdbError>;

    /// Removes every value in a store.
    fn clear(&mut self, store: &str) -> Result<(), IdbError>;

    /// Number of values in a store.
    fn count(&self, store: &str) -> Result<usize, IdbError>;
}

/// In-memory [`IdbBackend`] using ordered maps.
#[derive(Debug, Default, Clone)]
pub struct MemoryIdbBackend {
    stores: BTreeMap<String, BTreeMap<IdbKey, IdbValue>>,
}

impl MemoryIdbBackend {
    /// Creates an empty backend.
    pub fn new() -> Self {
        Self::default()
    }
}

impl IdbBackend for MemoryIdbBackend {
    fn create_object_store(&mut self, name: &str) -> Result<(), IdbError> {
        if self.stores.contains_key(name) {
            return Err(IdbError::StoreAlreadyExists(name.to_string()));
        }
        self.stores.insert(name.to_string(), BTreeMap::new());
        Ok(())
    }

    fn object_store_names(&self) -> Vec<String> {
        self.stores.keys().cloned().collect()
    }

    fn has_object_store(&self, name: &str) -> bool {
        self.stores.contains_key(name)
    }

    fn put(&mut self, store: &str, key: IdbKey, value: IdbValue) -> Result<(), IdbError> {
        self.stores
            .get_mut(store)
            .ok_or_else(|| IdbError::StoreNotFound(store.to_string()))?
            .insert(key, value);
        Ok(())
    }

    fn get(&self, store: &str, key: &IdbKey) -> Result<Option<IdbValue>, IdbError> {
        let entries = self
            .stores
            .get(store)
            .ok_or_else(|| IdbError::StoreNotFound(store.to_string()))?;
        Ok(entries.get(key).cloned())
    }

    fn delete(&mut self, store: &str, key: &IdbKey) -> Result<bool, IdbError> {
        let entries = self
            .stores
            .get_mut(store)
            .ok_or_else(|| IdbError::StoreNotFound(store.to_string()))?;
        Ok(entries.remove(key).is_some())
    }

    fn clear(&mut self, store: &str) -> Result<(), IdbError> {
        self.stores
            .get_mut(store)
            .ok_or_else(|| IdbError::StoreNotFound(store.to_string()))?
            .clear();
        Ok(())
    }

    fn count(&self, store: &str) -> Result<usize, IdbError> {
        let entries = self
            .stores
            .get(store)
            .ok_or_else(|| IdbError::StoreNotFound(store.to_string()))?;
        Ok(entries.len())
    }
}

/// A database handle backed by `B`.
#[derive(Debug)]
pub struct IdbDatabase<B: IdbBackend = MemoryIdbBackend> {
    name: String,
    version: u32,
    backend: B,
    open: bool,
    next_tx_id: u64,
}

impl IdbDatabase<MemoryIdbBackend> {
    /// Opens (creates) a database with the default in-memory backend.
    pub fn open(name: impl Into<String>, version: u32) -> Self {
        open_with_backend(name, version, MemoryIdbBackend::new())
    }
}

/// Opens a database with the default in-memory backend.
pub fn open(name: impl Into<String>, version: u32) -> IdbDatabase<MemoryIdbBackend> {
    IdbDatabase::open(name, version)
}

/// Opens a database with a caller-supplied backend.
pub fn open_with_backend<B: IdbBackend>(
    name: impl Into<String>,
    version: u32,
    backend: B,
) -> IdbDatabase<B> {
    IdbDatabase {
        name: name.into(),
        version,
        backend,
        open: true,
        next_tx_id: 0,
    }
}

impl<B: IdbBackend> IdbDatabase<B> {
    /// Database name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Database version supplied at open time.
    pub const fn version(&self) -> u32 {
        self.version
    }

    /// Whether the database is still open.
    pub const fn is_open(&self) -> bool {
        self.open
    }

    /// Read-only access to the backend.
    pub const fn backend(&self) -> &B {
        &self.backend
    }

    /// Mutable access to the backend, for host-specific maintenance.
    pub const fn backend_mut(&mut self) -> &mut B {
        &mut self.backend
    }

    /// Closes the database. All subsequent operations fail with
    /// [`IdbError::DatabaseClosed`].
    pub fn close(&mut self) {
        self.open = false;
    }

    /// Creates an object store.
    pub fn create_object_store(&mut self, name: &str) -> Result<(), IdbError> {
        self.ensure_open()?;
        self.backend.create_object_store(name)
    }

    /// Names of all object stores.
    pub fn object_store_names(&self) -> Vec<String> {
        self.backend.object_store_names()
    }

    /// Starts a transaction scoped to `stores`.
    pub fn transaction(
        &mut self,
        mode: IdbTransactionMode,
        stores: &[&str],
    ) -> Result<IdbTransaction<'_, B>, IdbError> {
        self.ensure_open()?;
        if stores.is_empty() {
            return Err(IdbError::NoStores);
        }
        let mut owned = Vec::with_capacity(stores.len());
        for &store in stores {
            if !self.backend.has_object_store(store) {
                return Err(IdbError::StoreNotFound(store.to_string()));
            }
            if !owned.iter().any(|name| name == store) {
                owned.push(store.to_string());
            }
        }
        self.next_tx_id += 1;
        Ok(IdbTransaction {
            id: IdbTransactionId(self.next_tx_id),
            mode,
            stores: owned,
            db: self,
            writes: Vec::new(),
            operations: Vec::new(),
            state: IdbTransactionState::Active,
        })
    }

    /// Stores a value using an implicit read-write transaction.
    pub fn put(&mut self, store: &str, key: IdbKey, value: IdbValue) -> Result<(), IdbError> {
        let mut transaction = self.transaction(IdbTransactionMode::ReadWrite, &[store])?;
        transaction.put(store, key, value)?;
        transaction.commit().map(|_| ())
    }

    /// Reads a value using an implicit read-only transaction.
    pub fn get(&mut self, store: &str, key: &IdbKey) -> Result<Option<IdbValue>, IdbError> {
        let mut transaction = self.transaction(IdbTransactionMode::ReadOnly, &[store])?;
        let value = transaction.get(store, key)?;
        transaction.commit()?;
        Ok(value)
    }

    /// Deletes a value using an implicit read-write transaction.
    pub fn delete(&mut self, store: &str, key: &IdbKey) -> Result<bool, IdbError> {
        let mut transaction = self.transaction(IdbTransactionMode::ReadWrite, &[store])?;
        let existed = transaction.delete(store, key)?;
        transaction.commit()?;
        Ok(existed)
    }

    fn ensure_open(&self) -> Result<(), IdbError> {
        if self.open {
            Ok(())
        } else {
            Err(IdbError::DatabaseClosed)
        }
    }
}

/// A pending write buffered by a transaction.
#[derive(Debug, Clone, PartialEq)]
struct PendingWrite {
    store: String,
    key: IdbKey,
    /// `None` represents a deletion.
    value: Option<IdbValue>,
}

/// Summary returned by a successful transaction commit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdbCommitReceipt {
    /// Transaction that committed.
    pub transaction: IdbTransactionId,
    /// Number of values written.
    pub puts: usize,
    /// Number of keys deleted.
    pub deletes: usize,
}

/// A transaction scoped to a fixed set of object stores.
///
/// Writes are buffered until [`Self::commit`] and are visible to subsequent
/// reads in the same transaction. Dropping the value without committing discards
/// them, which is equivalent to an abort.
#[derive(Debug)]
pub struct IdbTransaction<'a, B: IdbBackend = MemoryIdbBackend> {
    id: IdbTransactionId,
    mode: IdbTransactionMode,
    stores: Vec<String>,
    db: &'a mut IdbDatabase<B>,
    writes: Vec<PendingWrite>,
    operations: Vec<IdbOperation>,
    state: IdbTransactionState,
}

impl<B: IdbBackend> IdbTransaction<'_, B> {
    /// Transaction identifier.
    pub const fn id(&self) -> IdbTransactionId {
        self.id
    }

    /// Access mode.
    pub const fn mode(&self) -> IdbTransactionMode {
        self.mode
    }

    /// Current state.
    pub const fn state(&self) -> IdbTransactionState {
        self.state
    }

    /// Object stores this transaction is scoped to.
    pub fn stores(&self) -> &[String] {
        &self.stores
    }

    /// Operations performed so far, in order.
    pub fn operations(&self) -> &[IdbOperation] {
        &self.operations
    }

    /// Number of writes buffered but not yet applied to the backend.
    pub fn pending_writes(&self) -> usize {
        self.writes.len()
    }

    /// Stages a value for writing.
    pub fn put(&mut self, store: &str, key: IdbKey, value: IdbValue) -> Result<(), IdbError> {
        self.ensure_write(store)?;
        self.writes.push(PendingWrite {
            store: store.to_string(),
            key: key.clone(),
            value: Some(value),
        });
        self.operations.push(IdbOperation::Put {
            store: store.to_string(),
            key,
        });
        Ok(())
    }

    /// Reads a value, observing this transaction's pending writes first.
    pub fn get(&mut self, store: &str, key: &IdbKey) -> Result<Option<IdbValue>, IdbError> {
        self.ensure_active()?;
        self.ensure_store(store)?;
        let value = match self.pending_value(store, key) {
            Some(value) => value,
            None => self.db.backend.get(store, key)?,
        };
        self.operations.push(IdbOperation::Get {
            store: store.to_string(),
            key: key.clone(),
            found: value.is_some(),
        });
        Ok(value)
    }

    /// Stages a deletion, returning whether the key existed before it.
    pub fn delete(&mut self, store: &str, key: &IdbKey) -> Result<bool, IdbError> {
        self.ensure_write(store)?;
        let existed = match self.pending_value(store, key) {
            Some(value) => value.is_some(),
            None => self.db.backend.get(store, key)?.is_some(),
        };
        self.writes.push(PendingWrite {
            store: store.to_string(),
            key: key.clone(),
            value: None,
        });
        self.operations.push(IdbOperation::Delete {
            store: store.to_string(),
            key: key.clone(),
            existed,
        });
        Ok(existed)
    }

    /// Applies all buffered writes to the backend.
    ///
    /// On success the transaction becomes `Committed`. If the backend rejects a
    /// write, the transaction is consumed and its buffered writes are discarded;
    /// earlier writes in the same batch are not rolled back automatically, so a
    /// persistent backend should implement its own atomicity if required.
    pub fn commit(mut self) -> Result<IdbCommitReceipt, IdbError> {
        self.ensure_active()?;
        let writes = std::mem::take(&mut self.writes);
        let mut puts = 0;
        let mut deletes = 0;
        for write in writes {
            match write.value {
                Some(value) => {
                    self.db.backend.put(&write.store, write.key, value)?;
                    puts += 1;
                }
                None => {
                    self.db.backend.delete(&write.store, &write.key)?;
                    deletes += 1;
                }
            }
        }
        self.state = IdbTransactionState::Committed;
        Ok(IdbCommitReceipt {
            transaction: self.id,
            puts,
            deletes,
        })
    }

    /// Discards all buffered writes.
    pub fn abort(mut self) {
        self.writes.clear();
        self.state = IdbTransactionState::Aborted;
    }

    fn pending_value(&self, store: &str, key: &IdbKey) -> Option<Option<IdbValue>> {
        self.writes
            .iter()
            .rev()
            .find(|write| write.store == store && write.key == *key)
            .map(|write| write.value.clone())
    }

    fn ensure_active(&self) -> Result<(), IdbError> {
        if self.state == IdbTransactionState::Active {
            Ok(())
        } else {
            Err(IdbError::InactiveTransaction)
        }
    }

    fn ensure_store(&self, store: &str) -> Result<(), IdbError> {
        if self.stores.iter().any(|name| name == store) {
            Ok(())
        } else {
            Err(IdbError::StoreNotFound(store.to_string()))
        }
    }

    fn ensure_write(&self, store: &str) -> Result<(), IdbError> {
        self.ensure_active()?;
        if self.mode.is_read_only() {
            return Err(IdbError::ReadOnly {
                store: store.to_string(),
            });
        }
        self.ensure_store(store)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn database() -> IdbDatabase {
        let mut db = open("test", 1);
        db.create_object_store("notes").unwrap();
        db.create_object_store("meta").unwrap();
        db
    }

    #[test]
    fn open_exposes_name_version_and_stores() {
        let db = open("demo", 7);
        assert_eq!(db.name(), "demo");
        assert_eq!(db.version(), 7);
        assert!(db.is_open());
        assert!(db.object_store_names().is_empty());
    }

    #[test]
    fn create_object_store_rejects_duplicates() {
        let mut db = database();
        assert_eq!(
            db.create_object_store("notes"),
            Err(IdbError::StoreAlreadyExists("notes".to_string()))
        );
        assert_eq!(
            db.object_store_names(),
            ["meta".to_string(), "notes".to_string()]
        );
    }

    #[test]
    fn put_get_delete_round_trip() {
        let mut db = database();
        let key = IdbKey::string("a");
        assert_eq!(db.get("notes", &key).unwrap(), None);
        assert!(!db.delete("notes", &key).unwrap());
        db.put("notes", key.clone(), IdbValue::String("hello".into()))
            .unwrap();
        assert_eq!(
            db.get("notes", &key).unwrap(),
            Some(IdbValue::String("hello".into()))
        );
        assert!(db.delete("notes", &key).unwrap());
        assert_eq!(db.get("notes", &key).unwrap(), None);
    }

    #[test]
    fn stores_are_isolated() {
        let mut db = database();
        let key = IdbKey::number(1);
        db.put("notes", key.clone(), IdbValue::Bool(true)).unwrap();
        assert_eq!(db.get("meta", &key).unwrap(), None);
        assert_eq!(db.backend().count("notes").unwrap(), 1);
        assert_eq!(db.backend().count("meta").unwrap(), 0);
    }

    #[test]
    fn all_value_variants_round_trip() {
        let mut db = database();
        let values = [
            IdbValue::Null,
            IdbValue::Bool(false),
            IdbValue::Number(1.5),
            IdbValue::String("s".into()),
            IdbValue::Bytes(vec![0, 1, 2]),
        ];
        for (index, value) in values.iter().enumerate() {
            let key = IdbKey::number(index as i64);
            db.put("notes", key.clone(), value.clone()).unwrap();
            assert_eq!(db.get("notes", &key).unwrap(), Some(value.clone()));
        }
    }

    #[test]
    fn transaction_commits_buffered_writes() {
        let mut db = database();
        let mut tx = db
            .transaction(IdbTransactionMode::ReadWrite, &["notes"])
            .unwrap();
        assert_eq!(tx.id(), IdbTransactionId(1));
        assert_eq!(tx.mode(), IdbTransactionMode::ReadWrite);
        assert_eq!(tx.state(), IdbTransactionState::Active);
        assert_eq!(tx.stores(), ["notes".to_string()]);
        tx.put("notes", IdbKey::string("k"), IdbValue::Number(42.0))
            .unwrap();
        assert_eq!(tx.pending_writes(), 1);
        let receipt = tx.commit().unwrap();
        assert_eq!(receipt.transaction, IdbTransactionId(1));
        assert_eq!(receipt.puts, 1);
        assert_eq!(receipt.deletes, 0);
        assert_eq!(
            db.get("notes", &IdbKey::string("k")).unwrap(),
            Some(IdbValue::Number(42.0))
        );
    }

    #[test]
    fn transaction_reads_its_own_writes_and_deletes() {
        let mut db = database();
        db.put("notes", IdbKey::string("old"), IdbValue::String("v".into()))
            .unwrap();
        let mut tx = db
            .transaction(IdbTransactionMode::ReadWrite, &["notes"])
            .unwrap();
        tx.put("notes", IdbKey::string("new"), IdbValue::String("n".into()))
            .unwrap();
        assert_eq!(
            tx.get("notes", &IdbKey::string("new")).unwrap(),
            Some(IdbValue::String("n".into()))
        );
        assert!(tx.delete("notes", &IdbKey::string("new")).unwrap());
        assert_eq!(tx.get("notes", &IdbKey::string("new")).unwrap(), None);
        // Both changes are buffered until commit.
        assert_eq!(tx.pending_writes(), 2);
        let receipt = tx.commit().unwrap();
        assert_eq!(receipt.puts, 1);
        assert_eq!(receipt.deletes, 1);
        assert_eq!(db.backend().count("notes").unwrap(), 1);
    }

    #[test]
    fn dropping_a_transaction_discards_writes() {
        let mut db = database();
        {
            let mut tx = db
                .transaction(IdbTransactionMode::ReadWrite, &["notes"])
                .unwrap();
            tx.put("notes", IdbKey::string("gone"), IdbValue::Null)
                .unwrap();
        }
        assert_eq!(db.get("notes", &IdbKey::string("gone")).unwrap(), None);
        assert_eq!(db.backend().count("notes").unwrap(), 0);
    }

    #[test]
    fn abort_discards_writes() {
        let mut db = database();
        let mut tx = db
            .transaction(IdbTransactionMode::ReadWrite, &["notes"])
            .unwrap();
        tx.put("notes", IdbKey::string("x"), IdbValue::Bool(true))
            .unwrap();
        tx.abort();
        assert_eq!(db.get("notes", &IdbKey::string("x")).unwrap(), None);
    }

    #[test]
    fn read_only_transactions_reject_writes() {
        let mut db = database();
        let mut tx = db
            .transaction(IdbTransactionMode::ReadOnly, &["notes"])
            .unwrap();
        assert_eq!(tx.mode().as_str(), "readonly");
        assert!(tx.mode().is_read_only());
        assert_eq!(
            tx.put("notes", IdbKey::string("k"), IdbValue::Null),
            Err(IdbError::ReadOnly {
                store: "notes".to_string()
            })
        );
        assert_eq!(
            tx.delete("notes", &IdbKey::string("k")),
            Err(IdbError::ReadOnly {
                store: "notes".to_string()
            })
        );
        tx.get("notes", &IdbKey::string("k")).unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn transaction_validates_scope_and_mode() {
        let mut db = database();
        assert_eq!(
            db.transaction(IdbTransactionMode::ReadOnly, &[]).err(),
            Some(IdbError::NoStores)
        );
        assert_eq!(
            db.transaction(IdbTransactionMode::ReadOnly, &["missing"])
                .err(),
            Some(IdbError::StoreNotFound("missing".to_string()))
        );
        let mut tx = db
            .transaction(IdbTransactionMode::ReadOnly, &["notes"])
            .unwrap();
        assert_eq!(
            tx.get("meta", &IdbKey::string("k")),
            Err(IdbError::StoreNotFound("meta".to_string()))
        );
        tx.commit().unwrap();
    }

    #[test]
    fn operations_are_logged_in_order() {
        let mut db = database();
        let mut tx = db
            .transaction(IdbTransactionMode::ReadWrite, &["notes"])
            .unwrap();
        let key = IdbKey::string("k");
        tx.put("notes", key.clone(), IdbValue::Bool(true)).unwrap();
        assert_eq!(tx.get("notes", &key).unwrap(), Some(IdbValue::Bool(true)));
        assert!(tx.delete("notes", &key).unwrap());
        assert_eq!(
            tx.operations(),
            [
                IdbOperation::Put {
                    store: "notes".into(),
                    key: key.clone()
                },
                IdbOperation::Get {
                    store: "notes".into(),
                    key: key.clone(),
                    found: true
                },
                IdbOperation::Delete {
                    store: "notes".into(),
                    key,
                    existed: true
                },
            ]
        );
        tx.commit().unwrap();
    }

    #[test]
    fn closed_database_rejects_operations() {
        let mut db = database();
        db.close();
        assert!(!db.is_open());
        assert_eq!(
            db.create_object_store("another"),
            Err(IdbError::DatabaseClosed)
        );
        assert_eq!(
            db.put("notes", IdbKey::string("k"), IdbValue::Null),
            Err(IdbError::DatabaseClosed)
        );
        assert_eq!(
            db.get("notes", &IdbKey::string("k")),
            Err(IdbError::DatabaseClosed)
        );
        assert_eq!(
            db.transaction(IdbTransactionMode::ReadOnly, &["notes"])
                .err(),
            Some(IdbError::DatabaseClosed)
        );
    }

    #[test]
    fn backend_errors_propagate() {
        let mut db = open_with_backend("custom", 2, MemoryIdbBackend::new());
        assert_eq!(db.version(), 2);
        assert_eq!(
            db.get("missing", &IdbKey::string("k")),
            Err(IdbError::StoreNotFound("missing".to_string()))
        );
        db.backend_mut().create_object_store("s").unwrap();
        db.put("s", IdbKey::string("k"), IdbValue::Null).unwrap();
        assert_eq!(db.backend().count("s").unwrap(), 1);
        db.backend_mut().clear("s").unwrap();
        assert_eq!(db.backend().count("s").unwrap(), 0);
    }

    #[test]
    fn key_helpers_and_error_display() {
        assert_eq!(IdbKey::string("a").as_str(), Some("a"));
        assert_eq!(IdbKey::number(3).as_i64(), Some(3));
        assert_eq!(IdbKey::number(3).as_str(), None);
        assert_eq!(IdbKey::string("3").as_i64(), None);
        assert_eq!(
            IdbError::StoreNotFound("s".into()).to_string(),
            "object store 's' does not exist"
        );
        assert_eq!(IdbTransactionId(2).to_string(), "idb-tx-2");
    }
}
