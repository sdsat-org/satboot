extern crate std;

use super::{Error, Fdt, PAYLOAD_LIMIT, SBI_LIMIT, STAGING_START, inspect, layout};
use std::{vec, vec::Vec};

// Build small, self-contained version-17 trees. No board dumps, filesystem access,
// MMIO or physical-memory writes are involved in these host tests.
#[derive(Default)]
struct Tree {
    structure: Vec<u8>,
    strings: Vec<u8>,
}

impl Tree {
    fn token(&mut self, value: u32) {
        self.structure.extend_from_slice(&value.to_be_bytes());
    }

    fn pad(&mut self) {
        while !self.structure.len().is_multiple_of(4) {
            self.structure.push(0);
        }
    }

    fn begin(&mut self, name: &str) {
        self.token(1);
        self.structure.extend_from_slice(name.as_bytes());
        self.structure.push(0);
        self.pad();
    }

    fn end(&mut self) {
        self.token(2);
    }

    fn property(&mut self, name: &str, value: &[u8]) {
        self.token(3);
        self.token(value.len() as u32);
        self.token(self.strings.len() as u32);
        self.strings.extend_from_slice(name.as_bytes());
        self.strings.push(0);
        self.structure.extend_from_slice(value);
        self.pad();
    }

    fn text(&mut self, name: &str, value: &str) {
        let mut bytes = value.as_bytes().to_vec();
        bytes.push(0);
        self.property(name, &bytes);
    }

    fn finish(mut self) -> Vec<u8> {
        self.token(9);
        // 40-byte header, followed by the empty 16-byte reservation map.
        let structure_offset = 56;
        let strings_offset = structure_offset + self.structure.len();
        let total = strings_offset + self.strings.len();
        let mut bytes = Vec::with_capacity(total);
        for field in [
            0xd00d_feed,
            total as u32,
            structure_offset as u32,
            strings_offset as u32,
            40,
            17,
            16,
            0,
            self.strings.len() as u32,
            self.structure.len() as u32,
        ] {
            bytes.extend_from_slice(&field.to_be_bytes());
        }
        bytes.extend_from_slice(&[0; 16]);
        bytes.extend_from_slice(&self.structure);
        bytes.extend_from_slice(&self.strings);
        bytes
    }
}

fn board_dtb(compatible: &str) -> Vec<u8> {
    let mut tree = Tree::default();
    tree.begin("");
    tree.text("compatible", compatible);
    tree.end();
    tree.finish()
}

fn image(
    tree: &mut Tree,
    name: &str,
    kind: &str,
    os: Option<&str>,
    data: &[u8],
    load: Option<u64>,
    entry: Option<u64>,
) {
    tree.begin(name);
    tree.text("type", kind);
    tree.text("arch", "riscv");
    tree.text("compression", "none");
    if let Some(os) = os {
        tree.text("os", os);
    }
    if let Some(address) = load {
        tree.property("load", &address.to_be_bytes());
    }
    if let Some(address) = entry {
        tree.property("entry", &address.to_be_bytes());
    }
    tree.property("data", data);
    tree.begin("hash-1");
    tree.text("algo", "crc32");
    tree.property("value", &layout::crc32(data).to_be_bytes());
    tree.end();
    tree.end();
}

fn sbi_fit() -> Vec<u8> {
    let mut tree = Tree::default();
    tree.begin("");
    tree.begin("images");
    image(
        &mut tree,
        "sbi",
        "firmware",
        Some("opensbi"),
        &[0x13, 0, 0, 0, 0x73, 0, 0, 0],
        Some(0),
        Some(0),
    );
    tree.end();
    tree.begin("configurations");
    tree.text("default", "conf-sbi");
    tree.begin("conf-sbi");
    tree.text("firmware", "sbi");
    tree.end();
    tree.end();
    tree.end();
    tree.finish()
}

const PAYLOAD: [u8; 10] = [0x13, 0, 0, 0, 0x13, 0, 0, 0, 1, 0];
const PAYLOAD_ADDRESS: u64 = 0x0020_0000;

fn payload_fit() -> Vec<u8> {
    payload_fit_with_dtb_load(None)
}

fn payload_fit_with_dtb_load(dtb_load: Option<u64>) -> Vec<u8> {
    let mut tree = Tree::default();
    tree.begin("");
    tree.begin("images");
    image(
        &mut tree,
        "uboot",
        "standalone",
        Some("U-Boot"),
        &PAYLOAD,
        Some(PAYLOAD_ADDRESS),
        Some(PAYLOAD_ADDRESS),
    );
    for (name, compatible) in [
        ("fdt-bpi", "bananapi,bpi-f3"),
        ("fdt-muse", "spacemit,muse-card"),
    ] {
        image(
            &mut tree,
            name,
            "flat_dt",
            None,
            &board_dtb(compatible),
            dtb_load,
            None,
        );
    }
    tree.end();
    tree.begin("configurations");
    tree.text("default", "conf-bpi");
    for (name, product, dtb) in [
        ("conf-bpi", "k1-x_deb1", "fdt-bpi"),
        ("conf-muse", "k1-x_MUSE-Card", "fdt-muse"),
    ] {
        tree.begin(name);
        tree.text("description", product);
        tree.text("loadables", "uboot");
        tree.text("fdt", dtb);
        tree.end();
    }
    tree.end();
    tree.end();
    tree.finish()
}

