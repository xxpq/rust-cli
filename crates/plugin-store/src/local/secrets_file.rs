//! `plugin-secrets.json` 的持锁读改写（plugin-store.md §10；Node `updatePluginSecretsFile`）：持存储锁时再拿私有文件自己的锁
//! （锁序，§4），原子写，0600。文件不合 schema 时先照 Node 备份成 `<文件>.corrupt-<内容哈希前 24 位>.bak`，再当空文件改；
//! 内容没变就不写。

use rust_cli_fs::{FileMode, LockOptions, ParentDir, TempName, WriteOptions};

use super::Locked;
use crate::core::diagnostic::StoreError;
use crate::core::secrets::SecretsFile;

/// 私有文件的写法（Node `atomicWritePrivateTextFile`）。
const PRIVATE_WRITE: WriteOptions = WriteOptions {
    mode: FileMode::Private,
    temp_name: TempName::Hidden,
    parent: ParentDir::Create,
    rename_retry: rust_cli_fs::NODE_RENAME_RETRY,
};
/// 损坏文件的备份名里取内容哈希的前几位（Node `backupCorruptFile`）。
const CORRUPT_ID_LEN: usize = 24;

impl Locked<'_> {
    /// 改私有文件（`edit` 改内存里的模型）。
    pub async fn update_secrets(
        &self,
        edit: impl FnOnce(&mut SecretsFile),
    ) -> Result<(), StoreError> {
        let path = self.store.layout.secrets().to_path_buf();
        let result = rust_cli_fs::with_file_lock(&path, &LockOptions::default(), async || {
            let bytes = rust_cli_fs::read_optional(&path).await?;
            let (mut file, before) = match &bytes {
                None => (SecretsFile::default(), Some(SecretsFile::default().to_text())),
                Some(bytes) => match SecretsFile::parse(bytes) {
                    Some(file) => {
                        let text = file.to_text();
                        (file, Some(text))
                    }
                    None => {
                        let id = &rust_cli_base::hash::sha256_hex(bytes)[..CORRUPT_ID_LEN];
                        let mut backup = path.clone().into_os_string();
                        backup.push(format!(".corrupt-{id}.bak"));
                        rust_cli_fs::atomic_write(std::path::Path::new(&backup), bytes, PRIVATE_WRITE).await?;
                        tracing::warn!(event = "plugins.secrets.corrupt", path = %path.display(), "the plugin secrets file was corrupt and is backed up");
                        (SecretsFile::default(), None)
                    }
                },
            };
            edit(&mut file);
            let text = file.to_text();
            if before.as_deref() == Some(text.as_str()) {
                return Ok(());
            }
            rust_cli_fs::atomic_write(&path, text.as_bytes(), PRIVATE_WRITE).await
        })
        .await;
        result.map_err(|error: std::io::Error| StoreError::io(&path, &error))
    }
}
