//! 哈希、编码与对称加解密。口令模式与 OpenSSL `enc -md md5` / 常见前端加密库的口令格式兼容
//! （"Salted__" + 8 字节盐，EVP_BytesToKey 派生密钥与 IV）。
use std::io::Read;
use std::path::Path;

use anyhow::{anyhow, bail, Result};
use base64::Engine;
use cipher::{BlockDecryptMut, BlockEncryptMut, KeyInit, KeyIvInit, StreamCipher};
use cipher::block_padding::NoPadding;
use digest::Digest;

use super::Args;
use crate::i18n::{t, tf};

pub fn md5_text(s: &str) -> String {
    let h = hex::encode(md5::Md5::digest(s.as_bytes()));
    format!("{}: {h}\n{}: {}\n16 {}: {}\n16 {}: {}", t("tools.lower32"), t("tools.upper32"), h.to_uppercase(), t("tools.lower"), &h[8..24], t("tools.upper"), h[8..24].to_uppercase())
}

/// 分块读取文件计算哈希，内存占用固定（1 MiB 缓冲）。
pub fn file_hash(p: &Path, algo: &str) -> Result<String> {
    fn run<D: Digest>(p: &Path) -> Result<(String, u64)> {
        let mut f = std::fs::File::open(p)?;
        let mut h = D::new();
        let mut buf = vec![0u8; 1 << 20];
        let mut n = 0u64;
        loop { let k = f.read(&mut buf)?; if k == 0 { break; } n += k as u64; h.update(&buf[..k]); }
        Ok((hex::encode(h.finalize()), n))
    }
    let (h, n) = match algo { "sha1" => run::<sha1::Sha1>(p)?, "sha256" => run::<sha2::Sha256>(p)?, "sha512" => run::<sha2::Sha512>(p)?, _ => run::<md5::Md5>(p)? };
    let name = p.file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
    Ok(format!("{h}\n{}\n\n{}: {name}\n{}: {n}", h.to_uppercase(), t("tools.file_name"), t("tools.file_size")))
}

pub fn sha_all(s: &str, upper: bool) -> String {
    let b = s.as_bytes();
    let rows = [("SHA-1", hex::encode(sha1::Sha1::digest(b))), ("SHA-224", hex::encode(sha2::Sha224::digest(b))), ("SHA-256", hex::encode(sha2::Sha256::digest(b))),
        ("SHA-384", hex::encode(sha2::Sha384::digest(b))), ("SHA-512", hex::encode(sha2::Sha512::digest(b)))];
    rows.iter().map(|(k, v)| format!("{k:8} {}", if upper { v.to_uppercase() } else { v.clone() })).collect::<Vec<_>>().join("\n")
}

pub fn hmac(s: &str, key: &str, algo: &str) -> Result<String> {
    use hmac::{Hmac, Mac};
    macro_rules! mac { ($d:ty) => {{ let mut m = <Hmac<$d> as Mac>::new_from_slice(key.as_bytes()).map_err(|e| anyhow!(e.to_string()))?; m.update(s.as_bytes()); m.finalize().into_bytes().to_vec() }} }
    let out = match algo { "md5" => mac!(md5::Md5), "sha1" => mac!(sha1::Sha1), "sha224" => mac!(sha2::Sha224), "sha384" => mac!(sha2::Sha384), "sha512" => mac!(sha2::Sha512), _ => mac!(sha2::Sha256) };
    Ok(format!("Hex     {}\nHEX     {}\nBase64  {}", hex::encode(&out), hex::encode(&out).to_uppercase(), b64().encode(&out)))
}

fn b64() -> base64::engine::GeneralPurpose { base64::engine::general_purpose::STANDARD }

/// OpenSSL EVP_BytesToKey（MD5，1 轮）。
pub fn evp_bytes_to_key(pass: &[u8], salt: &[u8], key_len: usize, iv_len: usize) -> (Vec<u8>, Vec<u8>) {
    let mut out = vec![];
    let mut prev: Vec<u8> = vec![];
    while out.len() < key_len + iv_len {
        let mut h = md5::Md5::new();
        h.update(&prev); h.update(pass); h.update(salt);
        prev = h.finalize().to_vec();
        out.extend_from_slice(&prev);
    }
    (out[..key_len].to_vec(), out[key_len..key_len + iv_len].to_vec())
}

