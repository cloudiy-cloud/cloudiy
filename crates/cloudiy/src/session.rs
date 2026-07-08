//! Interactive session pump: shuttle bytes between a QUIC session stream and
//! a real pseudo-terminal (`docker exec -it`). The PTY is a blocking API, so
//! its read/write happen on blocking tasks bridged to the async QUIC side by
//! channels. Resize frames drive `MasterPty::resize`, so vim/htop redraw at
//! the client's real terminal size.
//!
//! Lifetime rule that matters: the PTY **master** is held in this function
//! and dropped only after the shell exits. Dropping it early would close the
//! pseudo-terminal and SIGHUP whatever command is running (e.g. kill an
//! in-flight `apt-get`), so a client closing stdin must NOT tear the PTY down
//! — the shell ends on its own `exit`/Ctrl-D.

use anyhow::Result;
use cloudiy_common::proto::{self, SessionFrame};
use portable_pty::PtySize;
use std::io::{Read, Write};
use tokio::sync::mpsc;
use tracing::debug;

use crate::vm::PtySession;

pub async fn pump(
    mut send: iroh::endpoint::SendStream,
    mut recv: iroh::endpoint::RecvStream,
    pty: PtySession,
) -> Result<()> {
    let PtySession {
        master,
        mut reader,
        mut writer,
        mut child,
    } = pty;

    // PTY master → QUIC (Data frames), then the process exit code (Exit) last.
    let (out_tx, mut out_rx) = mpsc::channel::<SessionFrame>(256);
    let reader_task = tokio::task::spawn_blocking(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if out_tx
                        .blocking_send(SessionFrame::Data(buf[..n].to_vec()))
                        .is_err()
                    {
                        break;
                    }
                }
            }
        }
        // Reader EOF ⇒ the shell exited. Reap without ever blocking the Exit
        // frame (a hung wait() would strand the client): brief try_wait poll,
        // then report whatever we have.
        let mut code = None;
        for _ in 0..50 {
            match child.try_wait() {
                Ok(Some(status)) => {
                    code = Some(status.exit_code() as i32);
                    break;
                }
                Ok(None) => std::thread::sleep(std::time::Duration::from_millis(20)),
                Err(_) => break,
            }
        }
        let _ = out_tx.blocking_send(SessionFrame::Exit(code));
    });

    // QUIC stdin → PTY writer, on a blocking task holding ONLY the writer.
    // (Not the master — see the module note.)
    let (data_tx, mut data_rx) = mpsc::channel::<Vec<u8>>(256);
    let writer_task = tokio::task::spawn_blocking(move || {
        while let Some(d) = data_rx.blocking_recv() {
            if writer.write_all(&d).is_err() || writer.flush().is_err() {
                break;
            }
        }
        // `writer` drops here; the reader clone keeps the PTY open.
    });

    // Read session frames off the QUIC stream: data → writer, resize → master.
    let (resize_tx, mut resize_rx) = mpsc::channel::<(u16, u16)>(16);
    let feed_task = tokio::spawn(async move {
        loop {
            match proto::read_session_frame(&mut recv).await {
                Ok(Some(SessionFrame::Data(d))) => {
                    if data_tx.send(d).await.is_err() {
                        break;
                    }
                }
                Ok(Some(SessionFrame::Resize { cols, rows })) => {
                    let _ = resize_tx.send((cols, rows)).await;
                }
                // Client closed stdin: stop feeding, but leave the PTY up so a
                // running command finishes; the shell exits via `exit`/Ctrl-D.
                Ok(Some(SessionFrame::Eof)) | Ok(None) | Err(_) => break,
                Ok(Some(_)) => {}
            }
        }
    });

    // Main loop owns the master (for resize) and drains output to the client.
    loop {
        tokio::select! {
            frame = out_rx.recv() => {
                match frame {
                    Some(f) => {
                        let is_exit = matches!(f, SessionFrame::Exit(_));
                        if proto::write_session_frame(&mut send, &f).await.is_err() { break }
                        if is_exit { break }
                    }
                    None => break,
                }
            }
            Some((cols, rows)) = resize_rx.recv() => {
                let _ = master.resize(PtySize { rows, cols, pixel_width: 0, pixel_height: 0 });
            }
        }
    }

    feed_task.abort();
    writer_task.abort();
    let _ = reader_task.await;
    let _ = send.finish();
    // `master` drops here — after the shell has already exited.
    debug!("pty session ended");
    Ok(())
}
