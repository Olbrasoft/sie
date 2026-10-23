//! Device-local macro isolation without grabbing or replaying normal typing.
use std::io;

use evdev::{Device, KeyCode};

use crate::{
    macros::MACRO1,
    openrazer::{PRODUCT_ID, VENDOR_ID},
};

type Entry = (u32, Vec<u8>);

pub(crate) trait Keymap {
    fn entries(&self) -> io::Result<Vec<Entry>>;
    fn write(&self, scancode: &[u8], keycode: u16) -> io::Result<()>;
}

impl Keymap for Device {
    fn entries(&self) -> io::Result<Vec<Entry>> {
        let id = self.input_id();
        if id.vendor() != VENDOR_ID || id.product() != PRODUCT_ID {
            return Err(io::Error::other("Unsupported keyboard keymap"));
        }
        let mut entries = Vec::new();
        for index in 0..=u16::MAX {
            match self.get_scancode_by_index(index) {
                Ok(entry) => entries.push(entry),
                Err(error) if error.kind() == io::ErrorKind::InvalidInput => break,
                Err(error) => return Err(error),
            }
        }
        Ok(entries)
    }

    fn write(&self, scancode: &[u8], keycode: u16) -> io::Result<()> {
        self.update_scancode(KeyCode::new(keycode), scancode)?;
        Ok(())
    }
}

pub(crate) struct MacroKeymap<T: Keymap = Device> {
    device: T,
    changed: Vec<Entry>,
}

impl<T: Keymap> MacroKeymap<T> {
    fn install_on(device: T) -> io::Result<Self> {
        let entries = device.entries()?;
        let mut guard = Self {
            device,
            changed: Vec::new(),
        };
        for (keycode, scancode) in entries {
            if (u32::from(KeyCode::KEY_F13.code())..=u32::from(KeyCode::KEY_F20.code()))
                .contains(&keycode)
            {
                let replacement = MACRO1 + (keycode as u16 - KeyCode::KEY_F13.code());
                // Record before writing so partial failures are rolled back as well.
                guard.changed.push((keycode, scancode.clone()));
                guard.device.write(&scancode, replacement)?;
            }
        }
        Ok(guard)
    }
}

impl MacroKeymap<Device> {
    pub fn install(device: Device) -> io::Result<Self> {
        Self::install_on(device)
    }
}

impl<T: Keymap> Drop for MacroKeymap<T> {
    fn drop(&mut self) {
        for (keycode, scancode) in self.changed.iter().rev() {
            if let Err(error) = self.device.write(scancode, *keycode as u16) {
                tracing::debug!(%error, "Could not restore a removed keyboard's macro mapping");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone)]
    struct FakeKeymap {
        entries: Arc<Mutex<Vec<Entry>>>,
        fail_code: Option<u16>,
    }
    impl Keymap for FakeKeymap {
        fn entries(&self) -> io::Result<Vec<Entry>> {
            Ok(self.entries.lock().unwrap().clone())
        }
        fn write(&self, scancode: &[u8], keycode: u16) -> io::Result<()> {
            if Some(keycode) == self.fail_code {
                return Err(io::Error::other("Injected write failure"));
            }
            let mut entries = self.entries.lock().unwrap();
            let entry = entries
                .iter_mut()
                .find(|(_, scan)| scan == scancode)
                .unwrap();
            entry.0 = u32::from(keycode);
            Ok(())
        }
    }

    fn fixture() -> FakeKeymap {
        let mut entries = vec![(30, 0x70004u32.to_ne_bytes().to_vec())];
        entries.extend((0..8).map(|i| (183 + i, (0x70068u32 + i).to_ne_bytes().to_vec())));
        FakeKeymap {
            entries: Arc::new(Mutex::new(entries)),
            fail_code: None,
        }
    }

    #[test]
    fn only_macro_keys_are_isolated_and_original_mappings_return_on_drop() {
        let keymap = fixture();
        let original = keymap.entries().unwrap();
        let guard = MacroKeymap::install_on(keymap.clone()).unwrap();
        let mapped = keymap.entries().unwrap();
        assert_eq!(mapped[0], original[0], "Normal typing must be untouched");
        assert_eq!(
            mapped
                .iter()
                .skip(1)
                .map(|entry| entry.0)
                .collect::<Vec<_>>(),
            [656, 657, 658, 659, 660, 661, 662, 663]
        );
        assert!(
            !mapped.iter().any(|entry| entry.0 == 183),
            "M1 must not reach GNOME as XF86Tools/F13"
        );
        drop(guard);
        assert_eq!(keymap.entries().unwrap(), original);
    }

    #[test]
    fn partial_installation_failure_restores_every_original_mapping() {
        let mut keymap = fixture();
        let original = keymap.entries().unwrap();
        keymap.fail_code = Some(658);
        assert!(MacroKeymap::install_on(keymap.clone()).is_err());
        assert_eq!(keymap.entries().unwrap(), original);
    }

    #[test]
    fn native_macro_codes_do_not_need_rewriting() {
        let keymap = FakeKeymap {
            entries: Arc::new(Mutex::new(vec![(656, vec![1, 2, 3, 4])])),
            fail_code: Some(656),
        };
        let guard = MacroKeymap::install_on(keymap.clone()).unwrap();
        assert!(guard.changed.is_empty());
        drop(guard);
        assert_eq!(keymap.entries().unwrap()[0].0, 656);
    }
}
