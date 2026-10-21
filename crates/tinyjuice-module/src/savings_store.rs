//! The savings totals on the host's document port.
//!
//! One document, `juice.savings/totals`, holds the whole aggregate in the
//! module's scope. The in-memory ledger stays the working copy: it is loaded
//! from the document on first use and the document is rewritten after every
//! change. A host that serves no storage leaves the totals in memory only.

use std::sync::Arc;

use tinybus::{Connection, Result as BusResult};
use tinyjuice::savings::ledger::SavingsLedger;
use tinyjuice_bus::savings::{
    RecordSavingsRequest, SAVINGS_DOC_ID, SavingsAggregate, StoredSavings,
};
use tinyjuice_bus::storage::SAVINGS_COLLECTION;
use tinystoragedrivers_api::{DocumentStore, DocumentStoreExt, Storage};
use tinystoragedrivers_types::Precondition;

/// Working state behind one lock, so load, change and write never interleave.
#[derive(Default)]
struct State {
    docs: Option<Arc<dyn DocumentStore>>,
    /// The ledger has been reconciled with the stored document (or the module
    /// has no storage connection at all and stays in memory).
    loaded: bool,
    /// The ledger changed before the stored document could be read.
    dirty: bool,
    /// A reset happened before the stored document could be read, so the
    /// stored totals must be discarded rather than added to.
    reset_early: bool,
    /// When the last attempt to reach the document port failed, so a host
    /// without storage is not asked again on every call.
    last_connect_failure: Option<std::time::Instant>,
}

/// How long to wait before asking again for a document port that was absent.
const CONNECT_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(5);

/// The module's savings ledger and its storage.
pub(crate) struct Savings {
    ledger: SavingsLedger,
    cooldown: std::time::Duration,
    state: tokio::sync::Mutex<State>,
}

impl Savings {
    pub(crate) fn new() -> Self {
        Self::with_cooldown(CONNECT_COOLDOWN)
    }

    pub(crate) fn with_cooldown(cooldown: std::time::Duration) -> Self {
        Self {
            ledger: SavingsLedger::new(),
            cooldown,
            state: tokio::sync::Mutex::new(State::default()),
        }
    }

    pub(crate) async fn record(&self, connection: Option<&Connection>, req: &RecordSavingsRequest) {
        let mut state = self.state.lock().await;
        self.load(&mut state, connection).await;
        self.ledger.record(req);
        state.dirty |= !state.loaded;
        self.persist(&state).await;
    }

    pub(crate) async fn stats(&self, connection: Option<&Connection>) -> SavingsAggregate {
        let mut state = self.state.lock().await;
        self.load(&mut state, connection).await;
        self.ledger.stats()
    }

    pub(crate) async fn reset(&self, connection: Option<&Connection>) {
        let mut state = self.state.lock().await;
        self.load(&mut state, connection).await;
        self.ledger.reset();
        if !state.loaded {
            state.dirty = true;
            state.reset_early = true;
        }
        self.persist(&state).await;
    }

    /// Adopt `legacy` when nothing has been recorded. Answers whether it did;
    /// fails while the stored totals cannot be read, so the host retries.
    pub(crate) async fn import(
        &self,
        connection: Option<&Connection>,
        legacy: SavingsAggregate,
    ) -> BusResult<bool> {
        let mut state = self.state.lock().await;
        self.load(&mut state, connection).await;
        if !state.loaded {
            return Err(tinybus::Error::failed(
                "savings storage is not readable yet",
            ));
        }
        if self.ledger.stats().total.events != 0 {
            log::debug!("[tinyjuice_module][savings] import skipped: totals exist");
            return Ok(false);
        }
        let previous = self.ledger.stats();
        self.ledger.replace(legacy);
        if !self.persist(&state).await {
            self.ledger.replace(previous);
            return Err(tinybus::Error::failed("savings could not be written"));
        }
        Ok(true)
    }

