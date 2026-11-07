use std::path::{Component, Path};

use saka_core::policy::RequestPath;
use saka_ports::{BestEffort, PortError, PortResult, WorktreeFile};
use serde_json::json;

use super::{io, resolve};

pub fn resolve_in(worktree: &Path, cwd: &str, raw: &str) -> RequestPath {
    if cwd.is_empty() || Path::new(raw).is_absolute() {
        return resolve(worktree, raw);
    }
    let resolved = resolve(worktree, &Path::new(cwd).join(raw).to_string_lossy());
    if resolved.inside_worktree {
        resolved
    } else {
        RequestPath {
            path: raw.to_string(),
            inside_worktree: false,
            through_symlink: resolved.through_symlink,
        }
    }
}

pub(super) fn relative_parts(path: &str) -> PortResult<Vec<&std::ffi::OsStr>> {
    let invalid = || PortError::permanent("worktree_path_invalid", json!({ "path": path }));
    let parts = Path::new(path)
        .components()
        .map(|c| match c {
            Component::Normal(name) => Ok(name),
            _ => Err(invalid()),
        })
        .collect::<PortResult<Vec<_>>>()?;
    if parts.is_empty() {
        return Err(invalid());
    }
    Ok(parts)
}

pub(super) fn open_no_follow(
    path: &Path,
    flags: rustix::fs::OFlags,
) -> std::io::Result<std::fs::File> {
    let flags = flags | rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::CLOEXEC;
    let fd = rustix::fs::open(path, flags, rustix::fs::Mode::from_raw_mode(0o644))?;
    Ok(std::fs::File::from(fd))
}

pub fn read_file_in(worktree: &Path, path: &str, max_bytes: u64) -> PortResult<WorktreeFile> {
    use std::io::Read;

    let parts = relative_parts(path)?;
    let mut current = worktree.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        current.push(part);
        let meta = match std::fs::symlink_metadata(&current) {
            Ok(meta) => meta,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(WorktreeFile::Missing),
            Err(e) => return Err(io("read_worktree_file", &e)),
        };
        if meta.file_type().is_symlink() {
            return Ok(WorktreeFile::NotRegular);
        }
        if i + 1 < parts.len() {
            if !meta.is_dir() {
                return Ok(WorktreeFile::Missing);
            }
            continue;
        }
        if !meta.is_file() {
            return Ok(WorktreeFile::NotRegular);
        }
        if meta.len() > max_bytes {
            return Ok(WorktreeFile::TooLarge { bytes: meta.len() });
        }
    }
    let file = match open_no_follow(&current, rustix::fs::OFlags::RDONLY) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(WorktreeFile::Missing),
        Err(e) if e.raw_os_error() == Some(rustix::io::Errno::LOOP.raw_os_error()) => {
            return Ok(WorktreeFile::NotRegular);
        }
        Err(e) => return Err(io("read_worktree_file", &e)),
    };
    let mut bytes = Vec::new();
    file.take(max_bytes.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|e| io("read_worktree_file", &e))?;
    let read = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
    if read > max_bytes {
        return Ok(WorktreeFile::TooLarge { bytes: read });
    }
    Ok(WorktreeFile::Bytes(bytes))
}

pub fn write_file_in(worktree: &Path, path: &str, bytes: &[u8]) -> PortResult<()> {
    use std::io::Write;

    let parts = relative_parts(path)?;
    let conflict = |at: &Path| {
        PortError::permanent(
            "worktree_path_conflict",
            json!({ "path": path, "at": at.strip_prefix(worktree).unwrap_or(at).to_string_lossy() }),
        )
    };
    let mut current = worktree.to_path_buf();
    for (i, part) in parts.iter().enumerate() {
        current.push(part);
        let last = i + 1 == parts.len();
        match std::fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() => return Err(conflict(&current)),
            Ok(meta) if last && !meta.is_file() => return Err(conflict(&current)),
            Ok(meta) if !last && !meta.is_dir() => return Err(conflict(&current)),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !last => {
                std::fs::create_dir(&current).map_err(|e| io("write_worktree_file", &e))?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(io("write_worktree_file", &e)),
        }
    }
    let dir = current.parent().unwrap_or(worktree);
    let name = current
        .file_name()
        .map(|n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!(".{}.saka-tmp", name.unwrap_or_default()));
    std::fs::remove_file(&tmp).best_effort("remove a temp file left by an earlier write");
    let write = || -> std::io::Result<()> {
        let flags =
            rustix::fs::OFlags::WRONLY | rustix::fs::OFlags::CREATE | rustix::fs::OFlags::EXCL;
        let mut file = open_no_follow(&tmp, flags)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, &current)
    };
    write().map_err(|e| {
        std::fs::remove_file(&tmp).best_effort("remove the temp file of a failed write");
        io("write_worktree_file", &e)
    })
}
