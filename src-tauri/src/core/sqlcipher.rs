// Trae AI-Agent 对话库：SQLCipher 4 页编解码（AES-256-CBC + HMAC-SHA512，页 4096 / reserve 80）
use aes::Aes256;
use cbc::cipher::{block_padding::NoPadding, BlockDecryptMut, BlockEncryptMut, KeyIvInit};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha512;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

type Aes256CbcEnc = cbc::Encryptor<Aes256>;
type Aes256CbcDec = cbc::Decryptor<Aes256>;
type HmacSha512 = Hmac<Sha512>;

pub const PAGE: usize = 4096;
const SALT: usize = 16;
const IV: usize = 16;
const HMAC: usize = 64;
const RESERVE: usize = 80;
const SQLITE_HDR: [u8; 16] = *b"SQLite format 3\0";
const WAL_MAGIC_LE_CKSUM: u32 = 0x377f_0682;
const WAL_MAGIC_BE_CKSUM: u32 = 0x377f_0683;
const WAL_HDR: usize = 32;
const WAL_FRAME_HDR: usize = 24;

#[derive(Debug)]
pub enum CipherError {
    Io(std::io::Error),
    BadSize,
    BadKey,
    BadWal,
}

impl std::fmt::Display for CipherError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "{e}"),
            Self::BadSize => write!(f, "对话库大小不是页对齐"),
            Self::BadKey => write!(f, "对话库密钥无法解开当前文件"),
            Self::BadWal => write!(f, "WAL 格式无法识别"),
        }
    }
}

impl From<std::io::Error> for CipherError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e)
    }
}

fn mac_key(enc_key: &[u8; 32], salt: &[u8; 16]) -> [u8; 32] {
    let mut salt_x = [0u8; 16];
    for i in 0..16 {
        salt_x[i] = salt[i] ^ 0x3A;
    }
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha512>(enc_key, &salt_x, 2, &mut out);
    out
}

fn hmac_page(mk: &[u8; 32], payload: &[u8], pgno: u32) -> [u8; HMAC] {
    let mut mac = HmacSha512::new_from_slice(mk).expect("hmac key");
    mac.update(payload);
    mac.update(&pgno.to_le_bytes());
    let d = mac.finalize().into_bytes();
    let mut out = [0u8; HMAC];
    out.copy_from_slice(&d);
    out
}

fn aes_decrypt(key: &[u8; 32], iv: &[u8; 16], enc: &[u8]) -> Result<Vec<u8>, CipherError> {
    let mut buf = enc.to_vec();
    let pt = Aes256CbcDec::new(key.into(), iv.into())
        .decrypt_padded_mut::<NoPadding>(&mut buf)
        .map_err(|_| CipherError::BadKey)?;
    Ok(pt.to_vec())
}

fn aes_encrypt(key: &[u8; 32], iv: &[u8; 16], pt: &[u8]) -> Vec<u8> {
    Aes256CbcEnc::new(key.into(), iv.into()).encrypt_padded_vec_mut::<NoPadding>(pt)
}

fn split_page(raw: &[u8], pgno: u32) -> ([u8; 16], &[u8], [u8; HMAC]) {
    let iv_off = PAGE - RESERVE;
    let mut iv = [0u8; 16];
    iv.copy_from_slice(&raw[iv_off..iv_off + IV]);
    let mut hmac = [0u8; HMAC];
    hmac.copy_from_slice(&raw[iv_off + IV..iv_off + IV + HMAC]);
    let enc = if pgno == 1 {
        &raw[SALT..iv_off]
    } else {
        &raw[..iv_off]
    };
    (iv, enc, hmac)
}

fn decrypt_page(key: &[u8; 32], mk: &[u8; 32], raw: &[u8], pgno: u32) -> Result<[u8; PAGE], CipherError> {
    let (iv, enc, hmac) = split_page(raw, pgno);
    let mut payload = Vec::with_capacity(enc.len() + IV);
    payload.extend_from_slice(enc);
    payload.extend_from_slice(&iv);
    if hmac_page(mk, &payload, pgno) != hmac {
        return Err(CipherError::BadKey);
    }
    let pt = aes_decrypt(key, &iv, enc)?;
    let mut out = [0u8; PAGE];
    if pgno == 1 {
        out[..SALT].copy_from_slice(&SQLITE_HDR);
        out[SALT..PAGE - RESERVE].copy_from_slice(&pt);
    } else {
        out[..PAGE - RESERVE].copy_from_slice(&pt);
    }
    Ok(out)
}

fn encrypt_page(key: &[u8; 32], mk: &[u8; 32], salt: &[u8; 16], page: &[u8], pgno: u32) -> [u8; PAGE] {
    let mut iv = [0u8; 16];
    rand::thread_rng().fill_bytes(&mut iv);
    let pt = if pgno == 1 {
        &page[SALT..PAGE - RESERVE]
    } else {
        &page[..PAGE - RESERVE]
    };
    let ct = aes_encrypt(key, &iv, pt);
    let mut payload = Vec::with_capacity(ct.len() + IV);
    payload.extend_from_slice(&ct);
    payload.extend_from_slice(&iv);
    let hmac = hmac_page(mk, &payload, pgno);
    let mut out = [0u8; PAGE];
    if pgno == 1 {
        out[..SALT].copy_from_slice(salt);
        out[SALT..PAGE - RESERVE].copy_from_slice(&ct);
    } else {
        out[..PAGE - RESERVE].copy_from_slice(&ct);
    }
    let iv_off = PAGE - RESERVE;
    out[iv_off..iv_off + IV].copy_from_slice(&iv);
    out[iv_off + IV..].copy_from_slice(&hmac);
    out
}

