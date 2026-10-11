// SPDX-License-Identifier: MIT OR MulanPSL-2.0
// Copyright 2020-2022 RustSBI Team
// Adapted from spacemit-hal/xtask/src/fsbl.rs; see ../README.md.

use anyhow::{Context, Result, ensure};
use rsa::{
    BigUint, Pkcs1v15Sign, RsaPrivateKey, RsaPublicKey, pkcs1::DecodeRsaPrivateKey,
    pkcs8::DecodePublicKey, traits::PublicKeyParts,
};
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

// Vendor bootinfo_{sd,emmc,spinor,spinand}.json: spl_size_limit.
const IMAGE_LIMIT: usize = 0x36000;

/// Checks the development container structure and its embedded-key signatures.
pub fn validate(image: &[u8]) -> Result<()> {
    ensure!(
        (0x1120..=IMAGE_LIMIT).contains(&image.len()),
        "invalid FSBL size"
    );
    let size = usize::try_from(u64::from_le_bytes(image[0xfe8..0xff0].try_into()?))?;
    ensure!(
        size > 0 && size.is_multiple_of(32) && size.checked_add(0x1100) == Some(image.len()),
        "invalid SPL length"
    );
    let mut expected = [0; 32];
    header(&mut expected, 0x1000);
    ensure!(image[0x100..0x120] == expected, "invalid root header");
    header(&mut expected, size);
    ensure!(image[0xfe0..0x1000] == expected, "invalid SPL header");
    ensure!(
        Sha256::digest(&image[..256])[..] == image[0x2b8..0x2d8],
        "invalid root-key digest"
    );
    for (modulus, data, signature) in [
        (&image[..256], &image[0x100..0xb00], &image[0xb00..0xc00]),
        (
            &image[0x300..0x400],
            &image[0xfe0..0x1000 + size],
            &image[0x1000 + size..],
        ),
    ] {
        let key = RsaPublicKey::new(BigUint::from_bytes_be(modulus), 65537u32.into())?;
        key.verify(
            Pkcs1v15Sign::new::<Sha256>(),
            &Sha256::digest(data),
            signature,
        )?;
    }
    Ok(())
}

// K1 vendor board/spacemit/k1-x/configs/fsbl.json; secure=0 development container.
pub fn wrap(raw: &[u8], directory: &Path) -> Result<Vec<u8>> {
    let keys = Keys::load(directory)?;
    wrap_with_keys(raw, &keys)
}

struct Keys {
    root: RsaPrivateKey,
    spl: RsaPrivateKey,
    downstream: [RsaPublicKey; 3],
}

impl Keys {
    fn load(directory: &Path) -> Result<Self> {
        let read = |name: &str| {
            let path = directory.join(name);
            fs::read_to_string(&path).with_context(|| format!("read key {}", path.display()))
        };
        let keys = Self {
            root: RsaPrivateKey::from_pkcs1_pem(&read("rsakeypair0_prv.key")?)
                .context("parse RSA PKCS#1 root private key")?,
            spl: RsaPrivateKey::from_pkcs1_pem(&read("spl_pubkey_prv.key")?)
                .context("parse RSA PKCS#1 SPL private key")?,
            downstream: [
                RsaPublicKey::from_public_key_pem(&read("uboot_pubkey_pub.key")?)
                    .context("parse SPKI U-Boot public key")?,
                RsaPublicKey::from_public_key_pem(&read("kernel_pubkey_pub.key")?)
                    .context("parse SPKI kernel public key")?,
                RsaPublicKey::from_public_key_pem(&read("rootfs_pubkey_pub.key")?)
                    .context("parse SPKI rootfs public key")?,
            ],
        };
        keys.validate()?;
        Ok(keys)
    }

    fn validate(&self) -> Result<()> {
        self.root.validate().context("invalid root private key")?;
        self.spl.validate().context("invalid SPL private key")?;
        for key in [self.root.to_public_key(), self.spl.to_public_key()]
            .iter()
            .chain(self.downstream.iter())
        {
            ensure!(
                key.n().bits() == 2048 && key.e() == &BigUint::from(65537u32),
                "K1 container keys must be RSA-2048 with public exponent 65537"
            );
        }
        Ok(())
    }
}

