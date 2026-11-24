//! Noise_XK over TCP: authenticated, encrypted bidirectional frames.
//!
//! Framing: `[u32 LE length][AEAD ciphertext]` where ciphertext length is
//! `plaintext + 16` (ChaCha20-Poly1305 tag).

use noise_protocol::patterns::{HandshakePattern, Token};
use noise_protocol::{CipherState, HandshakeState, U8Array};
use noise_rust_crypto::sensitive::Sensitive;
use noise_rust_crypto::{ChaCha20Poly1305, Sha256, X25519};
use spidernotes_core::DeviceIdentity;
use std::marker::Unpin;
use std::net::SocketAddr;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

type HS = HandshakeState<X25519, ChaCha20Poly1305, Sha256>;
type Ciph = CipherState<ChaCha20Poly1305>;

const TAG_LEN: usize = 16;

pub fn xk_pattern() -> HandshakePattern {
    HandshakePattern::new(
        &[],
        &[],
        &[
            &[Token::E],
            &[Token::E, Token::EE, Token::S, Token::ES],
        ],
        "XK",
    )
}

/// An authenticated, encrypted frame stream. Generic over the byte source
/// so the same handshake/frames can run over a TCP socket OR a relay
/// session stream (server-mediated sync).
pub struct SecureStream<S> {
    stream: S,
    to: Ciph,
    from: Ciph,
}

impl SecureStream<TcpStream> {
    pub async fn connect(
        addr: SocketAddr,
        identity: &DeviceIdentity,
        remote_pub: [u8; 32],
    ) -> anyhow::Result<Self> {
        let stream = TcpStream::connect(addr).await?;
        Self::initiator(stream, identity, remote_pub).await
    }
}

impl<S> SecureStream<S>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    /// Run the initiator side of the handshake on an already-connected stream.
    pub async fn initiator(
        mut stream: S,
        identity: &DeviceIdentity,
        remote_pub: [u8; 32],
    ) -> anyhow::Result<Self> {
        let static_key: Sensitive<[u8; 32]> =
            Sensitive::from_slice(&identity.x25519_seed_bytes());
        let mut hs: HS = HandshakeState::new(
            xk_pattern(),
            true,
            b"",
            Some(static_key),
            None,
            Some(remote_pub),
            None,
        );

        // msg1: initiator sends E
        let msg1 = hs.write_message_vec(b"")?;
        stream.write_all(&msg1).await?;
        // msg2: E, EE, S, ES
        let overhead = hs.get_next_message_overhead();
        let mut buf = vec![0u8; overhead];
        stream.read_exact(&mut buf).await?;
        let payload = hs.read_message_vec(&buf)?;
        debug_assert!(payload.is_empty());

        // noise-protocol does not verify the expected peer static key itself
        let actual = hs.get_rs().ok_or_else(|| anyhow::anyhow!("no peer static key"))?;
        if actual != remote_pub {
            anyhow::bail!("peer static key mismatch (wrong device?)");
        }

        let (to, from) = hs.get_ciphers();
        Ok(Self { stream, to, from })
    }

    pub async fn accept(mut stream: S, identity: &DeviceIdentity) -> anyhow::Result<Self> {
        let static_key: Sensitive<[u8; 32]> =
            Sensitive::from_slice(&identity.x25519_seed_bytes());
        let mut hs: HS = HandshakeState::new(
            xk_pattern(),
            false,
            b"",
            Some(static_key),
            None,
            None,
            None,
        );

        // msg1
        let overhead = hs.get_next_message_overhead();
        let mut buf = vec![0u8; overhead];
        stream.read_exact(&mut buf).await?;
        let payload = hs.read_message_vec(&buf)?;
        debug_assert!(payload.is_empty());
        // msg2
        let msg2 = hs.write_message_vec(b"")?;
        stream.write_all(&msg2).await?;

        // get_ciphers() -> (initiator->responder, responder->initiator)
        let (i2r, r2i) = hs.get_ciphers();
        Ok(Self {
            stream,
            to: r2i,
            from: i2r,
        })
    }

    /// Send one plaintext frame.
    pub async fn send_frame(&mut self, plaintext: &[u8]) -> anyhow::Result<()> {
        let ct = self.to.encrypt_vec(plaintext);
        let len = (ct.len() as u32).to_le_bytes();
        self.stream.write_all(&len).await?;
        self.stream.write_all(&ct).await?;
        Ok(())
    }

    /// Receive one plaintext frame.
    pub async fn recv_frame(&mut self) -> anyhow::Result<Vec<u8>> {
        let mut len_buf = [0u8; 4];
        self.stream.read_exact(&mut len_buf).await?;
        let len = u32::from_le_bytes(len_buf) as usize;
        if len > 64 * 1024 * 1024 || len < TAG_LEN {
            anyhow::bail!("frame length {len} out of bounds");
        }
        let mut ct = vec![0u8; len];
        self.stream.read_exact(&mut ct).await?;
        let pt = self.from.decrypt_vec(&ct).map_err(|e| anyhow::anyhow!("decrypt failed: {e:?}"))?;
        Ok(pt)
    }

    /// Close the write half (works for TCP and relay streams alike).
    pub async fn shutdown(mut self) -> std::io::Result<()> {
        self.stream.shutdown().await
    }
}

