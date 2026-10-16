//! Length-prefixed JSON frames, as native messaging uses them.

use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{Error, Result};

/// Largest frame accepted. Chrome itself sends at most 64 MiB to a host and accepts 1 MiB
/// back; a page's list of links stays well under this.
pub const MAX_FRAME: usize = 8 << 20;

/// One frame's bytes, or `None` at a clean end of input (the other side closed between
/// frames).
pub async fn read_frame<R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    match r.read_exact(&mut len).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(e) => return Err(e.into()),
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(Error::TooLarge(len));
    }
    let mut buf = vec![0u8; len];
    r.read_exact(&mut buf).await?;
    Ok(Some(buf))
}

pub async fn write_frame<W: AsyncWrite + Unpin>(w: &mut W, data: &[u8]) -> Result<()> {
    if data.len() > MAX_FRAME {
        return Err(Error::TooLarge(data.len()));
    }
    w.write_all(&(data.len() as u32).to_le_bytes()).await?;
    w.write_all(data).await?;
    w.flush().await?;
    Ok(())
}

pub async fn read_json<T: DeserializeOwned, R: AsyncRead + Unpin>(r: &mut R) -> Result<Option<T>> {
    match read_frame(r).await? {
        Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        None => Ok(None),
    }
}

pub async fn write_json<T: Serialize, W: AsyncWrite + Unpin>(w: &mut W, value: &T) -> Result<()> {
    write_frame(w, &serde_json::to_vec(value)?).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_and_limits() {
        let (mut a, mut b) = tokio::io::duplex(64);
        let writer = tokio::spawn(async move {
            write_json(&mut a, &serde_json::json!({ "type": "ping", "n": "ç" }))
                .await
                .expect("write");
            write_frame(&mut a, b"").await.expect("empty");
        });
        let v: serde_json::Value = read_json(&mut b).await.expect("read").expect("frame");
        assert_eq!(v["n"], "ç");
        assert_eq!(read_frame(&mut b).await.expect("read"), Some(vec![]));
        writer.await.expect("join");
        // The writer closed between frames: a clean end.
        assert_eq!(read_frame(&mut b).await.expect("eof"), None);

        // A length over the limit is refused before reading the body.
        let mut big = std::io::Cursor::new(((MAX_FRAME + 1) as u32).to_le_bytes().to_vec());
        assert!(matches!(
            read_frame(&mut big).await,
            Err(Error::TooLarge(_))
        ));
        // A frame cut short is an error, not an end.
        let mut cut = std::io::Cursor::new([5u8, 0, 0, 0, b'{'].to_vec());
        assert!(matches!(read_frame(&mut cut).await, Err(Error::Io(_))));
    }
}
