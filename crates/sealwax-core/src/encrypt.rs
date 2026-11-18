//! Saving encrypted with the standard security handler (ISO 32000-2, 7.6.4): RC4 with a 128-bit
//! key (revision 3), AES-128 (revision 4) or AES-256 (revision 6), an owner password and a user
//! password (empty: the document opens without one), and the permissions granted.
//!
//! The randomness the AES-256 file key, the salts and the initialization vectors take is drawn
//! from 32 bytes the caller gives ([`Encrypt::seed`]): this crate reads no source of its own.

use std::fmt;

use aes::cipher::{BlockCipherEncrypt, KeyInit};
use aes::{Aes128, Aes256};
use md5::Md5;
use sha2::{Digest, Sha256, Sha384, Sha512};

use crate::error::{Error, Result};
use crate::object::{Dict, Name, Object, Stream};

/// The padding a password of revisions 3 and 4 is completed with (ISO 32000-2, 7.6.4.3.2).
const PADDING: [u8; 32] = [
    0x28, 0xBF, 0x4E, 0x5E, 0x4E, 0x75, 0x8A, 0x41, 0x64, 0x00, 0x4E, 0x56, 0xFF, 0xFA, 0x01, 0x08,
    0x2E, 0x2E, 0x00, 0xB6, 0xD0, 0x68, 0x3E, 0x80, 0x2F, 0x0C, 0xA9, 0xFE, 0x64, 0x53, 0x69, 0x7A,
];

/// The permission bits a document may grant (ISO 32000-2, table 22): print, modify, copy,
/// annotate, fill forms, extract for accessibility, assemble, print in high quality.
pub const PERMISSIONS: u32 = 0xF3C;

/// How a document is encrypted when saved.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cipher {
    /// RC4, a 128-bit key (`/V 2 /R 3`).
    Rc4,
    /// AES-128 (`/V 4 /R 4`, `AESV2`).
    Aes128,
    /// AES-256 (`/V 5 /R 6`, `AESV3`).
    Aes256,
}

/// Encryption for a full save ([`crate::SaveOptions::encrypt`]).
#[derive(Clone)]
pub struct Encrypt {
    pub cipher: Cipher,
    /// The password that grants every permission. Not empty.
    pub owner_password: String,
    /// The password a reader asks to open the document; empty, it opens without one.
    pub user_password: String,
    /// The permissions granted to who opens it with the user password, as their bits
    /// ([`PERMISSIONS`]: 4 print, 8 modify, 16 copy, 32 annotate, 256 fill forms, 512 extract
    /// for accessibility, 1024 assemble, 2048 print in high quality).
    pub permissions: u32,
    /// Random bytes from a secure source: the file key, salts and initialization vectors are
    /// drawn from them.
    pub seed: [u8; 32],
}

impl fmt::Debug for Encrypt {
    // Neither the passwords nor the seed.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Encrypt")
            .field("cipher", &self.cipher)
            .field("permissions", &self.permissions)
            .finish_non_exhaustive()
    }
}

/// Bytes drawn from a seed: SHA-256 of the seed and a counter, block after block.
struct Draw {
    seed: [u8; 32],
    counter: u64,
}

impl Draw {
    fn bytes<const N: usize>(&mut self) -> [u8; N] {
        let mut out = [0u8; N];
        for chunk in out.chunks_mut(32) {
            let block = Sha256::new()
                .chain_update(self.seed)
                .chain_update(self.counter.to_le_bytes())
                .finalize();
            self.counter += 1;
            chunk.copy_from_slice(&block[..chunk.len()]);
        }
        out
    }
}

fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut state: [u8; 256] = std::array::from_fn(|i| u8::try_from(i).unwrap_or(0));
    let mut j = 0u8;
    for i in 0..256 {
        j = j.wrapping_add(state[i]).wrapping_add(key[i % key.len()]);
        state.swap(i, usize::from(j));
    }
    let (mut i, mut j) = (0u8, 0u8);
    data.iter()
        .map(|byte| {
            i = i.wrapping_add(1);
            j = j.wrapping_add(state[usize::from(i)]);
            state.swap(usize::from(i), usize::from(j));
            let k = state[usize::from(state[usize::from(i)].wrapping_add(state[usize::from(j)]))];
            byte ^ k
        })
        .collect()
}

