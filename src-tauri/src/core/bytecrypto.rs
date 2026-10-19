// Trae byteCrypto（byteCrypto.js）的 Rust 复刻。
// 格式：magic(6B) || seed(32B) || AES-128-CBC 密文，整体 base64（前缀 dGMFEAAA）。
// 派生：aesKey‖iv = SHA512(SHA512(seed) ‖ mask(64B)) 前 32 字节；
// 明文布局：SHA512(plain)(64B) ‖ plain。
use aes::Aes128;
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use cbc::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use rand::RngCore;
use sha2::{Digest, Sha512};

use super::bytecrypto_const::{HIE, QIE, VIE, WIE};

type Aes128CbcEnc = cbc::Encryptor<Aes128>;
type Aes128CbcDec = cbc::Decryptor<Aes128>;

const MAGIC_AES: [u8; 6] = [116, 99, 5, 16, 0, 0];
const MAGIC_AES_PRIVATE: [u8; 6] = [18, 57, 32, 32, 2, 3];
const HEADER_LEN: usize = 6;
const SEED_LEN: usize = 32;
const CHECK_LEN: usize = 64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlobVersion {
    Aes,
    AesPrivate,
}

#[derive(Debug)]
pub enum ByteCryptoError {
    UnknownVersion,
    BadLength,
    BadCheck,
    BadPadding,
}

impl std::fmt::Display for ByteCryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownVersion => write!(f, "unknown blob version"),
            Self::BadLength => write!(f, "bad blob length"),
            Self::BadCheck => write!(f, "sha512 check failed"),
            Self::BadPadding => write!(f, "aes padding invalid"),
        }
    }
}

impl std::error::Error for ByteCryptoError {}

fn sha512(data: &[u8]) -> [u8; 64] {
    let mut h = Sha512::new();
    h.update(data);
    h.finalize().into()
}

fn detect_version(blob: &[u8]) -> Option<BlobVersion> {
    if blob.len() < HEADER_LEN {
        return None;
    }
    if blob[..6] == MAGIC_AES {
        return Some(BlobVersion::Aes);
    }
    if blob[..6] == MAGIC_AES_PRIVATE {
        return Some(BlobVersion::AesPrivate);
    }
    None
}

fn mask(version: BlobVersion) -> [u8; 64] {
    let mut m = [0u8; 64];
    match version {
        BlobVersion::Aes => {
            for i in 0..64 {
                m[i] = VIE[i] ^ QIE[i];
            }
        }
        BlobVersion::AesPrivate => {
            for i in 0..64 {
                m[i] = HIE[i] ^ WIE[i];
            }
        }
    }
    m
}

/// Rie(seed, AES128, 16, version) -> {aesKey(16), iv(16)}
fn derive_key_iv(seed: &[u8], version: BlobVersion) -> ([u8; 16], [u8; 16]) {
    let mut n = [0u8; 128];
    n[..64].copy_from_slice(&sha512(seed));
    n[64..].copy_from_slice(&mask(version));
    let digest = sha512(&n);
    n[..64].copy_from_slice(&digest);
    let mut aes_key = [0u8; 16];
    let mut iv = [0u8; 16];
    aes_key.copy_from_slice(&n[..16]);
    iv.copy_from_slice(&n[16..32]);
    (aes_key, iv)
}

/// 解密原始 blob（magic‖seed‖密文）-> plain
pub fn decrypt(blob: &[u8]) -> Result<Vec<u8>, ByteCryptoError> {
    let version = detect_version(blob).ok_or(ByteCryptoError::UnknownVersion)?;
    if blob.len() < HEADER_LEN + SEED_LEN {
        return Err(ByteCryptoError::BadLength);
    }
    let seed = &blob[HEADER_LEN..HEADER_LEN + SEED_LEN];
    let payload = &blob[HEADER_LEN + SEED_LEN..];
    let (aes_key, iv) = derive_key_iv(seed, version);
    let mut buf = payload.to_vec();
    let inner = Aes128CbcDec::new(&aes_key.into(), &iv.into())
        .decrypt_padded_mut::<Pkcs7>(&mut buf)
        .map_err(|_| ByteCryptoError::BadPadding)?;
    if inner.len() < CHECK_LEN {
        return Err(ByteCryptoError::BadLength);
    }
    let (check, plain) = inner.split_at(CHECK_LEN);
    if sha512(plain) != check {
        return Err(ByteCryptoError::BadCheck);
    }
    Ok(plain.to_vec())
}

/// 加密 plain -> 原始 blob（AES 版本头，与 Trae 写入格式一致）
pub fn encrypt(plain: &[u8]) -> Vec<u8> {
    let mut seed = [0u8; SEED_LEN];
    rand::thread_rng().fill_bytes(&mut seed);
    let (aes_key, iv) = derive_key_iv(&seed, BlobVersion::Aes);
    let mut inner = Vec::with_capacity(CHECK_LEN + plain.len());
    inner.extend_from_slice(&sha512(plain));
    inner.extend_from_slice(plain);
    let ct = Aes128CbcEnc::new(&aes_key.into(), &iv.into()).encrypt_padded_vec_mut::<Pkcs7>(&inner);
    let mut out = Vec::with_capacity(HEADER_LEN + SEED_LEN + ct.len());
    out.extend_from_slice(&MAGIC_AES);
    out.extend_from_slice(&seed);
    out.extend_from_slice(&ct);
    out
}

/// Em：UTF-8 字符串 -> base64 加密串
pub fn encrypt_string(s: &str) -> String {
    B64.encode(encrypt(s.as_bytes()))
}

/// Pv：base64 加密串 -> UTF-8 字符串
pub fn decrypt_string(b64: &str) -> Result<String, ByteCryptoError> {
    let blob = B64.decode(b64.trim()).map_err(|_| ByteCryptoError::BadLength)?;
    let plain = decrypt(&blob)?;
    String::from_utf8(plain).map_err(|_| ByteCryptoError::BadLength)
}