// Locate a property through a valid tree, then mutate its wire representation.
// Mutations keep unrelated properties, including image hashes, unchanged.
fn property_offset(bytes: &[u8], path: &[&str], property: &str) -> usize {
    let mut node = Fdt::root(bytes).unwrap();
    for name in path {
        node = node.child(name).unwrap();
    }
    node.required(property).unwrap().as_ptr() as usize - bytes.as_ptr() as usize
}

fn set_address(bytes: &mut [u8], image: &str, property: &str, address: u64) {
    let offset = property_offset(bytes, &["images", image], property);
    bytes[offset..offset + 8].copy_from_slice(&address.to_be_bytes());
}

#[test]
fn default_bpi_fit_keeps_sbi_at_physical_zero_and_appends_aligned_dtb() {
    let sbi = sbi_fit();
    let payload = payload_fit();
    let images = inspect(&sbi, &payload, None).unwrap();
    let destinations = images.destinations();
    assert_eq!(destinations.sbi.address, 0);
    assert_eq!(destinations.sbi_entry, 0);
    assert_eq!(destinations.sbi.size, 8);
    assert_eq!(destinations.payload.address, PAYLOAD_ADDRESS as usize);
    assert_eq!(destinations.payload_entry, PAYLOAD_ADDRESS as usize);
    assert_eq!(images.payload.data, PAYLOAD);
    assert_eq!(destinations.dtb.address, PAYLOAD_ADDRESS as usize + 16);
    assert_eq!(images.dtb.data, board_dtb("bananapi,bpi-f3"));
}

#[test]
fn explicit_product_selects_its_dtb_instead_of_the_fit_default() {
    let sbi = sbi_fit();
    let payload = payload_fit();
    for (product, compatible) in [
        ("k1-x_deb1", "bananapi,bpi-f3"),
        ("k1-x_MUSE-Card", "spacemit,muse-card"),
    ] {
        let images = inspect(&sbi, &payload, Some(product)).unwrap();
        assert_eq!(images.dtb.data, board_dtb(compatible));
    }
    assert_eq!(
        inspect(&sbi, &payload, Some("unknown-board")).err(),
        Some(Error::Missing)
    );
}

#[test]
fn changed_image_data_and_changed_hash_are_rejected() {
    let original_sbi = sbi_fit();
    let original_payload = payload_fit();
    for (path, property) in [
        (&["images", "uboot"][..], "data"),
        (&["images", "uboot", "hash-1"][..], "value"),
        (&["images", "fdt-bpi"][..], "data"),
    ] {
        let mut payload = original_payload.clone();
        let offset = property_offset(&payload, path, property);
        payload[offset] ^= 1;
        assert_eq!(
            inspect(&original_sbi, &payload, None).err(),
            Some(Error::Integrity)
        );
    }
    let mut sbi = original_sbi;
    let offset = property_offset(&sbi, &["images", "sbi"], "data");
    sbi[offset] ^= 1;
    assert_eq!(
        inspect(&sbi, &original_payload, None).err(),
        Some(Error::Integrity)
    );
}

#[test]
fn compressed_images_are_rejected_before_their_bytes_can_be_loaded() {
    let sbi = sbi_fit();
    let mut payload = payload_fit();
    let offset = property_offset(&payload, &["images", "uboot"], "compression");
    payload[offset..offset + 5].copy_from_slice(b"gzip\0");
    assert_eq!(
        inspect(&sbi, &payload, None).err(),
        Some(Error::Unsupported)
    );
}

#[test]
fn every_truncated_prefix_of_either_fit_is_rejected_without_panicking() {
    let sbi = sbi_fit();
    let payload = payload_fit();
    for length in 0..sbi.len() {
        assert!(inspect(&sbi[..length], &payload, None).is_err());
    }
    for length in 0..payload.len() {
        assert!(inspect(&sbi, &payload[..length], None).is_err());
    }
}

#[test]
fn dtb_overlapping_payload_is_rejected_even_with_valid_data_hashes() {
    let sbi = sbi_fit();
    let payload = payload_fit_with_dtb_load(Some(PAYLOAD_ADDRESS));
    assert_eq!(inspect(&sbi, &payload, None).err(), Some(Error::Overlap));
}

#[test]
fn payload_entry_must_be_aligned_and_inside_its_loaded_bytes() {
    let sbi = sbi_fit();
    for entry in [
        PAYLOAD_ADDRESS - 2,
        PAYLOAD_ADDRESS + 1,
        PAYLOAD_ADDRESS + PAYLOAD.len() as u64,
    ] {
        let mut payload = payload_fit();
        set_address(&mut payload, "uboot", "entry", entry);
        assert_eq!(inspect(&sbi, &payload, None).err(), Some(Error::Address));
    }
}