/// `data` encrypted with AES in CBC mode after `iv`, padded (PKCS #7) or not (whole blocks).
fn aes_cbc(key: &[u8], iv: [u8; 16], data: &[u8], padded: bool) -> Vec<u8> {
    let mut input = data.to_vec();
    if padded {
        let pad = 16 - input.len() % 16;
        input.extend(std::iter::repeat_n(u8::try_from(pad).unwrap_or(16), pad));
    }
    let encrypt: Box<dyn Fn(&mut aes::Block)> = match key.len() {
        32 => match Aes256::new_from_slice(key) {
            Ok(cipher) => Box::new(move |block| cipher.encrypt_block(block)),
            Err(_) => return Vec::new(),
        },
        _ => match Aes128::new_from_slice(&key[..16.min(key.len())]) {
            Ok(cipher) => Box::new(move |block| cipher.encrypt_block(block)),
            Err(_) => return Vec::new(),
        },
    };
    let mut out = Vec::with_capacity(input.len());
    let mut previous = iv;
    for chunk in input.chunks(16) {
        let mut block = aes::Block::default();
        for (index, byte) in block.iter_mut().enumerate() {
            *byte = chunk.get(index).copied().unwrap_or(0) ^ previous[index];
        }
        encrypt(&mut block);
        previous.copy_from_slice(block.as_slice());
        out.extend_from_slice(block.as_slice());
    }
    out
}

/// A password of revisions 3 and 4: its characters as bytes (`?` past Latin-1), cut or padded
/// to 32 bytes.
fn padded(password: &str) -> [u8; 32] {
    let bytes: Vec<u8> = password
        .chars()
        .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
        .take(32)
        .collect();
    let mut out = PADDING;
    out[..bytes.len()].copy_from_slice(&bytes);
    out[bytes.len()..].copy_from_slice(&PADDING[..32 - bytes.len()]);
    out
}

/// The hash of revision 6 (ISO 32000-2, algorithm 2.B).
fn hash_r6(password: &[u8], salt: &[u8], user: &[u8]) -> [u8; 32] {
    let mut k: Vec<u8> = Sha256::new()
        .chain_update(password)
        .chain_update(salt)
        .chain_update(user)
        .finalize()
        .to_vec();
    let mut round: usize = 0;
    loop {
        let mut k1 = Vec::with_capacity(64 * (password.len() + k.len() + user.len()));
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(user);
        }
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&k[16..32]);
        let e = aes_cbc(&k[..16], iv, &k1, false);
        let sum: u32 = e[..16].iter().map(|b| u32::from(*b)).sum();
        k = match sum % 3 {
            0 => Sha256::digest(&e).to_vec(),
            1 => Sha384::digest(&e).to_vec(),
            _ => Sha512::digest(&e).to_vec(),
        };
        round += 1;
        let last = usize::from(e.last().copied().unwrap_or(0));
        if round >= 64 && last + 32 <= round {
            break;
        }
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&k[..32]);
    out
}

/// What encrypting a save needs: its file key, and how it encrypts each object.
pub(crate) struct Security {
    cipher: Cipher,
    key: Vec<u8>,
    draw: Draw,
    /// The `/Encrypt` dictionary.
    pub(crate) dict: Dict,
}

