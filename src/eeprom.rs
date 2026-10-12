//! Reads SpacemiT's ONIE-compatible board EEPROM.

use crate::{Error, Result};

/// DDR fields recorded in EEPROM, without board defaults.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DdrConfig<'a> {
    /// Populated chip selects.
    pub chip_selects: Option<u8>,
    /// Memory type.
    pub memory_type: Option<&'a str>,
    /// Transfer rate in MT/s.
    pub data_rate: Option<u16>,
    /// Transmit termination resistance in ohms.
    pub tx_odt: Option<u8>,
}

/// Board information borrowed from a validated EEPROM image.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EepromInfo<'a> {
    /// Product name.
    pub product_name: Option<&'a str>,
    /// Board part number.
    pub part_number: Option<&'a str>,
    /// Board serial number.
    pub serial_number: Option<&'a str>,
    /// Base Ethernet MAC address.
    pub base_mac: Option<[u8; 6]>,
    /// Manufacture date as MM/DD/YYYY HH:MM:SS.
    pub manufacture_date: Option<&'a str>,
    /// Hardware revision.
    pub device_version: Option<u8>,
    /// Label revision.
    pub label_revision: Option<&'a str>,
    /// Platform name.
    pub platform_name: Option<&'a str>,
    /// Factory ONIE version.
    pub onie_version: Option<&'a str>,
    /// Number of sequential Ethernet MAC addresses.
    pub mac_count: Option<u16>,
    /// Manufacturer name.
    pub manufacturer: Option<&'a str>,
    /// Two-letter manufacturing country code.
    pub country_code: Option<&'a str>,
    /// Vendor name.
    pub vendor: Option<&'a str>,
    /// Diagnostic software version.
    pub diagnostic_version: Option<&'a str>,
    /// Vendor service tag.
    pub service_tag: Option<&'a str>,
    /// SpacemiT SDK version number.
    pub sdk_version: Option<u32>,
    /// DDR information, absent when no DDR TLVs exist.
    pub ddr: Option<DdrConfig<'a>>,
    /// Wi-Fi MAC address.
    pub wifi_mac: Option<[u8; 6]>,
    /// Bluetooth address.
    pub bluetooth_address: Option<[u8; 6]>,
    /// PMIC model name.
    pub pmic_type: Option<&'a str>,
    /// EEPROM I²C controller index.
    pub eeprom_i2c_index: Option<u8>,
    /// EEPROM I²C pin group.
    pub eeprom_pin_group: Option<u8>,
}

/// Validates the TLV image and reads its optional board fields.
pub fn parse(bytes: &[u8]) -> Result<EepromInfo<'_>> {
    let mut info = EepromInfo::default();
    parse_into(bytes, &mut info)?;
    Ok(info)
}

/// Parses into caller-owned storage; errors may leave partially decoded fields.
pub fn parse_into<'a>(bytes: &'a [u8], info: &mut EepromInfo<'a>) -> Result<()> {
    let mut fields = payload(bytes)?;
    info.product_name = None;
    info.part_number = None;
    info.serial_number = None;
    info.base_mac = None;
    info.manufacture_date = None;
    info.device_version = None;
    info.label_revision = None;
    info.platform_name = None;
    info.onie_version = None;
    info.mac_count = None;
    info.manufacturer = None;
    info.country_code = None;
    info.vendor = None;
    info.diagnostic_version = None;
    info.service_tag = None;
    info.sdk_version = None;
    info.ddr = None;
    info.wifi_mac = None;
    info.bluetooth_address = None;
    info.pmic_type = None;
    info.eeprom_i2c_index = None;
    info.eeprom_pin_group = None;
    while !fields.is_empty() {
        if fields.len() < 2 {
            return Err(Error::Format);
        }
        let next = 2 + usize::from(fields[1]);
        if next > fields.len() {
            return Err(Error::Format);
        }
        let value = &fields[2..next];
        if matches!(fields[0], 0x41..=0x44) {
            parse_ddr(
                info.ddr.get_or_insert_with(DdrConfig::default),
                fields[0],
                value,
            )?;
        } else {
            parse_field(info, fields[0], value)?;
        }
        fields = &fields[next..];
    }
    Ok(())
}

