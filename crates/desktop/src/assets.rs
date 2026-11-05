//! Bundle the device icons in addition to GPUI's default component icons.
use gpui_kit::{AssetSource, SharedString};
use std::borrow::Cow;

gpui_kit::assets::icon_assets!(DeviceIcons, [Globe, Laptop, Monitor, Server]);

pub(crate) struct AppAssets;
impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = DeviceIcons.load(path)? {
            return Ok(Some(bytes));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> anyhow::Result<Vec<SharedString>> {
        let mut paths = gpui_kit::assets::Assets.list(path)?;
        paths.extend(DeviceIcons.list(path)?);
        paths.sort();
        paths.dedup();
        Ok(paths)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_and_standard_icons_are_bundled() {
        for icon in ["globe", "laptop", "monitor", "server", "check"] {
            let bytes = AppAssets
                .load(&format!("icons/{icon}.svg"))
                .unwrap()
                .unwrap();
            assert!(bytes.starts_with(b"<svg"));
        }
    }
}