pub fn rc4_apply(key: &[u8], data: &mut [u8]) {
    let mut s: [u8; 256] = std::array::from_fn(|i| i as u8);
    let mut j = 0u8;
    for i in 0..256 { j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]); s.swap(i, j as usize); }
    let (mut i, mut j) = (0u8, 0u8);
    for b in data.iter_mut() {
        i = i.wrapping_add(1); j = j.wrapping_add(s[i as usize]); s.swap(i as usize, j as usize);
        *b ^= s[s[i as usize].wrapping_add(s[j as usize]) as usize];
    }
}

fn pad(mut d: Vec<u8>, bs: usize, how: &str) -> Vec<u8> {
    match how {
        "pkcs7" => { let n = bs - d.len() % bs; d.extend(std::iter::repeat_n(n as u8, n)); }
        "zero" => { if d.len() % bs != 0 { let n = bs - d.len() % bs; d.extend(std::iter::repeat_n(0, n)); } }
        _ => {}
    }
    d
}
fn unpad(mut d: Vec<u8>, how: &str) -> Result<Vec<u8>> {
    match how {
        "pkcs7" => {
            let n = *d.last().ok_or_else(|| anyhow!(t("tools.err_decrypt")))? as usize;
            if n == 0 || n > 16 || n > d.len() || !d[d.len() - n..].iter().all(|&b| b as usize == n) { bail!(t("tools.err_decrypt")); }
            d.truncate(d.len() - n);
        }
        "zero" => { while d.last() == Some(&0) { d.pop(); } }
        _ => {}
    }
    Ok(d)
}

fn parse_key(s: &str, mode: &str) -> Result<Vec<u8>> {
    if mode == "hex" { hex::decode(s.trim().replace(' ', "")).map_err(|_| anyhow!(t("tools.err_hex"))) } else { Ok(s.as_bytes().to_vec()) }
}

/// 分组模式加解密（数据已按需填充）。
fn block_crypt<C>(mode: &str, key: &[u8], iv: &[u8], data: &mut Vec<u8>, enc: bool) -> Result<()>
where C: cipher::BlockCipher + cipher::BlockEncrypt + cipher::BlockDecrypt + KeyInit + Clone {
    let bs = C::block_size();
    let bad_key = || anyhow!(tf("tools.err_key_len", &[("n", &key.len())]));
    let need_iv = || anyhow!(tf("tools.err_iv_len", &[("n", &bs)]));
    if mode != "ecb" && iv.len() != bs { return Err(need_iv()); }
    match mode {
        "ecb" | "cbc" => {
            if data.len() % bs != 0 { bail!(tf("tools.err_block", &[("n", &bs)])); }
            let n = data.len();
            if mode == "ecb" {
                if enc { ecb::Encryptor::<C>::new_from_slice(key).map_err(|_| bad_key())?.encrypt_padded_mut::<NoPadding>(data, n).map_err(|_| anyhow!(t("tools.err_decrypt")))?; }
                else { ecb::Decryptor::<C>::new_from_slice(key).map_err(|_| bad_key())?.decrypt_padded_mut::<NoPadding>(data).map_err(|_| anyhow!(t("tools.err_decrypt")))?; }
            } else if enc { cbc::Encryptor::<C>::new_from_slices(key, iv).map_err(|_| bad_key())?.encrypt_padded_mut::<NoPadding>(data, n).map_err(|_| anyhow!(t("tools.err_decrypt")))?; }
            else { cbc::Decryptor::<C>::new_from_slices(key, iv).map_err(|_| bad_key())?.decrypt_padded_mut::<NoPadding>(data).map_err(|_| anyhow!(t("tools.err_decrypt")))?; }
        }
        _ => bail!(tf("tools.err_mode", &[("m", &mode)])),
    }
    Ok(())
}