impl Security {
    /// The file key and `/Encrypt` dictionary of `encrypt`, for a document whose first
    /// identifier is `id`.
    pub(crate) fn new(encrypt: &Encrypt, id: &[u8]) -> Result<Self> {
        if encrypt.owner_password.is_empty() {
            return Err(Error::InvalidArgument {
                what: "owner password",
            });
        }
        if encrypt.permissions & !PERMISSIONS != 0 {
            return Err(Error::InvalidArgument {
                what: "permissions",
            });
        }
        // Reserved bits 7, 8 and 13 to 32 set.
        let p = 0xFFFF_F0C0_u32 | encrypt.permissions;
        let p_signed = i64::from(i32::from_le_bytes(p.to_le_bytes()));
        let mut draw = Draw {
            seed: encrypt.seed,
            counter: 0,
        };
        let mut dict = Dict::new();
        dict.insert("Filter", Name::from("Standard"));
        dict.insert("P", p_signed);
        let key = match encrypt.cipher {
            Cipher::Rc4 | Cipher::Aes128 => {
                let (version, revision) = if encrypt.cipher == Cipher::Rc4 {
                    (2, 3)
                } else {
                    (4, 4)
                };
                // Algorithm 3: the owner entry.
                let mut owner_key = Md5::digest(padded(&encrypt.owner_password)).to_vec();
                for _ in 0..50 {
                    owner_key = Md5::digest(&owner_key[..16]).to_vec();
                }
                let mut o = rc4(&owner_key[..16], &padded(&encrypt.user_password));
                for i in 1..=19u8 {
                    let key: Vec<u8> = owner_key[..16].iter().map(|b| b ^ i).collect();
                    o = rc4(&key, &o);
                }
                // Algorithm 2: the file key.
                let mut file_key = Md5::new()
                    .chain_update(padded(&encrypt.user_password))
                    .chain_update(&o)
                    .chain_update(p.to_le_bytes())
                    .chain_update(id)
                    .finalize()
                    .to_vec();
                for _ in 0..50 {
                    file_key = Md5::digest(&file_key[..16]).to_vec();
                }
                file_key.truncate(16);
                // Algorithm 5: the user entry, 16 bytes completed with any 16.
                let mut u = rc4(
                    &file_key,
                    &Md5::new().chain_update(PADDING).chain_update(id).finalize(),
                );
                for i in 1..=19u8 {
                    let key: Vec<u8> = file_key.iter().map(|b| b ^ i).collect();
                    u = rc4(&key, &u);
                }
                u.extend_from_slice(&draw.bytes::<16>());
                dict.insert("V", version);
                dict.insert("R", revision);
                dict.insert("Length", 128);
                dict.insert("O", Object::String(o));
                dict.insert("U", Object::String(u));
                if encrypt.cipher == Cipher::Aes128 {
                    dict.insert("CF", crypt_filters("AESV2", 16));
                    dict.insert("StmF", Name::from("StdCF"));
                    dict.insert("StrF", Name::from("StdCF"));
                    dict.insert("EncryptMetadata", Object::Bool(true));
                }
                file_key
            }
            Cipher::Aes256 => {
                let file_key = draw.bytes::<32>();
                let user = utf8_password(&encrypt.user_password);
                let owner = utf8_password(&encrypt.owner_password);
                // Algorithm 8: the user entries.
                let (validation, salt): ([u8; 8], [u8; 8]) = (draw.bytes(), draw.bytes());
                let mut u = hash_r6(user, &validation, &[]).to_vec();
                u.extend_from_slice(&validation);
                u.extend_from_slice(&salt);
                let ue = aes_cbc(&hash_r6(user, &salt, &[]), [0; 16], &file_key, false);
                // Algorithm 9: the owner entries, over the whole user entry.
                let (validation, salt): ([u8; 8], [u8; 8]) = (draw.bytes(), draw.bytes());
                let mut o = hash_r6(owner, &validation, &u).to_vec();
                o.extend_from_slice(&validation);
                o.extend_from_slice(&salt);
                let oe = aes_cbc(&hash_r6(owner, &salt, &u), [0; 16], &file_key, false);
                // Algorithm 10: the permissions, encrypted (one block, ECB).
                let mut perms = [0u8; 16];
                perms[..4].copy_from_slice(&p.to_le_bytes());
                perms[4..8].copy_from_slice(&[0xFF; 4]);
                perms[8..12].copy_from_slice(b"Tadb");
                perms[12..].copy_from_slice(&draw.bytes::<4>());
                let perms = aes_cbc(&file_key, [0; 16], &perms, false);
                dict.insert("V", 5);
                dict.insert("R", 6);
                dict.insert("Length", 256);
                dict.insert("CF", crypt_filters("AESV3", 32));
                dict.insert("StmF", Name::from("StdCF"));
                dict.insert("StrF", Name::from("StdCF"));
                dict.insert("O", Object::String(o));
                dict.insert("U", Object::String(u));
                dict.insert("OE", Object::String(oe));
                dict.insert("UE", Object::String(ue));
                dict.insert("Perms", Object::String(perms));
                dict.insert("EncryptMetadata", Object::Bool(true));
                file_key.to_vec()
            }
        };
        Ok(Self {
            cipher: encrypt.cipher,
            key,
            draw,
            dict,
        })
    }

