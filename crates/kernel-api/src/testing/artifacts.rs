//! 内存里的 Artifact 存储（从 `store.rs` 拆出：它是独立的类型，不依赖 `MemoryStore`）。

use rust_cli_base::sync::LockUnpoisoned;
use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;

use crate::ids::{ArtifactId, SessionId};
use crate::store::{ArtifactPurpose, ArtifactRef, ArtifactStore, StoreError};

/// 内存里的 Artifact（`MemoryStore` 的 artifact 不可用，测试 checkpoint 与落盘时用这个）。
///
/// 与 NodeStore 一样核对会话与大小（缺陷修复 F18：以前只按 id 取、不核对大小，Read 自己造的 `size: 0` 引用在
/// 单测里能读，生产路径上读不回）。
#[derive(Default)]
pub struct MemoryArtifacts {
    objects: Mutex<HashMap<ArtifactId, (ArtifactRef, Vec<u8>)>>,
    next: AtomicU64,
}

impl MemoryArtifacts {
    pub fn count(&self) -> usize {
        self.objects.lock_unpoisoned().len()
    }
}

#[async_trait]
impl ArtifactStore for MemoryArtifacts {
    async fn put(
        &self,
        session: &SessionId,
        purpose: ArtifactPurpose,
        media_type: String,
        bytes: Vec<u8>,
    ) -> Result<ArtifactRef, StoreError> {
        let id = ArtifactId::new(format!(
            "tool-result-{}",
            self.next.fetch_add(1, Ordering::Relaxed) + 1
        ));
        let artifact = ArtifactRef {
            session: session.clone(),
            id: id.clone(),
            purpose,
            media_type,
            size: bytes.len() as u64,
        };
        self.objects
            .lock_unpoisoned()
            .insert(id, (artifact.clone(), bytes));
        Ok(artifact)
    }

    async fn get(&self, artifact: &ArtifactRef) -> Result<Vec<u8>, StoreError> {
        let objects = self.objects.lock_unpoisoned();
        let Some((_, bytes)) = objects
            .get(&artifact.id)
            .filter(|(stored, _)| stored.session == artifact.session)
        else {
            return Err(StoreError::Storage {
                message: format!("artifact {} not found", artifact.id),
            });
        };
        if bytes.len() as u64 != artifact.size {
            return Err(StoreError::Storage {
                message: format!(
                    "artifact {} has {} bytes, expected {}",
                    artifact.id,
                    bytes.len(),
                    artifact.size
                ),
            });
        }
        Ok(bytes.clone())
    }

    async fn find(
        &self,
        session: &SessionId,
        id: &ArtifactId,
    ) -> Result<Option<ArtifactRef>, StoreError> {
        Ok(self
            .objects
            .lock_unpoisoned()
            .get(id)
            .filter(|(stored, _)| &stored.session == session)
            .map(|(stored, _)| stored.clone()))
    }
}
