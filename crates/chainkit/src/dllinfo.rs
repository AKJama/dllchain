//! What a DLL is, read from its bytes only: nothing is loaded or run.

use crate::game::{Game, Loader};
use crate::hash::sha256_hex;
use crate::paths::slug;
use crate::{Result, fail};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WordFrom {
    /// A text from the game's tool table.
    Known,
    /// The DLL's version information (its product name or description).
    VersionInfo,
}

#[derive(Clone, Debug)]
pub struct DllInfo {
    pub sha256: String,
    pub size: u64,
    /// Its file-name word and where it came from, when the DLL says who it is.
    pub word: Option<(String, WordFrom)>,
    pub product_name: Option<String>,
    /// Its version, from its version information (`ProductVersion`, else `FileVersion`).
    pub version: Option<String>,
    /// It reads the chain file, so it can start the others when it is the host.
    pub can_host: bool,
    /// A loader that only loads mods (EMF's, XHL-Core, ModLoader Proxy): it hosts, and is never a mod in the chain.
    pub loader: Option<&'static Loader>,
}

/// Checks that `bytes` is a 64-bit Windows DLL and says what it is.
pub fn inspect(bytes: &[u8], game: &Game) -> Result<DllInfo> {
    check_x64_dll(bytes)?;
    let product_name = version_string(bytes, "ProductName").or_else(|| version_string(bytes, "FileDescription"));
    let known = game.tools.iter().find(|t| t.texts.iter().any(|x| contains_text(bytes, x)));
    let loader = game.loaders.iter().find(|l| {
        // the free test first: a known tool's DLL is scanned only for loaders whose texts are their own
        (l.own_texts || known.is_none_or(|t| t.word == l.word))
            && l.texts.iter().any(|group| group.iter().all(|t| contains_text(bytes, t)))
    });
    let word = if let Some(l) = loader {
        Some((l.word.to_string(), WordFrom::Known))
    } else if let Some(t) = known {
        Some((t.word.to_string(), WordFrom::Known))
    } else {
        product_name
            .as_deref()
            .filter(|n| !n.contains("Microsoft"))
            .map(|n| slug(n.trim_end_matches(".dll")))
            .filter(|w| !w.is_empty() && !game.proxy_names.contains(&w.as_str()))
            .map(|w| (w, WordFrom::VersionInfo))
    };
    Ok(DllInfo {
        sha256: sha256_hex(bytes),
        size: bytes.len() as u64,
        word,
        product_name,
        version: version_string(bytes, "ProductVersion").or_else(|| version_string(bytes, "FileVersion")),
        can_host: loader.is_some() || game.host_texts.iter().any(|t| contains_text(bytes, t)),
        loader,
    })
}

/// "MZ", a PE header, a DLL, for x64: anything else is refused before it is copied anywhere.
fn check_x64_dll(b: &[u8]) -> Result<()> {
    if b.len() < 0x40 || &b[..2] != b"MZ" {
        fail!("not a Windows DLL (no MZ header)");
    }
    let pe = u32::from_le_bytes([b[0x3c], b[0x3d], b[0x3e], b[0x3f]]) as usize;
    if pe.checked_add(24).is_none_or(|end| end > b.len()) || &b[pe..pe + 4] != b"PE\0\0" {
        fail!("not a Windows DLL (no PE header)");
    }
    let machine = u16::from_le_bytes([b[pe + 4], b[pe + 5]]);
    let characteristics = u16::from_le_bytes([b[pe + 22], b[pe + 23]]);
    if characteristics & 0x2000 == 0 {
        fail!("a program, not a DLL");
    }
    if machine != 0x8664 {
        fail!("a DLL for another processor (not 64-bit x86), which the game cannot load");
    }
    Ok(())
}

/// `text` in `bytes`, as ASCII or as UTF-16LE, ignoring ASCII case.
pub fn contains_text(bytes: &[u8], text: &str) -> bool {
    let narrow: Vec<u8> = text.bytes().map(|b| b.to_ascii_lowercase()).collect();
    let wide: Vec<u8> = narrow.iter().flat_map(|&b| [b, 0]).collect();
    find_ci(bytes, &narrow) || find_ci(bytes, &wide)
}