    /// Adopt a legacy snapshot through a storage handle supplied for this one
    /// call. This handle is deliberately not cached in `State`: hosts use it
    /// for claim-bound imports, and its authority ends when the call returns.
    pub(crate) async fn import_at(
        &self,
        connection: Option<&Connection>,
        storage: &Storage,
        legacy: SavingsAggregate,
    ) -> BusResult<bool> {
        let mut state = self.state.lock().await;
        self.load(&mut state, connection).await;
        if !state.loaded || state.docs.is_none() {
            return Err(tinybus::Error::failed(
                "savings storage is not readable yet",
            ));
        }
        let docs = storage.documents();
        let stored = docs
            .get_as::<StoredSavings>(SAVINGS_COLLECTION, SAVINGS_DOC_ID)
            .await
            .map_err(|error| {
                tinybus::Error::failed(format!("savings could not be read: {error}"))
            })?;
        if stored
            .as_ref()
            .is_some_and(|stored| stored.doc.total.events != 0)
            || self.ledger.stats().total.events != 0
        {
            log::debug!("[tinyjuice_module][savings] import skipped: totals exist");
            return Ok(false);
        }

        // Import is a read-modify-write operation. Preserve the observed
        // document version (or absence) so a concurrent normal savings write
        // cannot be overwritten by this legacy snapshot.
        let precondition = stored.map_or(Precondition::Absent, |stored| {
            Precondition::Version(stored.version)
        });

        let previous = self.ledger.stats();
        self.ledger.replace(legacy);
        let body = StoredSavings::from_aggregate(&self.ledger.stats(), now_ms());
        if let Err(error) = docs
            .put_as(SAVINGS_COLLECTION, SAVINGS_DOC_ID, &body, precondition)
            .await
        {
            self.ledger.replace(previous);
            return Err(tinybus::Error::failed(format!(
                "savings could not be written: {error}"
            )));
        }

        // The document is now authoritative through the ordinary port already
        // held by this state. Never retain the invocation-scoped handle.
        state.loaded = true;
        state.dirty = false;
        state.reset_early = false;
        Ok(true)
    }

    /// Reach the document port and reconcile the ledger with its document.
    ///
    /// Changes made before the document could be read are kept: they are
    /// added to the stored totals (or replace them after an early reset) and
    /// the result is written back.
    async fn load(&self, state: &mut State, connection: Option<&Connection>) {
        if state.loaded {
            return;
        }
        let Some(connection) = connection else {
            state.loaded = true;
            return;
        };
        if state.docs.is_none() {
            if state
                .last_connect_failure
                .is_some_and(|at| at.elapsed() < self.cooldown)
            {
                return;
            }
            state.docs = crate::storage::documents(connection.clone()).await;
            state.last_connect_failure = state.docs.is_none().then(std::time::Instant::now);
        }
        let Some(docs) = state.docs.as_ref() else {
            return;
        };
        let stored = match docs
            .get_as::<StoredSavings>(SAVINGS_COLLECTION, SAVINGS_DOC_ID)
            .await
        {
            Ok(found) => found.map(|found| found.doc.into_aggregate()),
            Err(error) => {
                log::warn!("[tinyjuice_module][savings] load failed, retrying later: {error}");
                return;
            }
        };
        if let Some(stored) = stored.filter(|_| !state.reset_early) {
            let early = self.ledger.stats();
            self.ledger.replace(stored);
            self.ledger.absorb(&early);
        }
        state.loaded = true;
        if state.dirty {
            log::debug!("[tinyjuice_module][savings] reconciled early changes with storage");
            state.dirty = false;
            state.reset_early = false;
            self.persist(state).await;
        }
    }

    /// Write the ledger. `true` when it is stored, or when there is no
    /// storage to write to; `false` when a write failed.
    async fn persist(&self, state: &State) -> bool {
        // Nothing is written until the stored document has been read, so an
        // unread older document is never overwritten blindly.
        if !state.loaded {
            return true;
        }
        let Some(docs) = state.docs.as_ref() else {
            return true;
        };
        let body = StoredSavings::from_aggregate(&self.ledger.stats(), now_ms());
        match docs
            .put_as(
                SAVINGS_COLLECTION,
                SAVINGS_DOC_ID,
                &body,
                Precondition::None,
            )
            .await
        {
            Ok(_) => true,
            Err(error) => {
                log::warn!("[tinyjuice_module][savings] write failed: {error}");
                false
            }
        }
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

#[cfg(test)]
#[path = "savings_store_tests.rs"]
pub(crate) mod tests;