/// 用页 1 HMAC 验证 raw key 是否匹配该库
pub fn verify_key(enc_path: &Path, key: &[u8; 32]) -> Result<[u8; 16], CipherError> {
    let mut f = File::open(enc_path)?;
    let mut page = [0u8; PAGE];
    if f.read(&mut page)? != PAGE {
        return Err(CipherError::BadSize);
    }
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&page[..SALT]);
    let mk = mac_key(key, &salt);
    decrypt_page(key, &mk, &page, 1)?;
    Ok(salt)
}

pub fn read_salt(enc_path: &Path) -> Result<[u8; 16], CipherError> {
    let mut f = File::open(enc_path)?;
    let mut salt = [0u8; 16];
    f.read_exact(&mut salt)?;
    Ok(salt)
}

pub fn decrypt_file(src: &Path, dst: &Path, key: &[u8; 32]) -> Result<[u8; 16], CipherError> {
    let meta = std::fs::metadata(src)?;
    let size = meta.len() as usize;
    if size < PAGE || size % PAGE != 0 {
        return Err(CipherError::BadSize);
    }
    let npage = size / PAGE;
    let mut fin = File::open(src)?;
    let mut fout = File::create(dst)?;
    let mut raw = [0u8; PAGE];
    fin.read_exact(&mut raw)?;
    let mut salt = [0u8; 16];
    salt.copy_from_slice(&raw[..SALT]);
    let mk = mac_key(key, &salt);
    let p1 = decrypt_page(key, &mk, &raw, 1)?;
    fout.write_all(&p1)?;
    for pgno in 2..=npage {
        fin.read_exact(&mut raw)?;
        let plain = decrypt_page(key, &mk, &raw, pgno as u32)?;
        fout.write_all(&plain)?;
    }
    fout.flush()?;
    Ok(salt)
}

pub fn encrypt_file(src: &Path, dst: &Path, key: &[u8; 32], salt: &[u8; 16]) -> Result<(), CipherError> {
    let meta = std::fs::metadata(src)?;
    let size = meta.len() as usize;
    if size < PAGE || size % PAGE != 0 {
        return Err(CipherError::BadSize);
    }
    let npage = size / PAGE;
    let mk = mac_key(key, salt);
    let mut fin = File::open(src)?;
    let mut fout = File::create(dst)?;
    let mut page = [0u8; PAGE];
    for pgno in 1..=npage {
        fin.read_exact(&mut page)?;
        let enc = encrypt_page(key, &mk, salt, &page, pgno as u32);
        fout.write_all(&enc)?;
    }
    fout.flush()?;
    Ok(())
}

fn be_u32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// 把 SQLCipher WAL 已提交帧合并进明文库。返回合并的提交帧数。
pub fn apply_wal(plain: &Path, wal: &Path, key: &[u8; 32], salt: &[u8; 16]) -> Result<u32, CipherError> {
    if !wal.exists() {
        return Ok(0);
    }
    let bytes = std::fs::read(wal)?;
    if bytes.len() < WAL_HDR {
        return Ok(0);
    }
    let magic = be_u32(&bytes[0..4]);
    if magic != WAL_MAGIC_LE_CKSUM && magic != WAL_MAGIC_BE_CKSUM {
        return Err(CipherError::BadWal);
    }
    let page_sz = be_u32(&bytes[8..12]) as usize;
    if page_sz != PAGE {
        return Err(CipherError::BadWal);
    }
    let frame_sz = WAL_FRAME_HDR + PAGE;
    if bytes.len() < WAL_HDR + frame_sz {
        return Ok(0);
    }
    let n = (bytes.len() - WAL_HDR) / frame_sz;
    let mut last_commit: Option<(usize, u32)> = None;
    for i in 0..n {
        let off = WAL_HDR + i * frame_sz;
        let dbsize = be_u32(&bytes[off + 4..off + 8]);
        if dbsize != 0 {
            last_commit = Some((i, dbsize));
        }
    }
    let Some((last_i, dbsize)) = last_commit else {
        return Ok(0);
    };
    let mk = mac_key(key, salt);
    let mut f = std::fs::OpenOptions::new().read(true).write(true).open(plain)?;
    let mut commits = 0u32;
    for i in 0..=last_i {
        let off = WAL_HDR + i * frame_sz;
        let pgno = be_u32(&bytes[off..off + 4]);
        if pgno == 0 {
            continue;
        }
        let raw = &bytes[off + WAL_FRAME_HDR..off + WAL_FRAME_HDR + PAGE];
        let page = decrypt_page(key, &mk, raw, pgno)?;
        let pos = (pgno as u64 - 1) * PAGE as u64;
        let need = pos + PAGE as u64;
        let cur = f.metadata()?.len();
        if cur < need {
            f.set_len(need)?;
        }
        f.seek(SeekFrom::Start(pos))?;
        f.write_all(&page)?;
        if be_u32(&bytes[off + 4..off + 8]) != 0 {
            commits += 1;
        }
    }
    let final_len = dbsize as u64 * PAGE as u64;
    f.set_len(final_len)?;
    // sqlite 头：offset 28 是页数（BE）
    f.seek(SeekFrom::Start(28))?;
    f.write_all(&dbsize.to_be_bytes())?;
    f.flush()?;
    Ok(commits)
}