fn find_ci(hay: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || hay.len() < needle.len() {
        return needle.is_empty();
    }
    let first = needle[0];
    (0..=hay.len() - needle.len()).any(|i| {
        hay[i].to_ascii_lowercase() == first
            && hay[i..i + needle.len()].iter().zip(needle).all(|(a, b)| a.to_ascii_lowercase() == *b)
    })
}

/// A string from the DLL's version information (`VS_VERSIONINFO`), found by its key in UTF-16. Each `String` there is
/// `wLength, wValueLength, wType = 1`, the key with a 0 after it, padding to 4 bytes, then the value.
fn version_string(b: &[u8], key: &str) -> Option<String> {
    let mut pattern: Vec<u8> = key.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    pattern.extend_from_slice(&[0, 0]);
    let mut from = 6;
    while from + pattern.len() <= b.len() {
        let at = from + b[from..].windows(pattern.len()).position(|w| w == pattern.as_slice())?;
        from = at + 2;
        let value_words = u16::from_le_bytes([b[at - 4], b[at - 3]]) as usize;
        let kind = u16::from_le_bytes([b[at - 2], b[at - 1]]);
        let start = at - 6; // the String's own start, which is 4-byte aligned
        if kind != 1 || value_words == 0 || value_words > 256 || start % 4 != 0 {
            continue;
        }
        let mut v = at + pattern.len();
        v = start + (v - start).div_ceil(4) * 4;
        let end = v.checked_add(value_words * 2)?;
        if end > b.len() {
            continue;
        }
        let units: Vec<u16> =
            b[v..end].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).take_while(|&u| u != 0).collect();
        let s = String::from_utf16_lossy(&units).trim().to_string();
        if !s.is_empty() {
            return Some(s);
        }
    }
    None
}

/// Bytes that pass [`inspect`]'s DLL check, holding `texts` (ASCII) and an optional product name. For tests here and
/// in the crates that use chainkit; nothing in it can run.
pub fn fake_dll(texts: &[&str], product_name: Option<&str>) -> Vec<u8> {
    let info: Vec<(&str, &str)> = product_name.map(|n| ("ProductName", n)).into_iter().collect();
    fake_dll_with(texts, &info)
}

