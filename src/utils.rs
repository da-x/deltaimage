use std::ffi::OsString;
use std::fs::File;
use std::io::{Write, BufWriter};
use std::path::{PathBuf, Path};
use std::os::unix::prelude::{PermissionsExt, MetadataExt};
use std::time::SystemTime;
use anyhow::Context;
use nix::unistd::{Uid, Gid};
use serde::{de::DeserializeOwned, Serialize};

pub fn drop_components(nr: usize, path: &Path) -> PathBuf {
    path.components()
        .skip(nr)
        .fold(PathBuf::new(), |mut path_buf, comp| {
            path_buf.push(comp);
            path_buf
        })
}

type MetaData = (SystemTime, u32, u32, u32, Vec<(OsString, Vec<u8>)>, u64, u64);

pub fn get_meta_data(target_path: &Path) -> anyhow::Result<MetaData> {
    let meta_data = std::fs::metadata(&target_path)?;
    let modified = meta_data.modified()?;
    let mode = meta_data.permissions().mode();
    let uid = meta_data.uid();
    let gid = meta_data.gid();
    let ino = meta_data.ino();
    let dev = meta_data.dev();
    let mut xattrs = vec![];

    match xattr::list(target_path) {
        Ok(attributes) => {
            for attribute in attributes {
                if let Some(value) = xattr::get(target_path, &attribute)? {
                    xattrs.push((attribute, value));
                }
            }
        },
        _ => {}
    }

    return Ok((modified, mode, uid, gid, xattrs, ino, dev));
}

pub fn set_meta_data(target_path: &Path, meta_data: MetaData) -> anyhow::Result<()> {
    let (modified, mode, uid, gid, xattrs, _, _) = meta_data;

    nix::unistd::chown(target_path, Some(Uid::from_raw(uid)), Some(Gid::from_raw(gid)))
        .with_context(|| format!("failed to chown"))?;

    let mtime = filetime::FileTime::from_system_time(modified);
    filetime::set_file_times(target_path, mtime, mtime).map_err(|e| {
        crate::Error::FileTimeError(e, target_path.to_owned())
    }).with_context(|| format!("failed to set file time"))?;

    for (key, value) in xattrs {
        xattr::set(&target_path, key, value.as_slice())
            .with_context(|| format!("failed to set xattr"))?;
    }

    let perm = std::fs::Permissions::from_mode(mode);
    std::fs::set_permissions(&target_path, perm)
        .with_context(|| format!("failed to set permissions"))?;

    Ok(())
}


pub fn serialize_to_json<T>(data: &T, filename: &Path) -> anyhow::Result<()>
    where T: Serialize
{
    let json = serde_json::to_string(data).context("Failed to serialize data")?;
    let mut file = BufWriter::new(File::create(filename)
        .with_context(|| format!("Failed to create file {}", filename.display()))?);
    file.write_all(json.as_bytes())
        .with_context(|| format!("Failed to write to file {}", filename.display()))?;

    Ok(())
}

pub fn deserialize_from_json<T>(filename: &Path) -> anyhow::Result<T>
    where T: DeserializeOwned
{
    let file = File::open(filename).with_context(|| format!("Failed to open file {}", filename.display()))?;
    let data = serde_json::from_reader(file).context("Failed to deserialize data")?;
    Ok(data)
}