fn payload(bytes: &[u8]) -> Result<&[u8]> {
    let Some(&[b'T', b'l', b'v', b'I', b'n', b'f', b'o', 0, 1, hi, lo]) = bytes.first_chunk::<11>()
    else {
        return Err(Error::Format);
    };
    let end = 11 + usize::from(u16::from_be_bytes([hi, lo]));
    if !matches!(end, 17..=2048)
        || end > bytes.len()
        || bytes[end - 6] != 0xfe
        || bytes[end - 5] != 4
    {
        return Err(Error::Format);
    }
    let crc = [
        bytes[end - 4],
        bytes[end - 3],
        bytes[end - 2],
        bytes[end - 1],
    ];
    if crc32(&bytes[..end - 4]) != u32::from_be_bytes(crc) {
        return Err(Error::Checksum);
    }
    Ok(&bytes[11..end - 6])
}

fn parse_field<'a>(info: &mut EepromInfo<'a>, code: u8, value: &'a [u8]) -> Result<()> {
    // ONIE type codes and vendor include/configs/k1-x.h.
    match code {
        0x21..=0x23 | 0x25 | 0x27..=0x29 | 0x2b..=0x2f | 0x80 => {
            if (code == 0x25 && value.len() != 19) || (code == 0x2c && value.len() != 2) {
                return Err(Error::Format);
            }
            let field = match code {
                0x21 => &mut info.product_name,
                0x22 => &mut info.part_number,
                0x23 => &mut info.serial_number,
                0x25 => &mut info.manufacture_date,
                0x27 => &mut info.label_revision,
                0x28 => &mut info.platform_name,
                0x29 => &mut info.onie_version,
                0x2b => &mut info.manufacturer,
                0x2c => &mut info.country_code,
                0x2d => &mut info.vendor,
                0x2e => &mut info.diagnostic_version,
                0x2f => &mut info.service_tag,
                _ => &mut info.pmic_type,
            };
            set(field, text(value)?)
        }
        0x24 | 0x60 | 0x61 => {
            let field = match code {
                0x24 => &mut info.base_mac,
                0x60 => &mut info.wifi_mac,
                _ => &mut info.bluetooth_address,
            };
            set(field, *array(value)?)
        }
        0x26 | 0x81 | 0x82 => {
            let field = match code {
                0x26 => &mut info.device_version,
                0x81 => &mut info.eeprom_i2c_index,
                _ => &mut info.eeprom_pin_group,
            };
            set(field, array::<1>(value)?[0])
        }
        0x2a => set(&mut info.mac_count, u16::from_be_bytes(*array(value)?)),
        0x40 => set(&mut info.sdk_version, number(value)?),
        0x00 | 0xfe | 0xff => Err(Error::Format),
        _ => Ok(()),
    }
}

fn parse_ddr<'a>(ddr: &mut DdrConfig<'a>, code: u8, value: &'a [u8]) -> Result<()> {
    match code {
        0x41 => set(&mut ddr.chip_selects, array::<1>(value)?[0]),
        0x42 => set(&mut ddr.memory_type, text(value)?),
        0x43 => set(&mut ddr.data_rate, u16::from_be_bytes(*array(value)?)),
        _ => set(&mut ddr.tx_odt, array::<1>(value)?[0]),
    }
}

fn set<T>(field: &mut Option<T>, value: T) -> Result<()> {
    if field.is_some() {
        return Err(Error::Format);
    }
    *field = Some(value);
    Ok(())
}

fn array<const N: usize>(bytes: &[u8]) -> Result<&[u8; N]> {
    bytes.try_into().map_err(|_| Error::Format)
}

fn text(bytes: &[u8]) -> Result<&str> {
    let bytes = if !bytes.is_empty() && bytes[bytes.len() - 1] == 0 {
        &bytes[..bytes.len() - 1]
    } else {
        bytes
    };
    let mut index = 0;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte == 0 || !byte.is_ascii() {
            return Err(Error::Format);
        }
        index += 1;
    }
    // SAFETY: The loop accepts only non-NUL ASCII, a subset of valid UTF-8.
    Ok(unsafe { core::str::from_utf8_unchecked(bytes) })
}

fn number(bytes: &[u8]) -> Result<u32> {
    if !matches!(bytes.len(), 1..=4) {
        return Err(Error::Format);
    }
    Ok(bytes
        .iter()
        .fold(0, |value, &byte| (value << 8) | u32::from(byte)))
}

fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        let mut remaining = 8;
        while remaining != 0 {
            remaining -= 1;
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}
