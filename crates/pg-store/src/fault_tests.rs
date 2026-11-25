//! 真实 TCP 断线：分别丢弃 COMMIT 请求、服务器已提交后的回包，以及 COMMIT 之前的业务语句。
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};

use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgSslMode};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

use crate::tests::{batch, fixture, meta};
use crate::*;
use rust_cli_kernel_api::store::SessionStore;

const INSERT_MESSAGE: &[u8] = b"INSERT INTO \"zcode\".\"message\"";

struct Proxy {
    port: u16,
    mode: Arc<AtomicU8>,
    task: JoinHandle<()>,
}
impl Drop for Proxy {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Proxy {
    #[expect(
        clippy::disallowed_methods,
        reason = "故障测试仅代理独立本机 PG 的 TCP"
    )]
    async fn start(port: u16) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_port = listener.local_addr().unwrap().port();
        let mode = Arc::new(AtomicU8::new(0));
        let selected = mode.clone();
        let task = tokio::spawn(async move {
            let mut connections = tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let (client, _) = accepted.unwrap();
                        let mode = selected.clone();
                        connections.spawn(async move {
                            let server = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
                            let (mut cr, mut cw) = client.into_split();
                            let (mut sr, mut sw) = server.into_split();
                            let incoming = async {
                                let mut buffer = vec![0; 65536];
                                let mut tail = Vec::new();
                                loop {
                                    let size = cr.read(&mut buffer).await?;
                                    if size == 0 { return Ok::<_, std::io::Error>(()); }
                                    tail.extend_from_slice(&buffer[..size]);
                                    // 模式 3：业务 ORM 语句阶段断线（CR-01），事务尚未进入 COMMIT。
                                    if mode.load(Ordering::SeqCst) == 3 && tail.windows(INSERT_MESSAGE.len()).any(|bytes| bytes == INSERT_MESSAGE) {
                                        mode.store(0, Ordering::SeqCst);
                                        return Ok(());
                                    }
                                    if mode.load(Ordering::SeqCst) == 1 && tail.windows(7).any(|bytes| bytes == b"COMMIT\0") {
                                        mode.store(0, Ordering::SeqCst);
                                        return Ok(());
                                    }
                                    sw.write_all(&buffer[..size]).await?;
                                    if tail.len() > INSERT_MESSAGE.len() { tail.drain(..tail.len()-INSERT_MESSAGE.len()); }
                                }
                            };
                            let outgoing = async {
                                loop {
                                    let tag = sr.read_u8().await?;
                                    let size = sr.read_u32().await?;
                                    assert!((4..16_777_216).contains(&size));
                                    let mut body = vec![0; (size-4) as usize];
                                    sr.read_exact(&mut body).await?;
                                    if tag == b'C' && body == b"COMMIT\0" && mode.compare_exchange(2, 0, Ordering::SeqCst, Ordering::SeqCst).is_ok() {
                                        return Ok::<_, std::io::Error>(());
                                    }
                                    cw.write_u8(tag).await?; cw.write_u32(size).await?; cw.write_all(&body).await?;
                                }
                            };
                            tokio::select! { _ = incoming => {}, _ = outgoing => {} }
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
        });
        Self {
            port: local_port,
            mode,
            task,
        }
    }
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
#[expect(clippy::disallowed_methods, reason = "故障测试读取独立本机 PG 连接")]
async fn lost_commit_reply_requires_reloading_state_without_replay() {
    let (_database, store) = fixture("disconnect").await;
    let url = std::env::var("ZCODE_TEST_POSTGRES_URL").unwrap();
    let options = PgConnectOptions::from_str(&url).unwrap();
    assert_eq!(options.get_host(), "127.0.0.1");
    let proxy = Proxy::start(options.get_port()).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(
            options
                .port(proxy.port)
                .ssl_mode(PgSslMode::Disable)
                .disable_statement_logging(),
        )
        .await
        .unwrap();
    let database = PgDatabase::from_pool(pool).await.unwrap();
    let mut remote = store.clone();
    remote.orm = sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(database.pool.clone());
    let session = SessionId::new("one");
    store.create(meta("one"), batch("initial")).await.unwrap();
    for (mode, id) in [(1, "before"), (2, "after")] {
        let facts = batch(id);
        proxy.mode.store(mode, Ordering::SeqCst);
        assert!(matches!(
            remote.commit(&session, facts).await,
            Err(StoreError::OutcomeUnknown)
        ));
        let loaded = store.load(&session).await.unwrap();
        assert_eq!(
            loaded
                .messages
                .iter()
                .any(|message| message.id().as_str() == id),
            mode == 2
        );
    }
    assert_eq!(store.load(&session).await.unwrap().messages.len(), 2);
    database.close().await;
}

#[tokio::test]
#[ignore = "requires isolated PostgreSQL"]
#[expect(clippy::disallowed_methods, reason = "故障测试读取独立本机 PG 连接")]
async fn statement_disconnect_before_commit_is_unavailable() {
    let (_database, store) = fixture("statement-disconnect").await;
    let url = std::env::var("ZCODE_TEST_POSTGRES_URL").unwrap();
    let options = PgConnectOptions::from_str(&url).unwrap();
    let proxy = Proxy::start(options.get_port()).await;
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(
            options
                .port(proxy.port)
                .ssl_mode(PgSslMode::Disable)
                .disable_statement_logging(),
        )
        .await
        .unwrap();
    let database = PgDatabase::from_pool(pool).await.unwrap();
    let mut remote = store.clone();
    remote.orm = sea_orm::SqlxPostgresConnector::from_sqlx_postgres_pool(database.pool.clone());
    let session = SessionId::new("one");
    store.create(meta("one"), batch("initial")).await.unwrap();

    proxy.mode.store(3, Ordering::SeqCst);
    let result = remote.commit(&session, batch("mid")).await;
    assert!(
        matches!(result, Err(StoreError::Unavailable { .. })),
        "{result:?}"
    );
    assert_eq!(store.load(&session).await.unwrap().messages.len(), 1);
    database.close().await;
}
