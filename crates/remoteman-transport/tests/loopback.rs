//! A host and a client on the loopback: trust, a control message each way, and datagrams.

use std::sync::Arc;

use remoteman_proto::control::Message;
use remoteman_transport::{Expect, Identity, Trust, connect, listen, peer_fingerprint, read_message, write_message};

#[tokio::test(flavor = "multi_thread")]
async fn paired_devices_talk_and_strangers_are_refused() {
    let host_id = Identity::generate().unwrap();
    let client_id = Identity::generate().unwrap();
    let trust = Arc::new(Trust::in_memory());
    trust.add(&client_id.fingerprint, "test client").unwrap();
    let endpoint = listen("127.0.0.1:0".parse().unwrap(), &host_id, trust.clone()).unwrap();
    let addr = endpoint.local_addr().unwrap();

    let host_fp = host_id.fingerprint.clone();
    let client_fp = client_id.fingerprint.clone();
    let server = tokio::spawn(async move {
        let conn = endpoint.accept().await.unwrap().await.unwrap();
        assert_eq!(peer_fingerprint(&conn).unwrap(), client_fp);
        let (mut send, mut recv) = conn.accept_bi().await.unwrap();
        assert_eq!(read_message(&mut recv).await.unwrap(), Some(Message::Keyframe));
        write_message(&mut send, &Message::Pong { id: 1, client_us: 2, host_us: 3 }).await.unwrap();
        conn.send_datagram(vec![7u8; 1000].into()).unwrap();
        // Hold the connection until the client has read everything, then let the stranger try.
        conn.closed().await;
        endpoint
    });

    let (_ep, conn, fp) = connect(addr, &client_id, Expect::Fingerprint(host_fp.clone())).await.unwrap();
    assert_eq!(fp, host_fp);
    let (mut send, mut recv) = conn.open_bi().await.unwrap();
    write_message(&mut send, &Message::Keyframe).await.unwrap();
    assert_eq!(read_message(&mut recv).await.unwrap(), Some(Message::Pong { id: 1, client_us: 2, host_us: 3 }));
    assert_eq!(conn.read_datagram().await.unwrap().len(), 1000);
    assert!(conn.max_datagram_size().unwrap() >= 1100);
    conn.close(0u32.into(), b"done");
    let endpoint = server.await.unwrap();

    // A client expecting another host refuses this one.
    let wrong = connect(addr, &client_id, Expect::Fingerprint("0".repeat(64))).await;
    assert!(wrong.is_err());

    // A client the host doesn't know is refused: the connection dies before any stream opens.
    let stranger = Identity::generate().unwrap();
    let accept = tokio::spawn(async move {
        let incoming = endpoint.accept().await.unwrap();
        incoming.await.is_err()
    });
    let refused = match connect(addr, &stranger, Expect::Anyone).await {
        Err(_) => true,
        Ok((_ep, conn, _)) => conn.open_bi().await.is_err() || {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            conn.close_reason().is_some()
        },
    };
    assert!(refused, "a stranger got in");
    assert!(accept.await.unwrap(), "the host accepted a stranger");
}

#[test]
fn identity_and_trust_persist() {
    let dir = tempfile::tempdir().unwrap();
    let a = Identity::load_or_create(dir.path()).unwrap();
    let b = Identity::load_or_create(dir.path()).unwrap();
    assert_eq!(a.fingerprint, b.fingerprint);
    assert_eq!(a.fingerprint.len(), 64);
    let path = dir.path().join("trusted");
    Trust::load(&path).add(&a.fingerprint, "Anna's iPad").unwrap();
    assert!(Trust::load(&path).contains(&a.fingerprint));
    assert!(!Trust::load(&path).contains(&"0".repeat(64)));
}
