use std::{env, fs, io, path::PathBuf};

const ICON_PATH: &str = "assets/app-icon.ico";
const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;

fn main() -> io::Result<()> {
    println!("cargo:rerun-if-changed=build.rs");
    if !env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "windows") {
        return Ok(());
    }

    println!("cargo:rerun-if-changed={ICON_PATH}");
    let icon = fs::read(ICON_PATH)?;
    let resource =
        PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR")).join("app-icon.res");
    fs::write(&resource, icon_resource(&icon)?)?;
    println!(
        "cargo:rustc-link-arg-bin=tu-factura-ya-printer={}",
        resource.display()
    );
    Ok(())
}

fn icon_resource(icon: &[u8]) -> io::Result<Vec<u8>> {
    let header = icon
        .get(..6)
        .filter(|header| header[..4] == [0, 0, 1, 0])
        .ok_or_else(invalid_icon)?;
    let count = u16::from_le_bytes([header[4], header[5]]);
    let directory = icon
        .get(6..6 + usize::from(count) * 16)
        .filter(|directory| !directory.is_empty())
        .ok_or_else(invalid_icon)?;

    let mut resource = Vec::new();
    append_record(&mut resource, 0, 0, &[]);
    let mut group = header.to_vec();
    for (id, entry) in (1..=count).zip(directory.chunks_exact(16)) {
        let size = u32::from_le_bytes(entry[8..12].try_into().unwrap()) as usize;
        let offset = u32::from_le_bytes(entry[12..16].try_into().unwrap()) as usize;
        let end = offset.checked_add(size).ok_or_else(invalid_icon)?;
        let image = icon
            .get(offset..end)
            .filter(|image| !image.is_empty())
            .ok_or_else(invalid_icon)?;
        append_record(&mut resource, RT_ICON, id, image);
        // A group entry replaces the ICO file offset with the image resource ID.
        group.extend_from_slice(&entry[..12]);
        group.extend_from_slice(&id.to_le_bytes());
    }
    append_record(&mut resource, RT_GROUP_ICON, 1, &group);
    Ok(resource)
}

fn invalid_icon() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("Invalid ICO file: {ICON_PATH}"),
    )
}

fn append_record(resource: &mut Vec<u8>, kind: u16, id: u16, data: &[u8]) {
    // The numeric type and name each use 0xffff followed by a 16-bit ordinal.
    resource.extend_from_slice(&(data.len() as u32).to_le_bytes());
    resource.extend_from_slice(&32_u32.to_le_bytes());
    for ordinal in [kind, id] {
        resource.extend_from_slice(&u16::MAX.to_le_bytes());
        resource.extend_from_slice(&ordinal.to_le_bytes());
    }
    // Data version, memory flags, language ID, version, and characteristics.
    resource.extend_from_slice(&[0; 16]);
    resource.extend_from_slice(data);
    resource.resize(resource.len().next_multiple_of(4), 0);
}
