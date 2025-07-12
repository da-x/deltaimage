mod cmdline;
mod utils;

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::os::unix::prelude::{OsStrExt, MetadataExt};
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::Context;
use structopt::StructOpt;
use cmdline::Cmdline;
use thiserror::Error;
use utils::{drop_components, get_meta_data, set_meta_data, serialize_to_json, deserialize_from_json};
use walkdir::WalkDir;
use serde::{Serialize, Deserialize};

fn main() -> anyhow::Result<()> {
    let opt = Cmdline::from_args();
    match opt.command {
        cmdline::Command::Diff(info) => {
            diff(opt.debug, info)?;
        }
        cmdline::Command::Apply(info) => {
            apply(opt.debug, info)?;
        }
        cmdline::Command::DockerFile(df) => {
            docker_file(&df)?;
        },
    }

    Ok(())
}

fn docker_file(df: &cmdline::DockerFile) -> anyhow::Result<()> {
    let mut version = env!("CARGO_PKG_VERSION").to_owned();

    let override_version = match df {
        cmdline::DockerFile::Diff { override_version, .. } => {
            override_version
        },
        cmdline::DockerFile::Apply { override_version, .. } => {
            override_version
        },
    };

    if let Some(override_version) = override_version {
        version = override_version.to_owned();
    }

    match df {
        cmdline::DockerFile::Diff { image_a, image_b, unlinked, .. } => {
            let source = if *unlinked {
                "scratch"
            } else {
                image_a
            };

            println!(r#"
# Calculate delta under a temporary image
FROM scratch AS delta
COPY --from={image_a} / /source/
COPY --from={image_b} / /delta/
COPY --from=deltaimage/deltaimage:{version} /opt/deltaimage /opt/deltaimage
RUN ["/opt/deltaimage", "diff", "/source", "/delta"]

# Make the deltaimage
FROM {source}
COPY --from=delta /delta /__deltaimage__.delta
"#);
        },
        cmdline::DockerFile::Apply { delta_image, unlinked_source, .. } => {
            let copy_source = if let Some(unlinked_source) = unlinked_source {
                format!("COPY --from={unlinked_source} / /")
            } else {
                format!("")
            };

            println!(r#"
# Apply a delta under a temporary image
FROM {delta_image} AS applied
{copy_source}
COPY --from=deltaimage/deltaimage:{version} /opt/deltaimage /opt/deltaimage
USER root
RUN ["/opt/deltaimage", "apply", "/", "/__deltaimage__.delta"]

# Make the original image by applying the delta
FROM scratch
COPY --from=applied /__deltaimage__.delta/ /
"#);
        },
    }

    Ok(())
}

const DELTAIMAGE_META_FILE: &str = "__deltaimage.meta.json";

#[derive(Error, Debug)]
pub enum Error {
    #[error("XDelta3 encode error")]
    XDelta3EncodeError,

    #[error("XDelta3 decode error")]
    XDelta3DecodeError,

    #[error("XDelta3 failed validation: {0} -> {1}")]
    XDelta3FailedValidation(PathBuf, PathBuf),

    #[error("XDelta3 failed deflation: {0} -> {1}")]
    XDelta3FailedDeflation(PathBuf, PathBuf),

    #[error("File time error")]
    FileTimeError(std::io::Error, PathBuf),

    #[error("Output delta dir already exists: {0}")]
    DeltaDirExists(PathBuf),
}

#[derive(Serialize, Deserialize, Hash, Eq, PartialEq, Ord, PartialOrd)]
enum Algo {
    XDelta3,
    AsIs,
}

#[derive(Serialize, Deserialize)]
struct MetaData {
    version: String,
    keep_files: Vec<Vec<u8>>,
    changes: Vec<(Algo, Vec<u8>)>,
}

fn diff(debug: bool, info: cmdline::Diff) -> anyhow::Result<()> {
    let mut changes: Vec<_> = Vec::new();
    let mut keep_files: Vec<_> = Vec::new();
    let mut orig_files = BTreeSet::new();

    let n = info.source_dir.components().count();
    let mut total_size = 0u64;
    let mut reduced_size = 0u64;

    for entry in WalkDir::new(&info.source_dir) {
        let entry = entry?;
        let path = entry.path();
        let rel_path = drop_components(n, &path);

        if entry.file_type().is_file() {
            orig_files.insert(rel_path);
        }
    }

    let mut parent_modtime_save = HashMap::new();
    let mut fsid_link_groups = HashMap::new();
    let mut path_link_groups = HashMap::new();

    let n = info.target_delta_dir.components().count();
    for entry in WalkDir::new(&info.target_delta_dir) {
        let entry = entry?;
        let path = entry.path();
        let rel_path = drop_components(n, &path);

        if entry.file_type().is_file() {
            let metadata = entry.metadata()?;
            let fsid = (metadata.ino(), metadata.dev());
            if metadata.nlink() >= 2 {
                use std::collections::hash_map;
                let item = match fsid_link_groups.entry(fsid) {
                    hash_map::Entry::Vacant(v) => v.insert(Rc::new(RefCell::new(None::<PathBuf>))),
                    hash_map::Entry::Occupied(o) => o.into_mut(),
                };
                path_link_groups.insert(rel_path, item.clone());
            }
        }
    }

    for entry in WalkDir::new(&info.target_delta_dir) {
        let entry = entry?;
        let path = entry.path();
        let rel_path = drop_components(n, &path);

        if entry.file_type().is_file() {
            if orig_files.remove(&rel_path) {
                // File exists in two the two images, need to compare

                let src_path = info.source_dir.join(&rel_path);
                let old_content = std::fs::read(&src_path)?;
                let target_path = info.target_delta_dir.join(&rel_path);
                let meta_data = get_meta_data(&target_path)?;
                let new_content = std::fs::read(&target_path)?;

                if let Some(parent) = target_path.parent() {
                    use std::collections::hash_map;
                    match parent_modtime_save.entry(parent.to_owned()) {
                        hash_map::Entry::Vacant(v) => {
                            v.insert(parent.metadata()?.modified()?);
                        },
                        hash_map::Entry::Occupied(_) => {}
                    }
                }

                total_size += new_content.len() as u64;

                if let Some(x) = path_link_groups.get(&rel_path) {
                    let mut m = x.borrow_mut();
                    match &*m {
                        Some(other_path) => {
                            let target_other_path = info.target_delta_dir.join(&other_path);
                            std::fs::remove_file(&target_path)
                                .with_context(|| format!("failed removing {}",
                                        target_path.display()))?;
                            std::fs::hard_link(target_other_path, &target_path)?;
                            continue;
                        },
                        None => {
                            *m = Some(target_path.clone());
                        },
                    }
                };

                if old_content != new_content {
                    // Modified files, keep only the changes
                    let delta = xdelta3::encode(&new_content, &old_content)
                        .ok_or_else(|| Error::XDelta3EncodeError)?;

                    if debug {
                        println!("Modified {}: {} {} -> {}", rel_path.display(),
                            old_content.len(), new_content.len(), delta.len())
                    }

                    if let Some(deflated_content) = xdelta3::decode(&delta, &old_content) {
                        if deflated_content != new_content {
                            return Err(Error::XDelta3FailedValidation(src_path, target_path).into());
                        }
                    } else {
                        println!("Fallback to AsIs {}", target_path.display());

                        std::fs::remove_file(&target_path)
                            .with_context(|| format!("failed removing {}",
                                    target_path.display()))?;
                        std::fs::write(&target_path, new_content)
                            .with_context(|| format!("failed to write to {}",
                                    target_path.display()))?;
                        set_meta_data(&target_path, meta_data)
                            .with_context(|| format!("failed to set meta-data to {}",
                                    target_path.display()))?;
                        changes.push((Algo::AsIs, rel_path.as_os_str().as_bytes().to_owned()));
                        continue;
                    }

                    reduced_size += delta.len() as u64;

                    // Now write the changes, the meta-data of the original file are copied
                    std::fs::remove_file(&target_path)
                        .with_context(|| format!("failed to remove {}",
                                target_path.display()))?;
                    std::fs::write(&target_path, delta)
                        .with_context(|| format!("failed to write to {}",
                                target_path.display()))?;
                    set_meta_data(&target_path, meta_data)
                        .with_context(|| format!("failed to set meta-data to {}",
                                target_path.display()))?;

                    // We register that we have a delta here
                    changes.push((Algo::XDelta3, rel_path.as_os_str().as_bytes().to_owned()));
                    continue;
                } else {
                    // File not modified - keep a zero-sized file just for meta-data

                    if debug {
                        println!("Keep {}: {}", rel_path.display(),
                            entry.path().metadata()?.len());
                    }

                    std::fs::remove_file(&target_path)
                        .with_context(|| format!("failed removing {}",
                                target_path.display()))?;
                    std::fs::write(&target_path, "")
                        .with_context(|| format!("failed to write to {}",
                                target_path.display()))?;
                    set_meta_data(&target_path, meta_data)
                        .with_context(|| format!("failed to set meta-data to {}",
                                target_path.display()))?;
                }

                keep_files.push(rel_path.as_os_str().as_bytes().to_owned());
            }
        }
    }

    if debug {
        println!("Total size: {}", total_size);
        println!("Reduced size: {}", reduced_size);
    }

    let md = MetaData {
        keep_files,
        changes,
        version: env!("CARGO_PKG_VERSION").to_owned(),
    };

    for (pathname, modified) in parent_modtime_save {
        let mtime = filetime::FileTime::from_system_time(modified);
        filetime::set_file_times(&pathname, mtime, mtime).map_err(|e| {
            crate::Error::FileTimeError(e, pathname.to_owned())
        })?;
    }

    serialize_to_json(&md, &info.target_delta_dir.join(DELTAIMAGE_META_FILE))?;

    Ok(())
}

fn apply(debug: bool, info: cmdline::Apply) -> anyhow::Result<()> {
    let metadata_path = info.delta_target_dir.join(DELTAIMAGE_META_FILE);
    let md: MetaData =
        deserialize_from_json(&metadata_path)
        .with_context(|| format!("error reading meta-data from {}", metadata_path.display()))?;

    // Load lists
    let changes: BTreeSet<_> = md.changes.into_iter().collect();
    let mut parent_modtime_save = HashMap::new();

    let mut reduced_size = 0;
    let mut total_size = 0;

    // Detect hardlinks
    let mut fsid_link_groups = HashMap::new();
    let n = info.delta_target_dir.components().count();
    let mut recreated_paths = HashSet::new();

    for entry in WalkDir::new(&info.delta_target_dir) {
        let entry = entry?;
        let path = entry.path();
        let rel_path = drop_components(n, &path);

        if entry.file_type().is_file() {
            let metadata = entry.metadata()?;
            let fsid = (metadata.ino(), metadata.dev());
            if metadata.nlink() >= 2 {
                use std::collections::hash_map;
                let item = match fsid_link_groups.entry(fsid) {
                    hash_map::Entry::Vacant(v) => v.insert(Rc::new(RefCell::new(Vec::new()))),
                    hash_map::Entry::Occupied(o) => o.into_mut(),
                };
                item.borrow_mut().push(rel_path);
            }
        }
    }

    // Handle modified files
    for (algo, relative_path) in changes.into_iter() {
        let relative_path = PathBuf::from(OsStr::from_bytes(relative_path.as_ref()));
        let source_path = info.source_dir.join(&relative_path);

        let orig = std::fs::read(&source_path)?;
        let delta_path = info.delta_target_dir.join(&relative_path);
        let patch_data = std::fs::read(&delta_path)?;

        if let Some(parent) = delta_path.parent() {
            use std::collections::hash_map;
            match parent_modtime_save.entry(parent.to_owned()) {
                hash_map::Entry::Vacant(v) => {
                    v.insert(parent.metadata()?.modified()?);
                },
                hash_map::Entry::Occupied(_) => {}
            }
        }

        if debug {
            println!("Checking {}, {} + {} ->", relative_path.display(),
            orig.len(), delta_path.metadata()?.len());
        }

        let deflated_content = match algo {
            Algo::XDelta3 => xdelta3::decode(&patch_data, &orig)
                .ok_or_else(|| Error::XDelta3FailedDeflation(source_path.clone(),
                delta_path.clone()))?,
            Algo::AsIs => patch_data.clone(),
        };

        if debug {
            println!("Modified {}: {} -> {}", relative_path.display(), patch_data.len(),
                deflated_content.len())
        }

        reduced_size += patch_data.len() as u64;
        total_size += deflated_content.len() as u64;

        let meta_data = get_meta_data(&delta_path)?;
        std::fs::remove_file(&delta_path)?;
        std::fs::write(&delta_path, deflated_content)?;
        set_meta_data(&delta_path, meta_data)?;
        recreated_paths.insert(relative_path);
    }

    // Handle files that were not modified - simply copy from source
    for relative_path in md.keep_files.into_iter() {
        let relative_path = PathBuf::from(OsStr::from_bytes(relative_path.as_ref()));
        if debug {
            println!("Checking {}", relative_path.display())
        }
        let orig = std::fs::read(info.source_dir.join(&relative_path))?;
        let delta_path = info.delta_target_dir.join(&relative_path);

        if let Some(parent) = delta_path.parent() {
            use std::collections::hash_map;
            match parent_modtime_save.entry(parent.to_owned()) {
                hash_map::Entry::Vacant(v) => {
                    v.insert(parent.metadata()?.modified()?);
                },
                hash_map::Entry::Occupied(_) => {}
            }
        }

        if debug {
            println!("Keeping {}: {}", relative_path.display(), orig.len())
        }

        total_size += orig.len() as u64;

        let meta_data = get_meta_data(&delta_path)?;
        std::fs::write(&delta_path, orig)?;
        set_meta_data(&delta_path, meta_data)?;
        recreated_paths.insert(relative_path);
    }

    if debug {
        println!("Reduced size: {}", reduced_size);
        println!("Inflated size: {}", total_size);
    }

    // Restore hardlinks
    for (_, linkgroup) in fsid_link_groups.into_iter() {
        let linkgroup = linkgroup.borrow();
        for path in linkgroup.iter() {
            if recreated_paths.contains(path) {
                for other_path in linkgroup.iter() {
                    if other_path != path {
                        let abs_path = info.delta_target_dir.join(&path);
                        let abs_other_path = info.delta_target_dir.join(&other_path);

                        if let Some(parent) = abs_other_path.parent() {
                            use std::collections::hash_map;
                            match parent_modtime_save.entry(parent.to_owned()) {
                                hash_map::Entry::Vacant(v) => {
                                    v.insert(parent.metadata()?.modified()?);
                                },
                                hash_map::Entry::Occupied(_) => {}
                            }
                        }

                        std::fs::remove_file(&abs_other_path)?;
                        std::fs::hard_link(&abs_path, &abs_other_path)
                            .with_context(|| format!("failed linking {} -> {}",
                                    abs_path.display(), abs_other_path.display()))?;
                    }
                }
                break;
            }
        }
    }

    for (pathname, modified) in parent_modtime_save {
        let mtime = filetime::FileTime::from_system_time(modified);
        filetime::set_file_times(&pathname, mtime, mtime).map_err(|e| {
            crate::Error::FileTimeError(e, pathname.to_owned())
        })?;
    }

    std::fs::remove_file(&info.delta_target_dir.join(DELTAIMAGE_META_FILE))?;

    Ok(())
}