#[test]
fn sbi_entry_cannot_escape_its_image() {
    let payload = payload_fit();
    for entry in [1, 8] {
        let mut sbi = sbi_fit();
        set_address(&mut sbi, "sbi", "entry", entry);
        assert_eq!(inspect(&sbi, &payload, None).err(), Some(Error::Address));
    }
}

#[test]
fn payload_must_not_overlap_staging_or_the_reserved_sbi_region() {
    let sbi = sbi_fit();
    for address in [PAYLOAD_ADDRESS - 8, STAGING_START as u64 - 8] {
        let mut payload = payload_fit();
        set_address(&mut payload, "uboot", "load", address);
        set_address(&mut payload, "uboot", "entry", address);
        assert_eq!(inspect(&sbi, &payload, None).err(), Some(Error::Address));
    }
}

#[test]
fn overflowing_destination_end_is_rejected() {
    let sbi = sbi_fit();
    let mut payload = payload_fit();
    set_address(&mut payload, "uboot", "load", u64::MAX - 1);
    assert_eq!(inspect(&sbi, &payload, None).err(), Some(Error::Bounds));
}

#[test]
fn forged_fdt_lengths_and_offsets_are_rejected_without_panicking() {
    let sbi = sbi_fit();
    let original = payload_fit();
    for field in [4, 8, 12, 16, 32, 36] {
        let mut payload = original.clone();
        payload[field..field + 4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert!(inspect(&sbi, &payload, None).is_err());
    }
    let mut payload = original;
    let data = property_offset(&payload, &["images", "uboot"], "data");
    payload[data - 8..data - 4].copy_from_slice(&u32::MAX.to_be_bytes());
    assert_eq!(inspect(&sbi, &payload, None).err(), Some(Error::Bounds));
}

#[test]
fn fit_size_limits_are_checked_before_parsing() {
    assert_eq!(
        inspect(&vec![0; SBI_LIMIT + 1], &[], None).err(),
        Some(Error::Bounds)
    );
    assert_eq!(
        inspect(&[], &vec![0; PAYLOAD_LIMIT + 1], None).err(),
        Some(Error::Bounds)
    );
}

const PARTITIONS: &str = "128K(bootinfo),256K(fsbl),64K(env),512K(opensbi),2M(uboot),-(rootfs)";
const FLASH_CAPACITY: u32 = 16 * 1024 * 1024;

fn environment(size: usize, partitions: &str) -> Vec<u8> {
    let mut bytes = vec![0; size];
    let entry = std::format!("mtdparts=mtdparts=d420c000.spi-0:{partitions}");
    bytes[4..4 + entry.len()].copy_from_slice(entry.as_bytes());
    let checksum = layout::crc32(&bytes[4..]);
    bytes[..4].copy_from_slice(&checksum.to_le_bytes());
    bytes
}

#[test]
fn crc32_matches_the_standard_check_vector() {
    assert_eq!(layout::crc32(b"123456789"), 0xcbf4_3926);
}

#[test]
fn both_vendor_environment_sizes_resolve_boot_partitions() {
    for size in [layout::ENV_SIZE, layout::ENV_MAX_SIZE] {
        let env = environment(size, PARTITIONS);
        let parsed = layout::Layout::from_env(&env, FLASH_CAPACITY).unwrap();
        assert_eq!(parsed.env_size, size);
        assert_eq!(parsed.fsbl.offset, 0x20000);
        assert_eq!(parsed.fsbl.size, 0x40000);
        assert_eq!(parsed.sbi.offset, 0x70000);
        assert_eq!(parsed.sbi.size, 0x80000);
        assert_eq!(parsed.payload.offset, 0xf0000);
        assert_eq!(parsed.payload.size, 0x200000);
        // The reader always fetches 64 KiB, including when only 16 KiB is CRC-covered.
        let mut read_window = env;
        read_window.resize(layout::ENV_MAX_SIZE, 0xff);
        assert_eq!(
            layout::Layout::from_env(&read_window, FLASH_CAPACITY).unwrap(),
            parsed
        );
    }
}

#[test]
fn corrupt_environment_is_rejected_before_partition_parsing() {
    for size in [layout::ENV_SIZE, layout::ENV_MAX_SIZE] {
        let mut env = environment(size, PARTITIONS);
        env[size - 1] ^= 1;
        assert_eq!(
            layout::Layout::from_env(&env, FLASH_CAPACITY),
            Err(layout::Error::Integrity)
        );
    }
}

#[test]
fn crc_valid_overlapping_partitions_are_rejected() {
    let env = environment(
        layout::ENV_SIZE,
        "128K(bootinfo),256K(fsbl),64K(env),512K(opensbi),2M@0xe0000(uboot)",
    );
    assert_eq!(
        layout::Layout::from_env(&env, FLASH_CAPACITY),
        Err(layout::Error::Overlap)
    );
}

#[test]
fn crc_valid_partition_size_overflow_is_rejected() {
    let env = environment(
        layout::ENV_SIZE,
        "128K(bootinfo),256K(fsbl),64K(env),512K(opensbi),4G(uboot)",
    );
    assert_eq!(
        layout::Layout::from_env(&env, FLASH_CAPACITY),
        Err(layout::Error::Bounds)
    );
}
