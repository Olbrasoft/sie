//! Size-bounded native-messaging framing, independent of browser and desktop I/O.
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
pub(super) const MAX_FRAME: usize = 65536;

pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Option<Value>> {
    let mut size = [0; 4];
    if reader.read(&mut size[..1]).await? == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut size[1..])
        .await
        .context("Incomplete native frame")?;
    let size = u32::from_ne_bytes(size) as usize;
    ensure!(size > 0 && size <= MAX_FRAME, "Invalid native frame size");
    let mut bytes = vec![0; size];
    reader.read_exact(&mut bytes).await?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}
pub async fn write_frame<W: AsyncWrite + Unpin>(writer: &mut W, value: &Value) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    ensure!(bytes.len() <= MAX_FRAME, "Native frame too large");
    writer
        .write_all(&(bytes.len() as u32).to_ne_bytes())
        .await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[tokio::test]
    async fn native_frames_preserve_unicode_and_reject_incomplete_or_oversized_data() {
        let value = json!({"title":"Příliš žluťoučký kůň"});
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &value).await.unwrap();
        let mut reader = bytes.as_slice();
        assert_eq!(read_frame(&mut reader).await.unwrap(), Some(value));
        assert!(read_frame(&mut reader).await.unwrap().is_none());
        let mut partial = &[1u8, 0][..];
        assert!(read_frame(&mut partial).await.is_err());
        let size = (MAX_FRAME as u32 + 1).to_ne_bytes();
        let mut oversized = size.as_slice();
        assert!(read_frame(&mut oversized).await.is_err());
    }
}
