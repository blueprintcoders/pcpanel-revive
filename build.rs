//! Gives the exe its icon (Explorer, the taskbar, downloads). The icon is drawn by the same code the tray
//! uses, written into a Windows resource file, and handed to the linker; no resource compiler needed.
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

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
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
    let path = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("icon.res");
    std::fs::File::create(&path).unwrap().write_all(&res).unwrap();
    println!("cargo:rustc-link-arg-bins={}", path.display());
}
