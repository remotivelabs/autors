//! Feature-gated native bus-adapter registry.

use autors_can::device::CanDevice;
use autors_lin::device::LinDevice;

/// A CAN/LIN adapter family compiled into this CLI build.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    Virtual,
    #[cfg(feature = "hardware-vector")]
    Vector,
    #[cfg(feature = "hardware-kvaser")]
    Kvaser,
    #[cfg(feature = "hardware-peak")]
    Peak,
}

impl AdapterKind {
    pub const ALL: &'static [Self] = &[
        Self::Virtual,
        #[cfg(feature = "hardware-vector")]
        Self::Vector,
        #[cfg(feature = "hardware-kvaser")]
        Self::Kvaser,
        #[cfg(feature = "hardware-peak")]
        Self::Peak,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Virtual => "Virtual",
            #[cfg(feature = "hardware-vector")]
            Self::Vector => "Vector",
            #[cfg(feature = "hardware-kvaser")]
            Self::Kvaser => "Kvaser",
            #[cfg(feature = "hardware-peak")]
            Self::Peak => "PEAK",
        }
    }

    pub const fn driver(self) -> &'static str {
        match self {
            Self::Virtual => "in-process",
            #[cfg(feature = "hardware-vector")]
            Self::Vector => "vxlapi64.dll",
            #[cfg(feature = "hardware-kvaser")]
            Self::Kvaser => "canlib32.dll / linlib.dll",
            #[cfg(feature = "hardware-peak")]
            Self::Peak => "PCANBasic.dll / PLinApi.dll",
        }
    }

    pub const fn default_can_hardware_type(self) -> i32 {
        match self {
            #[cfg(feature = "hardware-peak")]
            Self::Peak => 0x51, // PCAN_USBBUS1 base handle
            _ => 0,
        }
    }

    pub const fn default_lin_hardware_type(self) -> i32 {
        match self {
            #[cfg(feature = "hardware-peak")]
            Self::Peak => 3, // PLIN_USB
            _ => 0,
        }
    }

    pub fn next(self) -> Self {
        let index = Self::ALL
            .iter()
            .position(|candidate| *candidate == self)
            .unwrap_or_default();
        Self::ALL[(index + 1) % Self::ALL.len()]
    }
}

pub fn create_can(kind: AdapterKind) -> Result<Box<dyn CanDevice + Send + Sync>, String> {
    match kind {
        AdapterKind::Virtual => Err("the virtual CAN adapter is managed internally".to_owned()),
        #[cfg(all(windows, feature = "hardware-vector"))]
        AdapterKind::Vector => autors_can::vendors::vector::VectorCan::new()
            .map(|device| Box::new(device) as Box<dyn CanDevice + Send + Sync>)
            .map_err(|error| error.to_string()),
        #[cfg(all(not(windows), feature = "hardware-vector"))]
        AdapterKind::Vector => Err("Vector hardware support requires Windows".to_owned()),
        #[cfg(all(windows, feature = "hardware-kvaser"))]
        AdapterKind::Kvaser => autors_can::vendors::kvaser::KvaserCan::new()
            .map(|device| Box::new(device) as Box<dyn CanDevice + Send + Sync>)
            .map_err(|error| error.to_string()),
        #[cfg(all(not(windows), feature = "hardware-kvaser"))]
        AdapterKind::Kvaser => Err("Kvaser hardware support requires Windows".to_owned()),
        #[cfg(all(windows, feature = "hardware-peak"))]
        AdapterKind::Peak => autors_can::vendors::peak::PeakCan::new()
            .map(|device| Box::new(device) as Box<dyn CanDevice + Send + Sync>)
            .map_err(|error| error.to_string()),
        #[cfg(all(not(windows), feature = "hardware-peak"))]
        AdapterKind::Peak => Err("PEAK hardware support requires Windows".to_owned()),
    }
}

pub fn create_lin(kind: AdapterKind) -> Result<Box<dyn LinDevice + Send>, String> {
    match kind {
        AdapterKind::Virtual => Err("the virtual LIN adapter is managed internally".to_owned()),
        #[cfg(all(windows, feature = "hardware-vector"))]
        AdapterKind::Vector => autors_lin::vendors::vector::VectorLin::new()
            .map(|device| Box::new(device) as Box<dyn LinDevice + Send>)
            .map_err(|error| error.to_string()),
        #[cfg(all(not(windows), feature = "hardware-vector"))]
        AdapterKind::Vector => Err("Vector hardware support requires Windows".to_owned()),
        #[cfg(all(windows, feature = "hardware-kvaser"))]
        AdapterKind::Kvaser => autors_lin::vendors::kvaser::KvaserLin::new()
            .map(|device| Box::new(device) as Box<dyn LinDevice + Send>)
            .map_err(|error| error.to_string()),
        #[cfg(all(not(windows), feature = "hardware-kvaser"))]
        AdapterKind::Kvaser => Err("Kvaser hardware support requires Windows".to_owned()),
        #[cfg(all(windows, feature = "hardware-peak"))]
        AdapterKind::Peak => autors_lin::vendors::peak::PeakLin::new()
            .map(|device| Box::new(device) as Box<dyn LinDevice + Send>)
            .map_err(|error| error.to_string()),
        #[cfg(all(not(windows), feature = "hardware-peak"))]
        AdapterKind::Peak => Err("PEAK hardware support requires Windows".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn virtual_adapter_is_always_first() {
        assert_eq!(AdapterKind::ALL[0], AdapterKind::Virtual);
        assert_eq!(AdapterKind::Virtual.name(), "Virtual");
        assert_eq!(
            AdapterKind::Virtual.next(),
            AdapterKind::ALL[1 % AdapterKind::ALL.len()]
        );
    }

    #[test]
    fn hardware_defaults_are_stable() {
        assert_eq!(AdapterKind::Virtual.default_can_hardware_type(), 0);
        assert_eq!(AdapterKind::Virtual.default_lin_hardware_type(), 0);
    }
}
