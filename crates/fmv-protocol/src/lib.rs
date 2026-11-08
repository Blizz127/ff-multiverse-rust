//! FF Multiverse wire protocol v1.
//!
//! Framing: `u32` little-endian payload length, then the payload. M1 payload codec is JSON
//! (serde_json; postcard is not in the local crate cache). The codec is isolated in
//! [`encode`]/[`decode`] so it can be swapped for a binary codec with a version bump.
//!
//! Invariant: no message carries game asset bytes. Areas are agreed via [`AreaId`] +
//! [`Fingerprint`] + path-free [`ProvenanceSummary`]; each peer reads its own owned copy.

use fmv_core::{
    AnimState, AreaId, EntityId, EntitySnapshot, Fingerprint, Pos, ProvenanceSummary, SessionId,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const PROTOCOL_VERSION: u16 = 1;
/// Hard cap on a single frame payload (bytes).
pub const MAX_FRAME: u32 = 256 * 1024;
/// Default TCP port for development. Deployment port is UNASSIGNED (see DEPLOYMENT-CONTRACT.md).
pub const DEFAULT_DEV_PORT: u16 = 47_770;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ClientMsg {
    Hello {
        protocol_version: u16,
        client_build: String,
        display_name: String,
        tint: [u8; 3],
        appearance_ref: Option<String>,
    },
    /// Sent after the client confirmed it holds the area with this exact fingerprint locally.
    JoinArea { area: AreaId, fingerprint: Fingerprint },
    /// Client cannot join: it lacks the area or has an incompatible edition.
    CannotJoin { area: AreaId, reason: String },
    MoveIntent { seq: u32, pos: Pos, facing: f32, anim: AnimState },
    Ping { nonce: u64 },
    Leave,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ServerMsg {
    Welcome {
        protocol_version: u16,
        server_build: String,
        session: SessionId,
        entity: EntityId,
    },
    Reject { reason: String },
    /// Offer of the area the server is hosting. `fixture == true` must be shown to the user.
    AreaManifest {
        area: AreaId,
        display_name: String,
        fingerprint: Fingerprint,
        provenance: ProvenanceSummary,
        fixture: bool,
    },
    AreaSnapshot { area: AreaId, tick: u64, entities: Vec<EntitySnapshot> },
    EntityUpdate { tick: u64, entities: Vec<EntitySnapshot> },
    MoveRejected { seq: u32, corrected: Pos },
    PlayerJoined { snapshot: EntitySnapshot },
    PlayerLeft { entity: EntityId },
    Pong { nonce: u64, tick: u64 },
}

#[derive(Debug, thiserror::Error)]
pub enum ProtoError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("frame too large: {0} bytes")]
    FrameTooLarge(u32),
    #[error("codec: {0}")]
    Codec(#[from] serde_json::Error),
    #[error("connection closed")]
    Closed,
}

pub fn encode<T: Serialize>(msg: &T) -> Result<Vec<u8>, ProtoError> {
    let payload = serde_json::to_vec(msg)?;
    let len = u32::try_from(payload.len()).map_err(|_| ProtoError::FrameTooLarge(u32::MAX))?;
    if len > MAX_FRAME {
        return Err(ProtoError::FrameTooLarge(len));
    }
    let mut out = Vec::with_capacity(4 + payload.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

pub fn decode<T: DeserializeOwned>(payload: &[u8]) -> Result<T, ProtoError> {
    Ok(serde_json::from_slice(payload)?)
}

pub async fn write_msg<W: AsyncWrite + Unpin, T: Serialize>(w: &mut W, msg: &T) -> Result<(), ProtoError> {
    let buf = encode(msg)?;
    w.write_all(&buf).await?;
    w.flush().await?;
    Ok(())
}

/// Read one framed message. Returns `Err(ProtoError::Closed)` on clean EOF at a frame boundary.
pub async fn read_msg<R: AsyncRead + Unpin, T: DeserializeOwned>(r: &mut R) -> Result<T, ProtoError> {
    let mut len_buf = [0u8; 4];
    match r.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Err(ProtoError::Closed),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len_buf);
    if len > MAX_FRAME {
        return Err(ProtoError::FrameTooLarge(len));
    }
    let mut payload = vec![0u8; len as usize];
    r.read_exact(&mut payload).await?;
    decode(&payload)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn roundtrip_over_duplex() {
        let (mut a, mut b) = tokio::io::duplex(4096);
        let m = ClientMsg::MoveIntent { seq: 3, pos: Pos::new(1.0, 2.0, 0.0), facing: 0.5, anim: AnimState::Walk };
        write_msg(&mut a, &m).await.unwrap();
        let got: ClientMsg = read_msg(&mut b).await.unwrap();
        assert_eq!(got, m);
        drop(a);
        assert!(matches!(read_msg::<_, ClientMsg>(&mut b).await, Err(ProtoError::Closed)));
    }

    #[tokio::test]
    async fn rejects_oversized_frame() {
        let (mut a, mut b) = tokio::io::duplex(64);
        a.write_all(&(MAX_FRAME + 1).to_le_bytes()).await.unwrap();
        assert!(matches!(read_msg::<_, ClientMsg>(&mut b).await, Err(ProtoError::FrameTooLarge(_))));
    }
}
