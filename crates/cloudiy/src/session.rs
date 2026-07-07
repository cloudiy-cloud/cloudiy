//! Interactive session pump: shuttle bytes between a QUIC session stream and
//! a spawned `docker exec` shell. Ordering guarantee — the terminal's final
//! output is flushed before the `Exit` frame, so the client never sees the
//! prompt return before the last line of output.

use anyhow::Result;
use cloudiy_common::proto::{self, SessionFrame};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;
use tracing::debug;

/// Drive one interactive session to completion. Consumes the QUIC streams and
/// the child process; returns when the shell exits or the peer disconnects.
pub async fn pump(
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    mut child: tokio::process::Child,
) -> Result<()> {
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");

    // Outbound (container → client) frames funnel through one writer so
    // stdout and stderr never interleave a half-written frame.
    let (tx, mut rx) = mpsc::channel::<SessionFrame>(256);
    let out_handle = spawn_reader(stdout, tx.clone());
    let err_handle = spawn_reader(stderr, tx.clone());

    // Exit watcher: wait for the process, then for both pipes to drain, then
    // enqueue Exit last — guaranteeing it trails all Data frames.
    let exit_tx = tx.clone();
    tokio::spawn(async move {
        let code = child.wait().await.ok().and_then(|s| s.code());
        let _ = out_handle.await;
        let _ = err_handle.await;
        let _ = exit_tx.send(SessionFrame::Exit(code)).await;
    });
    drop(tx); // channel closes once readers + exit watcher are done

    // Inbound (client → container): stdin + control frames.
    let stdin_task = tokio::spawn(async move {
        loop {
            match proto::read_session_frame(&mut recv).await {
                Ok(Some(SessionFrame::Data(d))) => {
                    if stdin.write_all(&d).await.is_err() || stdin.flush().await.is_err() {
                        break;
                    }
                }
                Ok(Some(SessionFrame::Eof)) => break, // dropping stdin sends EOF
                Ok(Some(_)) => {} // Resize: no PTY yet, ignore
                Ok(None) | Err(_) => break, // peer closed
            }
        }
        drop(stdin);
    });

    // Writer: drain outbound frames until Exit, then stop.
    while let Some(frame) = rx.recv().await {
        let is_exit = matches!(frame, SessionFrame::Exit(_));
        if proto::write_session_frame(&mut send, &frame).await.is_err() {
            break;
        }
        if is_exit {
            break;
        }
    }
    stdin_task.abort();
    let _ = send.finish();
    debug!("session ended");
    Ok(())
}

fn spawn_reader<R>(mut r: R, tx: mpsc::Sender<SessionFrame>) -> tokio::task::JoinHandle<()>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buf = [0u8; 8192];
        loop {
            match r.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(SessionFrame::Data(buf[..n].to_vec())).await.is_err() {
                        break;
                    }
                }
            }
        }
    })
}
