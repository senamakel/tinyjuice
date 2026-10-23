//! Runtime bridge for the synchronous CCR cache port.

use std::fmt;
use std::future::Future;
use std::sync::mpsc::{self, Sender};
use std::thread;

use tinystoragedrivers_types::StorageError;

type Job = Box<dyn FnOnce(&tokio::runtime::Runtime) + Send + 'static>;

thread_local! {
    static ON_BRIDGE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Runs asynchronous storage operations for the synchronous `CcrStore` trait.
#[derive(Clone)]
pub(crate) struct Blocking {
    jobs: Sender<Job>,
}

impl fmt::Debug for Blocking {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Blocking").finish_non_exhaustive()
    }
}

impl Blocking {
    pub(crate) fn new() -> Result<Self, StorageError> {
        let (jobs, receiver) = mpsc::channel::<Job>();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(runtime_build_error)?;
        thread::Builder::new()
            .name("tinyjuice-storage-bridge".into())
            .spawn(move || {
                ON_BRIDGE.with(|active| active.set(true));
                while let Ok(job) = receiver.recv() {
                    job(&runtime);
                }
            })
            .map_err(thread_start_error)?;
        Ok(Self { jobs })
    }

    pub(crate) fn run<F, T>(&self, future: F) -> Result<T, StorageError>
    where
        F: Future<Output = T> + Send + 'static,
        T: Send + 'static,
    {
        if ON_BRIDGE.with(std::cell::Cell::get) {
            return Err(StorageError::backend("storage bridge re-entered"));
        }
        let (reply, wait) = mpsc::sync_channel(1);
        self.jobs
            .send(Box::new(move |runtime| {
                let _ = reply.send(runtime.block_on(future));
            }))
            .map_err(|_| StorageError::backend("storage bridge stopped"))?;
        wait.recv()
            .map_err(|_| StorageError::backend("storage bridge dropped a call"))
    }
}

fn runtime_build_error(error: std::io::Error) -> StorageError {
    StorageError::backend("cannot build the storage bridge runtime").with_source(error)
}

fn thread_start_error(error: std::io::Error) -> StorageError {
    StorageError::backend("cannot start the storage bridge").with_source(error)
}

#[cfg(test)]
#[path = "blocking_tests.rs"]
mod tests;
