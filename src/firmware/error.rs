//! Unified error type for the firmware.
//!
//! This module provides a single FirmwareError enum that consolidates all error
//! types throughout the codebase, making error handling consistent and composable.

use esp_storage::FlashStorageError;

/// Comprehensive firmware error type.
///
/// This enum unifies errors from all subsystems: sensors, persistent storage,
/// and network operations.
#[derive(Clone, Copy, Debug)]
#[allow(dead_code)]
pub enum FirmwareError {
    /// One-wire sensor communication error
    Sensor(SensorError),
    /// Persistent storage (flash) error
    Storage(StorageError),
}

/// One-wire temperature sensor errors.
#[derive(Clone, Copy, Debug)]
pub enum SensorError {
    /// One-wire bus stuck at logical low (device not responding or shorted)
    BusStuckLow,
    /// No device detected on the bus
    NoDevice,
    /// Temperature reading failed CRC validation
    CrcMismatch,
}

/// Persistent storage (flash) errors.
#[derive(Clone, Copy, Debug)]
pub enum StorageError {
    /// Storage subsystem not initialized
    NotInitialized,
    /// Requested partition missing from partition table
    MissingPartition,
    /// Partition too small for data
    PartitionTooSmall,
    /// Value out of valid range for storage
    OutOfRange,
    /// Low-level flash operation failed
    #[allow(dead_code)]
    Flash(FlashStorageError),
}

impl From<FlashStorageError> for StorageError {
    fn from(error: FlashStorageError) -> Self {
        Self::Flash(error)
    }
}

impl From<SensorError> for FirmwareError {
    fn from(error: SensorError) -> Self {
        Self::Sensor(error)
    }
}

impl From<StorageError> for FirmwareError {
    fn from(error: StorageError) -> Self {
        Self::Storage(error)
    }
}

impl From<FlashStorageError> for FirmwareError {
    fn from(error: FlashStorageError) -> Self {
        Self::Storage(StorageError::Flash(error))
    }
}

// For backward compatibility during migration, auto-convert SensorError
impl From<SensorError> for StorageError {
    fn from(_: SensorError) -> Self {
        // This should never happen in normal code paths;
        // it's here to ease migration. Prefer explicit conversion.
        Self::NotInitialized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensor_to_firmware_error() {
        let sensor_err = SensorError::BusStuckLow;
        let fw_err: FirmwareError = sensor_err.into();
        assert!(matches!(fw_err, FirmwareError::Sensor(SensorError::BusStuckLow)));
    }

    #[test]
    fn storage_to_firmware_error() {
        let storage_err = StorageError::NotInitialized;
        let fw_err: FirmwareError = storage_err.into();
        assert!(matches!(fw_err, FirmwareError::Storage(StorageError::NotInitialized)));
    }
}