/// AES 的流式模式（CTR/CFB/OFB）。
fn aes_stream(mode: &str, key: &[u8], iv: &[u8], data: &mut [u8], enc: bool) -> Result<()> {
    use cipher::{AsyncStreamCipher};
    let bad_key = || anyhow!(tf("tools.err_key_len", &[("n", &key.len())]));
    if iv.len() != 16 { bail!(tf("tools.err_iv_len", &[("n", &16)])); }
    macro_rules! go { ($c:ty) => {{
        match mode {
            "ctr" => ctr::Ctr128BE::<$c>::new_from_slices(key, iv).map_err(|_| bad_key())?.apply_keystream(data),
            "ofb" => ofb::Ofb::<$c>::new_from_slices(key, iv).map_err(|_| bad_key())?.apply_keystream(data),
            _ => if enc { cfb_mode::Encryptor::<$c>::new_from_slices(key, iv).map_err(|_| bad_key())?.encrypt(data) } else { cfb_mode::Decryptor::<$c>::new_from_slices(key, iv).map_err(|_| bad_key())?.decrypt(data) },
        }
    }} }
    match key.len() { 16 => go!(aes::Aes128), 24 => go!(aes::Aes192), 32 => go!(aes::Aes256), _ => return Err(bad_key()) }
    Ok(())
}

/// AES / DES / 3DES / RC4 / Rabbit 统一入口。
pub fn cipher(algo: &str, a: &Args) -> Result<String> {
    let algo = if algo == "des" && a.get("algo") == "3des" { "3des" } else { algo };
    let enc = a.get("dir") != "dec";
    let keymode = a.get("keymode");
    let mode = if algo == "aes" || algo == "des" || algo == "3des" { a.get("mode") } else { "stream" };
    let padding = if matches!(algo, "rc4" | "rabbit") { "none" } else { a.get("pad") };
    let out_hex = a.get("enc") == "hex";
    let input = a.get("input");
    if a.get("key").is_empty() { bail!(t("tools.err_need_key")); }
    // (密钥长度, IV 长度, 分组大小) —— 口令派生用
    let (klen, ivlen, bs) = match algo { "aes" => (32, 16, 16), "des" => (8, 8, 8), "3des" => (24, 8, 8), "rc4" => (32, 0, 1), _ => (16, 8, 1) };
    // 准备数据、密钥与 IV
    let (mut data, key, iv, salt) = if enc {
        let (key, iv, salt) = if keymode == "pass" {
            let salt: [u8; 8] = rand::random();
            let (k, i) = evp_bytes_to_key(a.get("key").as_bytes(), &salt, klen, ivlen);
            (k, i, Some(salt))
        } else { (parse_key(a.get("key"), keymode)?, parse_key(a.get("iv"), keymode)?, None) };
        (pad(input.as_bytes().to_vec(), bs.max(1), if bs > 1 { padding } else { "none" }), key, iv, salt)
    } else {
        let raw: String = input.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = if out_hex { hex::decode(&raw).map_err(|_| anyhow!(t("tools.err_hex")))? } else { b64().decode(&raw).map_err(|_| anyhow!(t("tools.err_base64")))? };
        if keymode == "pass" {
            if bytes.len() < 16 || &bytes[..8] != b"Salted__" { bail!(t("tools.err_salted")); }
            let (k, i) = evp_bytes_to_key(a.get("key").as_bytes(), &bytes[8..16], klen, ivlen);
            (bytes[16..].to_vec(), k, i, None)
        } else { (bytes, parse_key(a.get("key"), keymode)?, parse_key(a.get("iv"), keymode)?, None) }
    };
    let bad_key = || anyhow!(tf("tools.err_key_len", &[("n", &key.len())]));
    match (algo, mode) {
        ("aes", "ecb" | "cbc") => match key.len() {
            16 => block_crypt::<aes::Aes128>(mode, &key, &iv, &mut data, enc)?, 24 => block_crypt::<aes::Aes192>(mode, &key, &iv, &mut data, enc)?,
            32 => block_crypt::<aes::Aes256>(mode, &key, &iv, &mut data, enc)?, _ => return Err(bad_key()),
        },
        ("aes", m) => {
            // 流式模式也沿用所选填充（与常见前端加密库默认行为一致）
            aes_stream(m, &key, &iv, &mut data, enc)?
        }
        ("des", _) => { if key.len() != 8 { return Err(bad_key()); } block_crypt::<des::Des>(mode, &key, &iv, &mut data, enc)? }
        ("3des", _) => match key.len() { 24 => block_crypt::<des::TdesEde3>(mode, &key, &iv, &mut data, enc)?, 16 => block_crypt::<des::TdesEde2>(mode, &key, &iv, &mut data, enc)?, _ => return Err(bad_key()) },
        ("rc4", _) => { if key.is_empty() || key.len() > 256 { return Err(bad_key()); } rc4_apply(&key, &mut data) }
        _ => {
            if key.len() != 16 { return Err(bad_key()); }
            if iv.is_empty() { <rabbit::RabbitKeyOnly as rabbit::cipher::KeyInit>::new_from_slice(&key).map_err(|_| bad_key())?.apply_keystream(&mut data); }
            else { if iv.len() != 8 { bail!(tf("tools.err_iv_len", &[("n", &8)])); } <rabbit::Rabbit as rabbit::cipher::KeyIvInit>::new_from_slices(&key, &iv).map_err(|_| bad_key())?.apply_keystream(&mut data); }
        }
    }
    if enc {
        let mut out = vec![];
        if let Some(s) = salt { out.extend_from_slice(b"Salted__"); out.extend_from_slice(&s); }
        out.extend_from_slice(&data);
        Ok(if out_hex { hex::encode(out) } else { b64().encode(out) })
    } else {
        let d = if bs > 1 { unpad(data, padding)? } else { data };
        String::from_utf8(d).map_err(|e| anyhow!(tf("tools.err_utf8_hex", &[("hex", &hex::encode(e.as_bytes()))])))
    }
}

