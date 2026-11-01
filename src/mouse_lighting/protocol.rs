//! Volatile Logitech RGB protocol independent of the HID transport.
use crate::lighting::LightingMode;
use anyhow::{Context, Result, ensure};
const RGB_EFFECTS: u16 = 0x8071;
pub(super) trait Transport {
    fn request(&mut self, feature: u8, function: u8, parameters: &[u8]) -> Result<Vec<u8>>;
}

pub(super) struct Mouse<T> {
    pub(super) transport: T,
    feature: u8,
    static_effect: u8,
    pub(super) applied: Option<LightingMode>,
}

fn reply(bytes: Vec<u8>, minimum: usize) -> Result<Vec<u8>> {
    ensure!(bytes.len() >= minimum, "Malformed mouse lighting response");
    Ok(bytes)
}

impl<T: Transport> Mouse<T> {
    pub(super) fn discover(mut transport: T) -> Result<Self> {
        let feature = reply(transport.request(0, 0, &RGB_EFFECTS.to_be_bytes())?, 3)?;
        ensure!(
            feature[0] != 0 && feature[2] == 0,
            "Unsupported mouse RGB feature/version"
        );
        let feature = feature[0];
        let general = reply(transport.request(feature, 0, &[0xff, 0xff, 0])?, 7)?;
        ensure!(
            general[0] == 0xff && general[2] == 1,
            "Unsupported mouse lighting cluster count"
        );
        let cluster = reply(transport.request(feature, 0, &[0, 0xff, 0])?, 6)?;
        ensure!(
            cluster[0] == 0 && cluster[2..4] == [0, 2] && (1..=16).contains(&cluster[4]),
            "Unsupported mouse lighting cluster"
        );
        let mut static_effect = None;
        for index in 0..cluster[4] {
            let effect = reply(transport.request(feature, 0, &[0, index, 0])?, 4)?;
            ensure!(effect[..2] == [0, index], "Malformed mouse effect index");
            if effect[2..4] == [0, 1] {
                static_effect = Some(index);
                break;
            }
        }
        let mut mouse = Self {
            transport,
            feature,
            static_effect: static_effect.context("Mouse does not advertise static RGB lighting")?,
            applied: None,
        };
        mouse.power_mode()?;
        Ok(mouse)
    }

    pub(super) fn power_mode(&mut self) -> Result<u8> {
        let mode = reply(self.transport.request(self.feature, 0x80, &[0])?, 2)?;
        ensure!(
            mode[0] == 0 && (1..=3).contains(&mode[1]),
            "Unsupported mouse RGB power mode"
        );
        Ok(mode[1])
    }

    fn set_power_mode(&mut self, mode: u8) -> Result<()> {
        let result = reply(self.transport.request(self.feature, 0x80, &[1, mode])?, 1)?;
        ensure!(
            result[0] == 1 && self.power_mode()? == mode,
            "Mouse did not confirm RGB power mode"
        );
        Ok(())
    }

    pub(super) fn software_mode(&mut self) -> Result<u8> {
        let mode = reply(self.transport.request(self.feature, 0x50, &[0])?, 2)?;
        ensure!(
            mode[0] == 0 && mode[1] <= 3,
            "Malformed mouse LED control mode"
        );
        Ok(mode[1])
    }

    fn set_software_mode(&mut self, mode: u8) -> Result<()> {
        // G203's real LED pipeline requires the event-enable handshake as well.
        let parameters = [1, mode, if mode == 3 { 5 } else { 0 }];
        let result = reply(self.transport.request(self.feature, 0x50, &parameters)?, 3)?;
        ensure!(
            result[..3] == parameters,
            "Mouse did not acknowledge LED control mode"
        );
        Ok(())
    }

    pub(super) fn apply(&mut self, mode: LightingMode) -> Result<()> {
        if self.applied == Some(mode) {
            return Ok(());
        }
        self.applied = None;
        match mode {
            LightingMode::Off => {
                self.set_software_mode(3)?;
                // PowerOff read-back is not an optical guarantee on G203 firmware.
                // Paint a volatile static-black effect with the tested G203 layout.
                self.set_power_mode(1)?;
                let mut black = [0; 13];
                black[1] = self.static_effect;
                black[5] = 2;
                black[12] = 1; // Volatile RAM only, never EEPROM.
                self.transport.request(self.feature, 0x10, &black)?;
                ensure!(
                    self.software_mode()? == 3,
                    "Mouse did not retain software LED control"
                );
            }
            LightingMode::On => {
                // Return to the unchanged onboard lighting; no profile/DPI/flash writes.
                if self.software_mode()? != 0 {
                    self.set_power_mode(1)?;
                    self.set_software_mode(0)?;
                }
                ensure!(
                    self.software_mode()? == 0,
                    "Mouse did not restore device LED control"
                );
            }
        }
        self.applied = Some(mode);
        Ok(())
    }
}