/// [`fake_dll`] with any version information strings: (key, value).
pub fn fake_dll_with(texts: &[&str], info: &[(&str, &str)]) -> Vec<u8> {
    let mut b = vec![0u8; 0x80];
    b[0] = b'M';
    b[1] = b'Z';
    b[0x3c] = 0x40;
    b[0x40..0x44].copy_from_slice(b"PE\0\0");
    b[0x44..0x46].copy_from_slice(&0x8664u16.to_le_bytes());
    b[0x56..0x58].copy_from_slice(&0x2022u16.to_le_bytes());
    for t in texts {
        b.extend_from_slice(t.as_bytes());
        b.push(0);
    }
    for (k, v) in info {
        while b.len() % 4 != 0 {
            b.push(0);
        }
        let key: Vec<u8> = format!("{k}\0").encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let value: Vec<u8> = format!("{v}\0").encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
        let mut s = Vec::new();
        s.extend_from_slice(&0u16.to_le_bytes()); // wLength, not read
        s.extend_from_slice(&((value.len() / 2) as u16).to_le_bytes());
        s.extend_from_slice(&1u16.to_le_bytes());
        s.extend_from_slice(&key);
        while s.len() % 4 != 0 {
            s.push(0);
        }
        s.extend_from_slice(&value);
        b.extend_from_slice(&s);
    }
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::ENSHROUDED;

    #[test]
    fn recognises_tools_and_hosts() {
        let alp = inspect(&fake_dll(&["x", "AUTO-LOOT-PLUS.log", "dinput8-chain.ini"], None), &ENSHROUDED).unwrap();
        assert_eq!(alp.word, Some(("auto-loot-plus".into(), WordFrom::Known)));
        assert!(alp.can_host && alp.loader.is_none());

        let ens = inspect(&fake_dll(&["enstash Quick Stack placeholders %s"], None), &ENSHROUDED).unwrap();
        assert_eq!(ens.word.unwrap().0, "enstash");
        assert!(!ens.can_host);

        let emf = inspect(&fake_dll(&["EMF-LOADER-7d3c9f"], None), &ENSHROUDED).unwrap();
        assert!(emf.loader.is_some_and(|l| l.word == "emf") && emf.can_host);
        // EMF names the mods it knows, and is still EMF
        let emf2 = inspect(&fake_dll(&["emf_probe", "shroudforge"], None), &ENSHROUDED).unwrap();
        assert_eq!(emf2.word.unwrap().0, "emf");

        // a DLL that says XHL is a mod; one that also reads the chain file is XHL-Core, the loader
        let plain = inspect(&fake_dll(&[], Some("XHL Core")), &ENSHROUDED).unwrap();
        assert_eq!(plain.word, Some(("xhl-core".into(), WordFrom::VersionInfo)));
        assert_eq!(plain.product_name.as_deref(), Some("XHL Core"));
        assert!(plain.loader.is_none() && !plain.can_host);
        let xhl = inspect(&fake_dll(&["dinput8-chain.ini"], Some("XHL-Core")), &ENSHROUDED).unwrap();
        assert_eq!(xhl.loader.map(|l| l.name), Some("XHL-Core"));
        assert!(xhl.can_host);
        // a mod dllchain knows that names XHL-Core (as a loader it works with) stays that mod
        let alp_xhl = inspect(&fake_dll(&["auto-loot-plus", "dinput8-chain.ini", "XHL-Core"], None), &ENSHROUDED);
        assert!(alp_xhl.unwrap().loader.is_none());

        // ModLoader Proxy, by its log's name and the ini it reads (both UTF-16 in its DLL)
        let utf16 = |t: &str| t.encode_utf16().flat_map(|u| u.to_le_bytes()).collect::<Vec<u8>>();
        let mut mlp = fake_dll(&[], None);
        mlp.extend(utf16("chainloader.ini"));
        mlp.extend(utf16("modloader-proxy.log"));
        let mlp = inspect(&mlp, &ENSHROUDED).unwrap();
        assert_eq!(mlp.loader.map(|l| l.list), Some("chainloader.ini"));
        assert_eq!(mlp.word.unwrap().0, "modloader-proxy");
        // a mod that only mentions chainloader.ini, or says "XHL" without XHL-Core, stays a mod
        let alp_mlp = inspect(&fake_dll(&["auto-loot-plus", "chainloader.ini"], None), &ENSHROUDED).unwrap();
        assert!(alp_mlp.loader.is_none());
        let some_xhl = inspect(&fake_dll(&["dinput8-chain.ini", "xhl"], Some("Shiny Mod")), &ENSHROUDED).unwrap();
        assert!(some_xhl.loader.is_none() && some_xhl.can_host);

        let ms = inspect(&fake_dll(&[], Some("Microsoft Windows Operating System")), &ENSHROUDED).unwrap();
        assert_eq!(ms.word, None);
    }

    #[test]
    fn version_from_version_info() {
        let v = |info: &[(&str, &str)]| inspect(&fake_dll_with(&[], info), &ENSHROUDED).unwrap().version;
        assert_eq!(v(&[("ProductVersion", "0.12.2"), ("FileVersion", "0.12.2.0")]).as_deref(), Some("0.12.2"));
        assert_eq!(v(&[("FileVersion", "1.1.8.0")]).as_deref(), Some("1.1.8.0"));
        assert_eq!(v(&[]), None);
    }

    #[test]
    fn wide_text_counts() {
        let mut b = fake_dll(&[], None);
        b.extend("Dinput8-Chain.INI".encode_utf16().flat_map(|u| u.to_le_bytes()));
        assert!(inspect(&b, &ENSHROUDED).unwrap().can_host);
    }

    #[test]
    fn refuses_what_is_not_an_x64_dll() {
        assert!(inspect(b"hello", &ENSHROUDED).is_err());
        let mut exe = fake_dll(&[], None);
        exe[0x56] = 0x22;
        exe[0x57] = 0x00;
        assert!(inspect(&exe, &ENSHROUDED).unwrap_err().0.contains("program"));
        let mut x86 = fake_dll(&[], None);
        x86[0x44..0x46].copy_from_slice(&0x014cu16.to_le_bytes());
        assert!(inspect(&x86, &ENSHROUDED).is_err());
        let mut short = fake_dll(&[], None);
        short[0x3c] = 0xff;
        assert!(inspect(&short, &ENSHROUDED).is_err());
    }
}