pub fn base64(s: &str, decode: bool, url: bool) -> Result<String> {
    use base64::engine::general_purpose as gp;
    if decode {
        let raw: String = s.chars().filter(|c| !c.is_whitespace()).collect();
        let norm = raw.replace('-', "+").replace('_', "/");
        let norm = norm.trim_end_matches('=').to_string();
        let b = gp::STANDARD_NO_PAD.decode(&norm).map_err(|_| anyhow!(t("tools.err_base64")))?;
        let _ = url;
        return String::from_utf8(b).map_err(|e| anyhow!(tf("tools.err_utf8_hex", &[("hex", &hex::encode(e.as_bytes()))])));
    }
    Ok(if url { gp::URL_SAFE_NO_PAD.encode(s) } else { gp::STANDARD.encode(s) })
}

pub fn url(s: &str, dir: &str) -> Result<String> {
    use percent_encoding::{utf8_percent_encode, AsciiSet, NON_ALPHANUMERIC};
    // 与 encodeURIComponent / encodeURI 保持一致的保留字符
    const COMPONENT: &AsciiSet = &NON_ALPHANUMERIC.remove(b'-').remove(b'_').remove(b'.').remove(b'!').remove(b'~').remove(b'*').remove(b'\'').remove(b'(').remove(b')');
    const URI: &AsciiSet = &COMPONENT.remove(b';').remove(b',').remove(b'/').remove(b'?').remove(b':').remove(b'@').remove(b'&').remove(b'=').remove(b'+').remove(b'$').remove(b'#');
    Ok(match dir {
        "dec" => percent_encoding::percent_decode_str(&s.replace('+', " ")).decode_utf8().map_err(|_| anyhow!(t("tools.err_url")))?.into_owned(),
        "enc_uri" => utf8_percent_encode(s, URI).to_string(),
        _ => utf8_percent_encode(s, COMPONENT).to_string(),
    })
}

