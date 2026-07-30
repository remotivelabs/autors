use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

use crate::error::{Error, Result};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VehicleFlashConfig {
    #[serde(rename = "Vehicle")]
    pub vehicle: VehicleSection,
    #[serde(rename = "Flow")]
    pub flow: PluginSection,
    #[serde(rename = "Can")]
    pub can: CanSection,
    #[serde(rename = "Loader")]
    pub loader: LoaderSection,
    #[serde(rename = "SeedKey")]
    pub seed_key: SeedKeySection,
    #[serde(rename = "FlashDriver")]
    pub flash_driver: Option<FlashDriverSection>,
    #[serde(skip)]
    pub source_path: PathBuf,
    #[serde(skip)]
    pub source_dir: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "PascalCase")]
pub struct VehicleSection {
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub demo_only: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "PascalCase")]
pub struct PluginSection {
    pub dll_path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "PascalCase")]
pub struct SeedKeySection {
    pub dll_path: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "PascalCase")]
pub struct LoaderSection {
    pub dll_path: PathBuf,
    #[serde(default)]
    pub mode: LoaderMode,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LoaderMode {
    #[default]
    Separate,
    Package,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "PascalCase")]
pub struct FlashDriverSection {
    pub path: PathBuf,
    #[serde(default)]
    pub address: u32,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "PascalCase")]
pub struct CanSection {
    pub baud_rate: u32,
    #[serde(default)]
    pub data_baud_rate: u32,
    pub functional_request_id: u32,
    pub physical_request_id: u32,
    pub response_id: u32,
    #[serde(default = "default_fill_byte")]
    pub fill_byte: u8,
    #[serde(default = "default_p2")]
    pub p2_client_ms: u32,
    #[serde(default = "default_p3")]
    pub p3_client_ms: u32,
    #[serde(default = "default_transfer_block_size")]
    pub transfer_block_size: usize,
}

impl VehicleFlashConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let mut config: Self = toml::from_str(&text)
            .map_err(|error| Error::Config(format!("{}: {error}", path.display())))?;
        config.source_path = path.to_path_buf();
        config.source_dir = path
            .parent()
            .ok_or_else(|| Error::Config("configuration has no parent directory".to_string()))?
            .to_path_buf();
        config.validate()?;
        Ok(config)
    }

    pub fn discover(root: &Path) -> (Vec<Self>, Vec<String>) {
        let mut configs = Vec::new();
        let mut errors = Vec::new();
        let entries = match std::fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) => {
                errors.push(format!("{}: {error}", root.display()));
                return (configs, errors);
            }
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|value| value.to_str()) else {
                continue;
            };
            let config_path = path.join(format!("{name}.toml"));
            if !config_path.is_file() {
                continue;
            }
            match Self::load(&config_path) {
                Ok(config) => configs.push(config),
                Err(error) => errors.push(error.to_string()),
            }
        }
        configs.sort_by(|left, right| left.vehicle.name.cmp(&right.vehicle.name));
        (configs, errors)
    }

    pub fn resolve(&self, relative: &Path) -> PathBuf {
        self.source_dir.join(relative)
    }

    pub fn flash_driver_path(&self) -> Option<PathBuf> {
        self.flash_driver
            .as_ref()
            .map(|driver| self.resolve(&driver.path))
    }

    pub fn summary(&self) -> String {
        let bus = if self.can.data_baud_rate == 0 {
            "Classic CAN".to_string()
        } else {
            format!("CAN FD / {} bit/s data", self.can.data_baud_rate)
        };
        format!(
            "{} bit/s {}  |  request 0x{:X}  |  response 0x{:X}",
            self.can.baud_rate, bus, self.can.physical_request_id, self.can.response_id
        )
    }

    fn validate(&self) -> Result<()> {
        if self.vehicle.name.trim().is_empty() || self.vehicle.description.trim().is_empty() {
            return Err(Error::Config(
                "Vehicle.Name and Vehicle.Description must not be empty".to_string(),
            ));
        }
        for (label, path) in [
            ("Flow.DllPath", &self.flow.dll_path),
            ("Loader.DllPath", &self.loader.dll_path),
            ("SeedKey.DllPath", &self.seed_key.dll_path),
        ] {
            validate_relative_path(label, path)?;
        }
        match (self.loader.mode, &self.flash_driver) {
            (LoaderMode::Separate, None) => {
                return Err(Error::Config(
                    "separate loader mode requires a [FlashDriver] section".to_string(),
                ));
            }
            (_, Some(driver)) => validate_relative_path("FlashDriver.Path", &driver.path)?,
            _ => {}
        }
        for (label, id) in [
            ("FunctionalRequestId", self.can.functional_request_id),
            ("PhysicalRequestId", self.can.physical_request_id),
            ("ResponseId", self.can.response_id),
        ] {
            if id > 0x1FFF_FFFF {
                return Err(Error::Config(format!(
                    "Can.{label} is outside the 29-bit CAN ID range"
                )));
            }
        }
        if autors_can::frame::CanBaudrate::from_u32(self.can.baud_rate).is_none() {
            return Err(Error::Config(format!(
                "unsupported Can.BaudRate {}",
                self.can.baud_rate
            )));
        }
        if autors_can::frame::CanFdBaudrate::from_u32(self.can.data_baud_rate).is_none() {
            return Err(Error::Config(format!(
                "unsupported Can.DataBaudRate {}",
                self.can.data_baud_rate
            )));
        }
        if !(8..=4093).contains(&self.can.transfer_block_size) {
            return Err(Error::Config(
                "Can.TransferBlockSize must be between 8 and 4093".to_string(),
            ));
        }
        Ok(())
    }
}

fn validate_relative_path(label: &str, path: &Path) -> Result<()> {
    if path.as_os_str().is_empty() || path.is_absolute() {
        return Err(Error::Config(format!("{label} must be a relative path")));
    }
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return Err(Error::Config(format!(
            "{label} must stay inside its configuration directory"
        )));
    }
    Ok(())
}

const fn default_fill_byte() -> u8 {
    0xFF
}

const fn default_p2() -> u32 {
    1000
}

const fn default_p3() -> u32 {
    30_000
}

const fn default_transfer_block_size() -> usize {
    1024
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_configuration_is_discovered_through_the_generic_scanner() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("Conf");
        let (configs, errors) = VehicleFlashConfig::discover(&root);
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].vehicle.name, "ExampleCar");
    }

    #[test]
    fn paths_cannot_escape_the_configuration_directory() {
        let error = validate_relative_path("test", Path::new("../private.dll")).unwrap_err();
        assert!(error.to_string().contains("must stay inside"));
    }
}
