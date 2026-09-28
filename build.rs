//! Gives the exe its icon (Explorer, the taskbar, downloads) and its version details (Properties > Details,
//! which code signing checks). The icon is drawn by the same code the tray uses; both are written into a
//! Windows resource file and handed to the linker, so no resource compiler is needed.
use std::io::Write;

#[allow(dead_code)]
mod icon {
    include!("src/icon.rs");
}

const SIZES: [usize; 9] = [16, 20, 24, 32, 40, 48, 64, 128, 256];

/// One size as an icon image: a 32-bit bitmap (twice as tall, for the unused AND mask), rows bottom-up in BGRA.
fn icon_image(n: usize) -> Vec<u8> {
    let rgba = icon::rgba(n);
    let mask_row = n.div_ceil(32) * 4;
    let mut out = Vec::new();
    for v in [40u32, n as u32, 2 * n as u32] {
        out.extend(v.to_le_bytes());
    }
    out.extend(1u16.to_le_bytes()); // planes
    out.extend(32u16.to_le_bytes()); // bits per pixel
    out.extend([0u8; 24]); // no compression, sizes and palette unused
    for y in (0..n).rev() {
        for p in rgba[y * n * 4..(y + 1) * n * 4].chunks(4) {
            out.extend([p[2], p[1], p[0], p[3]]);
        }
    }
    out.extend(vec![0u8; mask_row * n]);
    out
}

/// A resource entry: header (type and name as numeric ids) and data, each padded to 4 bytes.
fn resource(out: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]) {
    let mut header = Vec::new();
    header.extend((data.len() as u32).to_le_bytes());
    header.extend(32u32.to_le_bytes()); // header size
    header.extend([0xff, 0xff]);
    header.extend(kind.to_le_bytes());
    header.extend([0xff, 0xff]);
    header.extend(id.to_le_bytes());
    header.extend(0u32.to_le_bytes()); // data version
    header.extend(flags.to_le_bytes());
    header.extend(0x0409u16.to_le_bytes()); // English (US)
    header.extend([0u8; 8]); // version, characteristics
    out.extend(header);
    out.extend(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

fn utf16z(s: &str) -> Vec<u8> {
    s.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect()
}

fn pad4(b: &mut Vec<u8>) {
    while b.len() % 4 != 0 {
        b.push(0);
    }
}

/// One node of a version resource: length, value length, type (1 = text), key, value, then children.
fn version_node(key: &str, value: &[u8], text: bool, children: &[Vec<u8>]) -> Vec<u8> {
    let mut b = vec![0, 0];
    let value_len = if text { value.len() / 2 } else { value.len() };
    b.extend((value_len as u16).to_le_bytes());
    b.extend((text as u16).to_le_bytes());
    b.extend(utf16z(key));
    pad4(&mut b);
    b.extend(value);
    for child in children {
        pad4(&mut b);
        b.extend(child);
    }
    let len = b.len() as u16;
    b[..2].copy_from_slice(&len.to_le_bytes());
    b
}

/// The version resource: numbers from Cargo.toml, and the names shown in the exe's Properties.
fn version_info() -> Vec<u8> {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    let part = |name: &str| std::env::var(name).unwrap().parse::<u32>().unwrap();
    let (major, minor, patch) = (part("CARGO_PKG_VERSION_MAJOR"), part("CARGO_PKG_VERSION_MINOR"), part("CARGO_PKG_VERSION_PATCH"));
    let mut fixed = Vec::new();
    for v in [0xfeef04bd, 0x0001_0000, major << 16 | minor, patch << 16, major << 16 | minor, patch << 16, 0x3f, 0, 0x0004_0004, 1, 0, 0, 0u32] {
        fixed.extend(v.to_le_bytes()); // signature, struct version, file and product version, flags, NT, app
    }
    let strings: Vec<Vec<u8>> = [
        ("FileDescription", "PCPanel Revive"),
        ("FileVersion", version.as_str()),
        ("InternalName", "pcpanel-revive"),
        ("LegalCopyright", "GPL-3.0-or-later"),
        ("OriginalFilename", "pcpanel-revive.exe"),
        ("ProductName", "PCPanel Revive"),
        ("ProductVersion", version.as_str()),
    ].iter().map(|(k, v)| version_node(k, &utf16z(v), true, &[])).collect();
    let table = version_node("040904b0", &[], true, &strings); // English (US), Unicode
    let string_info = version_node("StringFileInfo", &[], true, &[table]);
    let translation = version_node("Translation", &0x04b0_0409u32.to_le_bytes(), false, &[]);
    let var_info = version_node("VarFileInfo", &[], true, &[translation]);
    version_node("VS_VERSION_INFO", &fixed, false, &[string_info, var_info])
}

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=Cargo.toml"); // the version
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }
    const RT_ICON: u16 = 3;
    const RT_GROUP_ICON: u16 = 14;
    // A .res file starts with an empty entry.
    let mut res = vec![0, 0, 0, 0, 0x20, 0, 0, 0, 0xff, 0xff, 0, 0, 0xff, 0xff, 0, 0];
    res.extend([0u8; 16]);
    let mut group = Vec::new();
    group.extend([0, 0, 1, 0]); // reserved, type 1 = icon
    group.extend((SIZES.len() as u16).to_le_bytes());
    for (i, &n) in SIZES.iter().enumerate() {
        let image = icon_image(n);
        resource(&mut res, RT_ICON, i as u16 + 1, 0x1010, &image);
        let side = if n >= 256 { 0 } else { n as u8 }; // 0 means 256
        group.extend([side, side, 0, 0]);
        group.extend(1u16.to_le_bytes());
        group.extend(32u16.to_le_bytes());
        group.extend((image.len() as u32).to_le_bytes());
        group.extend((i as u16 + 1).to_le_bytes());
    }
    // Explorer uses the first icon group in the exe.
    resource(&mut res, RT_GROUP_ICON, 1, 0x1030, &group);
    const RT_VERSION: u16 = 16;
    resource(&mut res, RT_VERSION, 1, 0x0030, &version_info());
    let path = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("icon.res");
    std::fs::File::create(&path).unwrap().write_all(&res).unwrap();
    println!("cargo:rustc-link-arg-bins={}", path.display());
}
