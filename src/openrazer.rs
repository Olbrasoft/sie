use anyhow::{Context, Result, ensure};
use zbus::{Connection, Proxy};

use crate::lighting::{COLS, ROWS, encode_frame, workspace_frame};

pub const VENDOR_ID: u16 = 0x1532;
pub const PRODUCT_ID: u16 = 0x028D;

pub fn supported_device(ids: &[i32], dimensions: &[i32]) -> bool {
    ids == [i32::from(VENDOR_ID), i32::from(PRODUCT_ID)] && dimensions == [ROWS as i32, COLS as i32]
}

pub async fn devices(connection: &Connection) -> Result<Vec<String>> {
    let manager = Proxy::new(connection, "org.razer", "/org/razer", "razer.devices").await?;
    let serials: Vec<String> = manager.call("getDevices", &()).await?;
    let mut paths = Vec::new();
    for serial in serials {
        let path = format!("/org/razer/device/{serial}");
        let misc = Proxy::new(connection, "org.razer", path.as_str(), "razer.device.misc").await?;
        let ids: Vec<i32> = misc.call("getVidPid", &()).await?;
        // Ask for dimensions only on the known keyboard, not on other Razer devices.
        if ids != [i32::from(VENDOR_ID), i32::from(PRODUCT_ID)] {
            continue;
        }
        let dimensions: Vec<i32> = misc.call("getMatrixDimensions", &()).await?;
        ensure!(
            supported_device(&ids, &dimensions),
            "Unsupported keyboard matrix: {dimensions:?}"
        );
        paths.push(path);
    }
    Ok(paths)
}

pub async fn apply(connection: &Connection, active_workspace: Option<i32>) -> Result<usize> {
    let paths = devices(connection)
        .await
        .context("OpenRazer discovery failed")?;
    let payload = encode_frame(&workspace_frame(active_workspace));
    for path in &paths {
        let lighting = Proxy::new(
            connection,
            "org.razer",
            path.as_str(),
            "razer.device.lighting.chroma",
        )
        .await?;
        lighting.call::<_, _, ()>("setKeyRow", &(&payload,)).await?;
        lighting.call::<_, _, ()>("setCustom", &()).await?;
    }
    Ok(paths.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_other_models_and_unexpected_matrix_dimensions() {
        assert!(supported_device(&[0x1532, 0x028D], &[8, 23]));
        assert!(!supported_device(&[0x1532, 0x0293], &[8, 23]));
        assert!(!supported_device(&[0x1532, 0x028D], &[23, 8]));
        assert!(!supported_device(&[0x1532, 0x028D], &[6, 22]));
        assert!(!supported_device(&[], &[]));
    }
}
