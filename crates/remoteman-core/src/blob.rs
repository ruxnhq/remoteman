//! Blobs: clipboard contents and files, each on a one-way stream of its own, so that a large
//! one never holds up the control messages or the input (docs/PROTOCOL.md, "Blobs"). The same
//! code serves both roles.

use remoteman_proto::media::BLOB_HEADER_LIMIT;
use remoteman_transport::{Connection, RecvStream};

/// What happens to a blob the other side is sending, in order: opened, its content in pieces,
/// closed. `id` tells blobs in flight apart; it is this side's own number and means nothing to
/// the sender.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlobEvent {
    /// `kind` is `media::BLOB_CLIPBOARD` or `media::BLOB_FILE`; `header` is its JSON header
    /// (`media::ClipboardHeader`, `media::FileHeader`).
    Open { id: u32, kind: u8, header: String },
    Data { id: u32, bytes: Vec<u8> },
    /// `complete` false: the sender gave it up or the connection went; what came is useless.
    Close { id: u32, complete: bool },
}

impl BlobEvent {
    pub fn id(&self) -> u32 {
        match self {
            BlobEvent::Open { id, .. } | BlobEvent::Data { id, .. } | BlobEvent::Close { id, .. } => *id,
        }
    }
}

/// What a [`BlobSender`] puts on its way: a piece of content, or the end.
pub enum Chunk {
    Data(Vec<u8>),
    End,
}

/// A blob being sent. Write its content, then `finish`; dropping it without finishing gives the
/// blob up, and the receiver throws away what it has.
pub struct BlobSender {
    pub(crate) tx: tokio::sync::mpsc::Sender<Chunk>,
}

impl BlobSender {
    /// A sender and the end the content comes out of.
    pub(crate) fn pair() -> (Self, tokio::sync::mpsc::Receiver<Chunk>) {
        let (tx, rx) = tokio::sync::mpsc::channel(8);
        (Self { tx }, rx)
    }

    /// Send the next piece. Waits while the network is behind; false once the other side has
    /// gone. Not for calling from one of the core's own callbacks.
    pub fn write(&self, data: &[u8]) -> bool {
        self.tx.blocking_send(Chunk::Data(data.to_vec())).is_ok()
    }

    /// That was all of it.
    pub fn finish(self) -> bool {
        self.tx.blocking_send(Chunk::End).is_ok()
    }
}

/// Carry one blob out: open a stream, write the kind, the header and whatever `rx` yields.
pub(crate) async fn send(conn: Connection, kind: u8, header: String, mut rx: tokio::sync::mpsc::Receiver<Chunk>) {
    let Ok(mut stream) = conn.open_uni().await else { return };
    let mut head = Vec::with_capacity(5 + header.len());
    head.push(kind);
    head.extend_from_slice(&(header.len() as u32).to_le_bytes());
    head.extend_from_slice(header.as_bytes());
    if stream.write_all(&head).await.is_err() {
        return;
    }
    loop {
        match rx.recv().await {
            Some(Chunk::Data(bytes)) => {
                if stream.write_all(&bytes).await.is_err() {
                    return;
                }
            }
            Some(Chunk::End) => {
                let _ = stream.finish();
                // Wait for the receiver to have it all, so a connection closed right after
                // doesn't cut the end off.
                let _ = stream.stopped().await;
                return;
            }
            // Given up.
            None => {
                let _ = stream.reset(1u32.into());
                return;
            }
        }
    }
}

/// Take one blob in. `kind` is its first byte, already read. `sink` gets the events and returns
/// false to stop (the receiver has gone); it may wait, which holds the sender back.
pub(crate) async fn receive<F, Fut>(mut stream: RecvStream, kind: u8, id: u32, mut sink: F)
where
    F: FnMut(BlobEvent) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let mut len = [0u8; 4];
    if stream.read_exact(&mut len).await.is_err() {
        return;
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > BLOB_HEADER_LIMIT {
        let _ = stream.stop(2u32.into());
        return;
    }
    let mut header = vec![0u8; len];
    if stream.read_exact(&mut header).await.is_err() {
        return;
    }
    let Ok(header) = String::from_utf8(header) else {
        let _ = stream.stop(2u32.into());
        return;
    };
    if !sink(BlobEvent::Open { id, kind, header }).await {
        let _ = stream.stop(3u32.into());
        return;
    }
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        match stream.read(&mut chunk).await {
            Ok(Some(n)) => {
                if !sink(BlobEvent::Data { id, bytes: chunk[..n].to_vec() }).await {
                    let _ = stream.stop(3u32.into());
                    return;
                }
            }
            Ok(None) => {
                sink(BlobEvent::Close { id, complete: true }).await;
                return;
            }
            Err(_) => {
                sink(BlobEvent::Close { id, complete: false }).await;
                return;
            }
        }
    }
}