pub fn unicode(s: &str, decode: bool, ascii: bool) -> String {
    if decode {
        let re = regex::Regex::new(r"\\u\{([0-9a-fA-F]{1,6})\}|\\u([0-9a-fA-F]{4})|&#x([0-9a-fA-F]+);|&#(\d+);|\\x([0-9a-fA-F]{2})").unwrap();
        // 先把代理对合并
        let mut units: Vec<u16> = vec![];
        let mut out = String::new();
        let mut last = 0;
        let flush = |units: &mut Vec<u16>, out: &mut String| { if !units.is_empty() { out.push_str(&String::from_utf16_lossy(units)); units.clear(); } };
        for c in re.captures_iter(s) {
            let m = c.get(0).unwrap();
            if m.start() > last { flush(&mut units, &mut out); out.push_str(&s[last..m.start()]); }
            last = m.end();
            if let Some(h) = c.get(2) { units.push(u16::from_str_radix(h.as_str(), 16).unwrap()); continue; }
            flush(&mut units, &mut out);
            let cp = if let Some(h) = c.get(1).or(c.get(3)).or(c.get(5)) { u32::from_str_radix(h.as_str(), 16).ok() } else { c.get(4).and_then(|d| d.as_str().parse().ok()) };
            match cp.and_then(char::from_u32) { Some(ch) => out.push(ch), None => out.push_str(m.as_str()) }
        }
        flush(&mut units, &mut out);
        out.push_str(&s[last..]);
        return out;
    }
    let mut out = String::new();
    for ch in s.chars() {
        if ch.is_ascii() && !ascii { out.push(ch); continue; }
        let mut buf = [0u16; 2];
        for u in ch.encode_utf16(&mut buf) { out.push_str(&format!("\\u{:04x}", u)); }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    fn args(kv: &[(&str, &str)]) -> Args { let mut a = Args::default(); for (k, v) in kv { a.set(k, *v); } a }
    #[test]
    fn hashes() {
        assert!(md5_text("hello").contains("5d41402abc4b2a76b9719d911017c592"));
        assert!(md5_text("hello").contains("bc4b2a76b9719d91"));
        assert!(sha_all("abc", false).contains("ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"));
        assert!(hmac("The quick brown fox jumps over the lazy dog", "key", "sha256").unwrap().contains("f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8"));
        assert!(hmac("The quick brown fox jumps over the lazy dog", "key", "md5").unwrap().contains("80070713463e7749b90c2dc24911e275"));
        let p = std::env::temp_dir().join("shellax-tools-md5.txt");
        std::fs::write(&p, "hello").unwrap();
        assert!(file_hash(&p, "md5").unwrap().starts_with("5d41402abc4b2a76b9719d911017c592"));
        assert!(file_hash(&p, "sha256").unwrap().starts_with("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"));
        let _ = std::fs::remove_file(p);
    }
    #[test]
    fn evp() {
        // openssl enc -aes-256-cbc -md md5 -S 0001020304050607 -pass pass:secret -P
        let (k, iv) = evp_bytes_to_key(b"secret", &[0, 1, 2, 3, 4, 5, 6, 7], 32, 16);
        assert_eq!(k.len(), 32); assert_eq!(iv.len(), 16);
        let (k2, _) = evp_bytes_to_key(b"secret", &[0, 1, 2, 3, 4, 5, 6, 7], 32, 16);
        assert_eq!(k, k2);
    }
    #[test]
    fn aes_known_vectors() {
        // 原始密钥、CBC、PKCS7
        let e = cipher("aes", &args(&[("input", "hello world"), ("key", "1234567890123456"), ("iv", "abcdefghijklmnop"), ("dir", "enc"), ("keymode", "utf8"), ("mode", "cbc"), ("pad", "pkcs7"), ("enc", "base64")])).unwrap();
        let d = cipher("aes", &args(&[("input", &e), ("key", "1234567890123456"), ("iv", "abcdefghijklmnop"), ("dir", "dec"), ("keymode", "utf8"), ("mode", "cbc"), ("pad", "pkcs7"), ("enc", "base64")])).unwrap();
        assert_eq!(d, "hello world");
        // FIPS-197 AES-128 ECB 向量
        let v = cipher("aes", &args(&[("input", "00112233445566778899aabbccddeeff"), ("key", "000102030405060708090a0b0c0d0e0f"), ("dir", "enc"), ("keymode", "hex"), ("mode", "ecb"), ("pad", "none"), ("enc", "hex")]));
        // 输入按文本处理，这里只验证能跑通；向量用 hex 输入单独验证
        assert!(v.is_ok());
        let mut data = hex::decode("00112233445566778899aabbccddeeff").unwrap();
        block_crypt::<aes::Aes128>("ecb", &hex::decode("000102030405060708090a0b0c0d0e0f").unwrap(), &[], &mut data, true).unwrap();
        assert_eq!(hex::encode(&data), "69c4e0d86a7b0430d8cdb78070b4c55a");
    }
    #[test]
    fn roundtrips() {
        for (algo, extra) in [("aes", vec![("mode", "cbc")]), ("aes", vec![("mode", "ctr")]), ("aes", vec![("mode", "cfb")]), ("aes", vec![("mode", "ofb")]), ("aes", vec![("mode", "ecb")]),
                              ("des", vec![("algo", "des"), ("mode", "cbc")]), ("des", vec![("algo", "3des"), ("mode", "ecb")]), ("rc4", vec![]), ("rabbit", vec![])] {
            for enc in ["base64", "hex"] {
                let mut kv = vec![("input", "中文 secret ✓"), ("key", "pass phrase"), ("dir", "enc"), ("keymode", "pass"), ("pad", "pkcs7"), ("enc", enc)];
                kv.extend(extra.iter().cloned());
                let e = cipher(algo, &args(&kv)).unwrap();
                if enc == "base64" { assert!(e.starts_with("U2FsdGVkX1"), "{algo} {e}"); }
                kv[0] = ("input", &e); kv[2] = ("dir", "dec");
                assert_eq!(cipher(algo, &args(&kv)).unwrap(), "中文 secret ✓", "{algo} {extra:?}");
                kv[1] = ("key", "wrong");
                assert_ne!(cipher(algo, &args(&kv)).ok().as_deref(), Some("中文 secret ✓"));
            }
        }
    }
    #[test]
    fn cryptojs_passphrase_compat() {
        // 由 crypto-js 4.x 以口令 "pass" 加密 "hello 世界" 得到
        for (algo, sub, ct) in [("aes", "", "U2FsdGVkX19L37EffizYEMczry9e8xBMu5+gMCSiRLw="), ("des", "des", "U2FsdGVkX18CKFt8qMEJCOBG3Lm4eR+nqXrrfdCZMkE="),
                                ("des", "3des", "U2FsdGVkX19QOW19aAmiuvONquJwSeiqnnAogccJwnY="), ("rc4", "", "U2FsdGVkX18cVjzPRxTvjCSa2HIRIq9qt02Gfg=="),
                                ("rabbit", "", "U2FsdGVkX19zkTYCAA9nbKrouWxt3qYnl3GYOg==")] {
            let d = cipher(algo, &args(&[("input", ct), ("key", "pass"), ("dir", "dec"), ("keymode", "pass"), ("algo", sub), ("mode", "cbc"), ("pad", "pkcs7"), ("enc", "base64")]));
            assert_eq!(d.ok().as_deref(), Some("hello 世界"), "{algo}/{sub}");
        }
    }
    #[test]
    fn rc4_vector() {
        let mut d = b"Plaintext".to_vec();
        rc4_apply(b"Key", &mut d);
        assert_eq!(hex::encode(d), "bbf316e8d940af0ad3");
    }
    #[test]
    fn encodings() {
        assert_eq!(base64("你好", false, false).unwrap(), "5L2g5aW9");
        assert_eq!(base64("5L2g5aW9", true, false).unwrap(), "你好");
        assert_eq!(base64("??>", false, true).unwrap(), "Pz8-");
        assert_eq!(base64("Pz8-", true, false).unwrap(), "??>");
        assert!(base64("!!", true, false).is_err());
        assert_eq!(url("a b&c=中", "enc").unwrap(), "a%20b%26c%3D%E4%B8%AD");
        assert_eq!(url("https://x.y/a b?q=1", "enc_uri").unwrap(), "https://x.y/a%20b?q=1");
        assert_eq!(url("a%20b+c", "dec").unwrap(), "a b c");
        assert_eq!(unicode("中a😀", false, false), "\\u4e2da\\ud83d\\ude00");
        assert_eq!(unicode("\\u4e2da\\ud83d\\ude00 &#x4e2d; &#20013; \\u{1F600}", true, false), "中a😀 中 中 😀");
    }
}
