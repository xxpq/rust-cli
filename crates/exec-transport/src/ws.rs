//! WebSocket 上的连接（docs/specs/rust-cli/remote-exec.md §29.5）：一条二进制消息就是一个完整的帧。
//!
//! WebSocket 自己有消息边界，所以不像 [`crate::stream`] 那样在帧前加 u32 长度，分片由 net 拼回。
//!
//! 收的一侧直接读：exec-client 的读任务独占它（stdio 的收也是这样；net 的读本身也是 cancel-safe 的）。发的一侧交给一个写
//! 任务：一条连接只能有一个写的一方，读的一侧收到 close 要回 close，这个回应也交给它。ping 的 pong 由 net 用的库在读的时候
//! 发（写不动就留到之后，读不等写）。exec-server 也是这样让读的一方回帧。
//!
//! 读的一侧从不等写的一侧：close 的回应走单独的队列，用 `try_send` 放，放不下就丢（见 [`Reply`]）。

use async_trait::async_trait;
use bytes::Bytes;
use rust_cli_base::OwnedTask;
use rust_cli_net::websocket::{CLOSE_NORMAL, WsError, WsFrame, WsReader, WsWriter};
use rust_cli_net::{Egress, HeaderValue};
use tokio::sync::mpsc;

use crate::link::{FrameSink, FrameSource, Link, LinkError};

/// 数据帧的队列只排这么多项，再多就让发的一方等（背压归 exec-client）。
const FRAMES: usize = 1;
/// 回应的队列：一个 close。
const REPLIES: usize = 1;

/// 拨一条 WebSocket：`url`、子协议与这一次连接的请求头（秘密值用 [`HeaderValue::Secret`]）。单条消息的上限是
/// [`crate::MAX_FRAME`]，与 stdio 相同。升级被拒（非 101）时原样交出 [`WsError::Rejected`]，由调用方按状态码判决。
pub async fn dial(
    egress: &Egress,
    url: &str,
    protocols: &[String],
    headers: &[(String, HeaderValue)],
) -> Result<Box<dyn Link>, WsError> {
    let socket = egress
        .websocket(url, (protocols, headers), crate::MAX_FRAME as u64)
        .await?;
    let (reader, writer) = socket.split();
    let (frames, frames_queue) = mpsc::channel(FRAMES);
    let (replies, replies_queue) = mpsc::channel(REPLIES);
    Ok(Box::new(WsLink {
        reader,
        frames,
        replies,
        writing: rust_cli_base::owned(write(writer, frames_queue, replies_queue)),
        peer: url.to_owned(),
    }))
}

/// 读的一侧请写任务发的回应：发 close 帧（带这个代码；对方先关时库回对方的代码与原因）后结束。
///
/// 修复（F142，评审 P2）：以前回应与数据帧共用一条容量 1 的队列，读的一侧 `send().await` 放进去。写任务堵在 TCP 上（对方
/// 不读）时队列是满的，读到 ping 就停住：对方写不进来、我们也读不出去，要等 exec-client 心跳判死（30 s）才重连。现在
/// 回应走自己的队列，读的一侧 `try_send`，放不下就丢：丢 close 的代价是对方收不到回应，它自己关 TCP。pong 自 LIB-SMALL
/// 起由库在读的时候发，不再经这个队列，写不动时库把它留到之后，读照样往下读。
enum Reply {
    Close(u16),
}

struct WsLink {
    reader: WsReader,
    frames: mpsc::Sender<Bytes>,
    replies: mpsc::Sender<Reply>,
    writing: OwnedTask<()>,
    peer: String,
}

impl Link for WsLink {
    fn split(self: Box<Self>) -> (Box<dyn FrameSink>, Box<dyn FrameSource>) {
        let WsLink {
            reader,
            frames,
            replies,
            writing,
            peer: _,
        } = *self;
        let sink = WsSink {
            frames,
            replies: replies.clone(),
            writing: Some(writing),
        };
        (Box::new(sink), Box::new(WsSource { reader, replies }))
    }

    fn peer(&self) -> String {
        self.peer.clone()
    }
}

struct WsSink {
    frames: mpsc::Sender<Bytes>,
    replies: mpsc::Sender<Reply>,
    /// 写任务归发的一侧：发的一侧丢掉时它随之中止。`close` 等它发完 close 帧，所以那时取走。
    writing: Option<OwnedTask<()>>,
}

#[async_trait]
impl FrameSink for WsSink {
    async fn send(&mut self, frame: Bytes) -> Result<(), LinkError> {
        self.frames.send(frame).await.map_err(|_| LinkError::Closed)
    }

    async fn close(&mut self) -> Result<(), LinkError> {
        // 写任务已经结束时连接本来就关了，这里只是没有 close 帧可发。发的一侧可以等：exec-client 收尾时有时限。
        let _ = self.replies.send(Reply::Close(CLOSE_NORMAL)).await;
        if let Some(writing) = self.writing.take() {
            let _ = writing.await;
        }
        Ok(())
    }
}

struct WsSource {
    reader: WsReader,
    replies: mpsc::Sender<Reply>,
}

#[async_trait]
impl FrameSource for WsSource {
    async fn recv(&mut self) -> Result<Option<Bytes>, LinkError> {
        match self.reader.next().await {
            Ok(WsFrame::Binary(frame)) => Ok(Some(frame)),
            Ok(WsFrame::Close { code, reason }) => {
                let _ = self.replies.try_send(Reply::Close(code));
                // 正常关闭是 `Ok(None)`（`FrameSource` 的约定）；别的关闭码带进错误：exec-client 把它记进断开的原因，
                // 中转的 4001（空闲）、4000（轮换）这样才看得出来。
                if code == CLOSE_NORMAL {
                    return Ok(None);
                }
                Err(LinkError::Io(if reason.is_empty() {
                    format!("websocket closed with code {code}")
                } else {
                    format!("websocket closed with code {code}: {reason}")
                }))
            }
            // 执行连接上只有二进制消息。
            Ok(WsFrame::Text(_)) => Err(LinkError::Io(
                "a text message arrived where a binary frame was expected".to_owned(),
            )),
            Err(WsError::TooBig(size)) => Err(LinkError::TooLarge {
                size: usize::try_from(size).unwrap_or(usize::MAX),
                max: crate::MAX_FRAME,
            }),
            Err(error) => Err(LinkError::Io(error.to_string())),
        }
    }
}

/// 写任务：回应优先，其次是数据帧。写失败就结束（连接坏了；读的一侧随后读到错误，发的一侧得到 `Closed`）。
async fn write(
    mut writer: WsWriter,
    mut frames: mpsc::Receiver<Bytes>,
    mut replies: mpsc::Receiver<Reply>,
) {
    loop {
        // 两个分支都是通道的 `recv`，被另一个分支抢先时不丢东西。
        let sent = tokio::select! {
            biased;
            reply = replies.recv() => match reply {
                Some(Reply::Close(code)) => {
                    writer.close(code).await;
                    return;
                }
                None => return,
            },
            frame = frames.recv() => match frame {
                Some(frame) => writer.send_binary(&frame).await,
                None => return,
            },
        };
        if let Err(error) = sent {
            tracing::debug!(target: "exec_transport", %error, "websocket write failed");
            return;
        }
    }
}

#[cfg(test)]
#[path = "ws_tests.rs"]
mod tests;