pub struct SyncListener {
    listener: TcpListener,
}

impl SyncListener {
    pub async fn bind(port: u16) -> anyhow::Result<Self> {
        let listener = TcpListener::bind(("0.0.0.0", port)).await?;
        Ok(Self { listener })
    }

    pub async fn bind_ephemeral() -> anyhow::Result<Self> {
        Self::bind(0).await
    }

    pub fn local_addr(&self) -> anyhow::Result<SocketAddr> {
        Ok(self.listener.local_addr()?)
    }

    pub fn port(&self) -> u16 {
        self.local_addr().map(|a| a.port()).unwrap_or(0)
    }

    pub async fn accept(&self) -> anyhow::Result<TcpStream> {
        let (stream, _) = self.listener.accept().await?;
        Ok(stream)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn handshake_and_frames_roundtrip() {
        let a = DeviceIdentity::generate("a").unwrap();
        let b = DeviceIdentity::generate("b").unwrap();
        let pub_b = b.x25519_public;

        let listener = SyncListener::bind_ephemeral().await.unwrap();
        let port = listener.port();

        let server = tokio::spawn(async move {
            let stream = listener.accept().await.unwrap();
            SecureStream::accept(stream, &b).await.unwrap()
        });

        let mut client = SecureStream::connect(
            SocketAddr::from(([127, 0, 0, 1], port)),
            &a,
            pub_b,
        )
        .await
        .unwrap();
        let mut server = server.await.unwrap();

        client.send_frame(b"hello peer").await.unwrap();
        let got = server.recv_frame().await.unwrap();
        assert_eq!(got, b"hello peer");

        server.send_frame(b"hello back").await.unwrap();
        let got = client.recv_frame().await.unwrap();
        assert_eq!(got, b"hello back");

        // larger frame
        let big = vec![0xABu8; 100_000];
        client.send_frame(&big).await.unwrap();
        let got = server.recv_frame().await.unwrap();
        assert_eq!(got, big);
    }

    #[tokio::test]
    async fn wrong_pubkey_fails_handshake() {
        let a = DeviceIdentity::generate("a").unwrap();
        let b = DeviceIdentity::generate("b").unwrap();
        let wrong = DeviceIdentity::generate("evil").unwrap();

        let listener = SyncListener::bind_ephemeral().await.unwrap();
        let port = listener.port();
        let server = tokio::spawn(async move {
            let stream = listener.accept().await.unwrap();
            SecureStream::accept(stream, &b).await
        });

        let res = SecureStream::connect(
            SocketAddr::from(([127, 0, 0, 1], port)),
            &a,
            wrong.x25519_public,
        )
        .await;
        let _ = server.await;
        assert!(res.is_err());
    }
}
