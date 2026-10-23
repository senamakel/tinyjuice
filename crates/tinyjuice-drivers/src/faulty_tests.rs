//! Blob handles that misbehave, to pin the store's failure handling.

use std::ops::Range;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tinystoragedrivers_api::BlobStore;
use tinystoragedrivers_types::{Blob, BlobMeta, Capabilities, Result, StorageError};

/// Wraps a real handle; `fail` makes every call error, `ttl` toggles whether
/// it claims `BlobTtl`.
#[derive(Debug)]
pub struct Faulty {
    pub inner: Arc<dyn BlobStore>,
    pub fail: Arc<AtomicBool>,
    pub ttl: bool,
}

impl Faulty {
    fn check(&self) -> Result<()> {
        if self.fail.load(Ordering::SeqCst) {
            Err(StorageError::backend("injected"))
        } else {
            Ok(())
        }
    }
}

#[async_trait]
impl BlobStore for Faulty {
    fn max_key_len(&self) -> usize {
        self.inner.max_key_len()
    }
    async fn put(&self, key: &str, bytes: Vec<u8>, ct: Option<&str>) -> Result<BlobMeta> {
        self.check()?;
        self.inner.put(key, bytes, ct).await
    }
    async fn put_expiring(
        &self,
        key: &str,
        bytes: Vec<u8>,
        ct: Option<&str>,
        ttl: Duration,
    ) -> Result<BlobMeta> {
        self.check()?;
        self.inner.put_expiring(key, bytes, ct, ttl).await
    }
    fn capabilities(&self) -> Capabilities {
        if self.ttl {
            self.inner.capabilities()
        } else {
            Capabilities::none()
        }
    }
    async fn get(&self, key: &str) -> Result<Option<Blob>> {
        self.check()?;
        self.inner.get(key).await
    }
    async fn get_range(&self, key: &str, range: Range<u64>) -> Result<Option<Vec<u8>>> {
        self.check()?;
        self.inner.get_range(key, range).await
    }
    async fn head(&self, key: &str) -> Result<Option<BlobMeta>> {
        self.check()?;
        self.inner.head(key).await
    }
    async fn delete(&self, key: &str) -> Result<bool> {
        self.check()?;
        self.inner.delete(key).await
    }
    async fn list(&self, prefix: &str) -> Result<Vec<BlobMeta>> {
        self.check()?;
        self.inner.list(prefix).await
    }
}
