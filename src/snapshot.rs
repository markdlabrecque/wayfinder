//! Online Tantivy index snapshots.
//!
//! A snapshot holds Tantivy's metadata lock only long enough to select one
//! committed generation and open all of its segment files. The open handles
//! pin those immutable bytes while the live writer continues committing and
//! merging; copying does not hold Tantivy's GC/reload lock.

use std::collections::BTreeSet;
use std::fs::{self, File};
use std::io::{self, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tantivy::Index;
use tantivy::directory::{Directory, META_LOCK, MmapDirectory};

use crate::schema;

const META_FILE: &str = "meta.json";
const MANAGED_FILE: &str = ".managed.json";

struct PinnedFile {
    path: PathBuf,
    file: File,
}

/// Copies one committed Tantivy generation from `source` into the previously
/// absent `destination` without stopping the source process.
pub fn create(source: &Path, destination: &Path) -> Result<()> {
    ensure_absent(destination)?;
    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let temporary = tempfile::Builder::new()
        .prefix(".wayfinder-snapshot-")
        .tempdir_in(parent)
        .with_context(|| {
            format!(
                "creating snapshot staging directory beside {}",
                destination.display()
            )
        })?;

    copy_generation(source, temporary.path())?;
    validate_generation(temporary.path())?;
    sync_directory(temporary.path())?;
    publish_noclobber(temporary.path(), destination)?;
    sync_directory(parent)?;
    Ok(())
}

fn ensure_absent(destination: &Path) -> Result<()> {
    match fs::symlink_metadata(destination) {
        Ok(_) => bail!(
            "snapshot destination {} already exists; destinations must be absent",
            destination.display()
        ),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "checking whether snapshot destination {} exists",
                destination.display()
            )
        }),
    }
}

fn copy_generation(source: &Path, destination: &Path) -> Result<()> {
    let source_directory = MmapDirectory::open(source)
        .with_context(|| format!("opening live Tantivy directory {}", source.display()))?;
    let (metadata, mut pinned_files) = {
        // GC takes this lock before unlinking obsolete components. Open every
        // file from one loaded generation while it is held, then release it:
        // open handles keep immutable segment bytes readable without delaying
        // the long copy's concurrent commits, merges, GC, or reader reloads.
        let _meta_lock = source_directory
            .acquire_lock(&META_LOCK)
            .with_context(|| format!("acquiring Tantivy metadata lock in {}", source.display()))?;
        let index = Index::open(source_directory.clone())
            .with_context(|| format!("opening Tantivy index in {}", source.display()))?;
        let metadata = index.load_metas().with_context(|| {
            format!("loading committed Tantivy metadata in {}", source.display())
        })?;
        let mut files = Vec::new();
        for component in metadata
            .segments
            .iter()
            .flat_map(tantivy::SegmentMeta::list_files)
        {
            let source_file = source.join(&component);
            match File::open(&source_file) {
                Ok(file) => files.push(PinnedFile {
                    path: component,
                    file,
                }),
                // SegmentMeta lists every possible component. Positions and
                // delete files, among others, legitimately may not exist for
                // a given schema/segment; staged reader validation below
                // distinguishes those cases from a missing required file.
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "opening Tantivy segment component {}",
                            source_file.display()
                        )
                    });
                }
            }
        }
        (metadata, files)
    };

    let mut managed_files = BTreeSet::new();
    for pinned in &mut pinned_files {
        copy_open_file(pinned, &destination.join(&pinned.path))?;
        managed_files.insert(pinned.path.clone());
    }

    write_json(
        &destination.join(META_FILE),
        &metadata,
        "writing snapshot Tantivy metadata",
    )?;
    managed_files.insert(PathBuf::from(META_FILE));
    write_json(
        &destination.join(MANAGED_FILE),
        &managed_files,
        "writing snapshot Tantivy managed-file list",
    )?;

    copy_file(
        &schema::snapshot_path(source),
        &schema::snapshot_path(destination),
    )?;
    copy_file(
        &schema::analyzer_contract_path(source),
        &schema::analyzer_contract_path(destination),
    )?;
    Ok(())
}

fn copy_open_file(source: &mut PinnedFile, destination: &Path) -> Result<()> {
    source
        .file
        .seek(SeekFrom::Start(0))
        .with_context(|| format!("rewinding pinned snapshot file {}", source.path.display()))?;
    let mut output = File::create(destination)
        .with_context(|| format!("creating snapshot file {}", destination.display()))?;
    io::copy(&mut source.file, &mut output).with_context(|| {
        format!(
            "copying pinned snapshot file {} to {}",
            source.path.display(),
            destination.display()
        )
    })?;
    output
        .sync_all()
        .with_context(|| format!("syncing snapshot file {}", destination.display()))?;
    Ok(())
}

fn validate_generation(destination: &Path) -> Result<()> {
    let index = Index::open_in_dir(destination).with_context(|| {
        format!(
            "validating staged snapshot Tantivy index {}",
            destination.display()
        )
    })?;
    let _reader = index.reader().with_context(|| {
        format!(
            "opening every staged snapshot segment in {}",
            destination.display()
        )
    })?;
    let damaged = index.validate_checksum().with_context(|| {
        format!(
            "validating staged snapshot checksums in {}",
            destination.display()
        )
    })?;
    if !damaged.is_empty() {
        bail!(
            "staged snapshot in {} has damaged files: {:?}",
            destination.display(),
            damaged
        );
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_vendor = "apple", target_os = "redox"))]
fn publish_noclobber(staging: &Path, destination: &Path) -> Result<()> {
    rustix::fs::renameat_with(
        rustix::fs::CWD,
        staging,
        rustix::fs::CWD,
        destination,
        rustix::fs::RenameFlags::NOREPLACE,
    )
    .with_context(|| {
        format!(
            "atomically publishing snapshot from {} to fresh destination {}",
            staging.display(),
            destination.display()
        )
    })
}

#[cfg(not(any(target_os = "linux", target_vendor = "apple", target_os = "redox")))]
fn publish_noclobber(_staging: &Path, destination: &Path) -> Result<()> {
    bail!(
        "online snapshots require atomic no-replace rename support on this platform; refusing to publish {}",
        destination.display()
    )
}

fn copy_file(source: &Path, destination: &Path) -> Result<()> {
    let mut input = File::open(source)
        .with_context(|| format!("opening snapshot file {}", source.display()))?;
    let mut output = File::create(destination)
        .with_context(|| format!("creating snapshot file {}", destination.display()))?;
    io::copy(&mut input, &mut output).with_context(|| {
        format!(
            "copying snapshot file {} to {}",
            source.display(),
            destination.display()
        )
    })?;
    output
        .sync_all()
        .with_context(|| format!("syncing snapshot file {}", destination.display()))?;
    Ok(())
}

fn write_json<T: serde::Serialize>(path: &Path, value: &T, action: &str) -> Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value).context("serializing snapshot metadata")?;
    bytes.push(b'\n');
    let mut output = File::create(path).with_context(|| format!("{action} {}", path.display()))?;
    io::Write::write_all(&mut output, &bytes)
        .with_context(|| format!("{action} {}", path.display()))?;
    output
        .sync_all()
        .with_context(|| format!("syncing snapshot metadata {}", path.display()))?;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<()> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("syncing snapshot directory {}", path.display()))
}