    /// `data` encrypted for the object numbered `number` (generation 0).
    fn bytes(&mut self, number: u32, data: &[u8]) -> Vec<u8> {
        let object_key = |aes: bool| -> Vec<u8> {
            let mut hasher = Md5::new()
                .chain_update(&self.key)
                .chain_update(&number.to_le_bytes()[..3])
                .chain_update([0u8, 0]);
            if aes {
                hasher.update(b"sAlT");
            }
            hasher.finalize()[..16].to_vec()
        };
        match self.cipher {
            Cipher::Rc4 => rc4(&object_key(false), data),
            Cipher::Aes128 => {
                let key = object_key(true);
                let iv = self.draw.bytes::<16>();
                let mut out = iv.to_vec();
                out.extend(aes_cbc(&key, iv, data, true));
                out
            }
            Cipher::Aes256 => {
                let iv = self.draw.bytes::<16>();
                let mut out = iv.to_vec();
                out.extend(aes_cbc(&self.key, iv, data, true));
                out
            }
        }
    }

    /// `object`, the object numbered `number`, its strings and stream data encrypted.
    pub(crate) fn object(&mut self, number: u32, object: &Object) -> Object {
        match object {
            Object::String(bytes) => Object::String(self.bytes(number, bytes)),
            Object::Array(items) => {
                Object::Array(items.iter().map(|item| self.object(number, item)).collect())
            }
            Object::Dict(dict) => Object::Dict(self.dict_of(number, dict)),
            Object::Stream(stream) => Object::Stream(Stream {
                dict: self.dict_of(number, &stream.dict),
                data: self.bytes(number, &stream.data),
            }),
            other => other.clone(),
        }
    }

    fn dict_of(&mut self, number: u32, dict: &Dict) -> Dict {
        let mut out = Dict::new();
        for (key, value) in dict.iter() {
            out.insert(key.clone(), self.object(number, value));
        }
        out
    }

    /// Stream data, the object stream numbered `number`'s.
    pub(crate) fn stream_data(&mut self, number: u32, data: &[u8]) -> Vec<u8> {
        self.bytes(number, data)
    }
}

/// A password of revision 6: its UTF-8 bytes, 127 at most (ISO 32000-2, 7.6.4.3.3, the
/// SASLprep profile not applied).
fn utf8_password(password: &str) -> &[u8] {
    let bytes = password.as_bytes();
    let mut end = bytes.len().min(127);
    while !password.is_char_boundary(end) {
        end -= 1;
    }
    &bytes[..end]
}

/// The crypt filter `StdCF`, with the method `method` and a key of `length` bytes.
fn crypt_filters(method: &str, length: i64) -> Dict {
    let mut filter = Dict::new();
    filter.insert("CFM", Name::from(method));
    filter.insert("AuthEvent", Name::from("DocOpen"));
    filter.insert("Length", length);
    let mut filters = Dict::new();
    filters.insert("StdCF", filter);
    filters
}

#[cfg(test)]
mod tests {
    use super::{aes_cbc, rc4};

    #[test]
    fn rc4_and_aes_give_the_known_vectors() {
        // RFC 6229, key 0x0102030405, the first bytes of the key stream.
        assert_eq!(
            rc4(&[1, 2, 3, 4, 5], &[0; 8]),
            [0xB2, 0x39, 0x63, 0x05, 0xF0, 0x3D, 0xC0, 0x27]
        );
        // NIST SP 800-38A, F.2.1: CBC-AES128, the first block.
        let key = [
            0x2B, 0x7E, 0x15, 0x16, 0x28, 0xAE, 0xD2, 0xA6, 0xAB, 0xF7, 0x15, 0x88, 0x09, 0xCF,
            0x4F, 0x3C,
        ];
        let iv: [u8; 16] = std::array::from_fn(|i| u8::try_from(i).unwrap_or(0));
        let plain = [
            0x6B, 0xC1, 0xBE, 0xE2, 0x2E, 0x40, 0x9F, 0x96, 0xE9, 0x3D, 0x7E, 0x11, 0x73, 0x93,
            0x17, 0x2A,
        ];
        assert_eq!(
            aes_cbc(&key, iv, &plain, false),
            [
                0x76, 0x49, 0xAB, 0xAC, 0x81, 0x19, 0xB2, 0x46, 0xCE, 0xE9, 0x8E, 0x9B, 0x12, 0xE9,
                0x19, 0x7D
            ]
        );
    }
}