fn wrap_with_keys(raw: &[u8], keys: &Keys) -> Result<Vec<u8>> {
    ensure!(
        !raw.is_empty() && raw.len() <= crate::elf2bin::LIMIT,
        "FSBL payload must contain 1..={:#x} bytes",
        crate::elf2bin::LIMIT
    );
    keys.validate()?;
    let root = &keys.root;
    let spl = &keys.spl;
    let size = raw.len().next_multiple_of(32);
    ensure!(
        size + 0x1100 <= IMAGE_LIMIT,
        "FSBL exceeds vendor BootInfo size limit"
    );
    let mut image = vec![0; 0x1000 + size];
    image[..256].copy_from_slice(&root.n().to_bytes_be());
    header(&mut image[0x100..0x120], 0x1000);
    image[0x124..0x128].copy_from_slice(&4u32.to_le_bytes());
    for (i, name) in ["spl", "uboot", "kernel", "rootfs"].iter().enumerate() {
        let offset = 0x128 + i * 20;
        image[offset..offset + name.len()].copy_from_slice(name.as_bytes());
        image[offset + 16..offset + 20].copy_from_slice(&(i as u32).to_le_bytes());
    }
    let hash = Sha256::digest(&image[..256]);
    image[0x2b8..0x2d8].copy_from_slice(&hash);
    image[0x300..0x400].copy_from_slice(&spl.n().to_bytes_be());
    for (i, key) in keys.downstream.iter().enumerate() {
        let offset = 0x400 + i * 256;
        image[offset..offset + 256].copy_from_slice(&key.n().to_bytes_be());
    }
    let signature = root.sign(
        Pkcs1v15Sign::new::<Sha256>(),
        &Sha256::digest(&image[0x100..0xb00]),
    )?;
    image[0xb00..0xc00].copy_from_slice(&signature);
    header(&mut image[0xfe0..0x1000], size);
    image[0x1000..0x1000 + raw.len()].copy_from_slice(raw);
    let signature = spl.sign(
        Pkcs1v15Sign::new::<Sha256>(),
        &Sha256::digest(&image[0xfe0..]),
    )?;
    image.extend(signature);
    Ok(image)
}

fn header(header: &mut [u8], size: usize) {
    header[..4].copy_from_slice(b"AIHD");
    header[4] = 1;
    header[8..16].copy_from_slice(&(size as u64).to_le_bytes());
    header[24..32].fill(0xa5);
}

#[cfg(test)]
mod tests {
    use super::*;
    use rsa::rand_core::OsRng;
    use std::sync::OnceLock;

    // Ephemeral test keys stay in memory; no private key is stored in this repository.
    fn keys() -> &'static Keys {
        static KEYS: OnceLock<Keys> = OnceLock::new();
        KEYS.get_or_init(|| {
            let root = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
            let spl = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
            let public = spl.to_public_key();
            Keys {
                root,
                spl,
                downstream: [public.clone(), public.clone(), public],
            }
        })
    }

    #[test]
    fn layout_and_signatures() {
        let raw: Vec<_> = (0..35).collect();
        let image = wrap_with_keys(&raw, keys()).unwrap();
        validate(&image).unwrap();
        for offset in [0, 0x120, 0x300, 0xb00, 0xfe0, 0x1000, 0x1040] {
            let mut corrupt = image.clone();
            corrupt[offset] ^= 1;
            assert!(
                validate(&corrupt).is_err(),
                "accepted corruption at {offset:#x}"
            );
        }
        assert_eq!(image.len(), 0x1140);
        assert_eq!(&image[0x100..0x108], b"AIHD\x01\0\0\0");
        assert_eq!(&image[0xfe8..0xff0], &64u64.to_le_bytes());
        assert_eq!(&image[0x1000..0x1023], raw);
        assert_eq!(&image[0x1023..0x1040], &[0; 29]);
        for (key, data, signature) in [
            (
                keys().root.to_public_key(),
                &image[0x100..0xb00],
                &image[0xb00..0xc00],
            ),
            (
                keys().spl.to_public_key(),
                &image[0xfe0..0x1040],
                &image[0x1040..],
            ),
        ] {
            key.verify(
                Pkcs1v15Sign::new::<Sha256>(),
                &Sha256::digest(data),
                signature,
            )
            .unwrap();
        }
    }

    #[test]
    fn rejects_empty_and_oversized() {
        assert!(wrap_with_keys(&[], keys()).is_err());
        assert!(wrap_with_keys(&vec![0; crate::elf2bin::LIMIT + 1], keys()).is_err());
        assert!(validate(&[]).is_err());
        assert!(validate(&vec![0; IMAGE_LIMIT + 1]).is_err());
    }

    #[test]
    fn maximum_payload_fits_bootinfo_with_padding_and_signature() {
        let raw = vec![0xa5; crate::elf2bin::LIMIT];
        let image = wrap_with_keys(&raw, keys()).unwrap();
        assert_eq!(image.len(), IMAGE_LIMIT);
        assert_eq!(&image[0x1000..0x1000 + raw.len()], raw);
        assert_eq!(&image[0xfe8..0xff0], &(raw.len() as u64).to_le_bytes());
        validate(&image).unwrap();
    }

    #[test]
    fn rejects_unsupported_key_size() {
        let small = RsaPrivateKey::new(&mut OsRng, 1024).unwrap();
        let keys = Keys {
            root: small,
            spl: keys().spl.clone(),
            downstream: keys().downstream.clone(),
        };
        assert!(wrap_with_keys(b"test", &keys).is_err());
    }
}
