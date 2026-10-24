use std::{
    ffi::{CStr, CString},
    fs::File,
    io::Write,
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::ffi::OsStrExt,
    },
    path::{Component, Path, PathBuf},
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Durability {
    Confirmed,
    Unconfirmed,
}

/// Local UI data. Deliberately has no Debug or serialization implementation.
pub struct SavedArtifact {
    pub path: PathBuf,
    pub durability: Durability,
}

/// A static failure guarantees this call did not publish an artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SaveError {
    UnsafeDirectory,
    Create,
    Write,
    Sync,
    Publish,
    Unsupported,
    Collision,
    Random,
}
impl std::fmt::Display for SaveError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for SaveError {}

/// Blocking I/O for an explicit Save; the controller must call this off Qt.
/// Once published, the artifact is never retried or removed by this function.
pub fn save_artifact(directory: &Path, artifact: &[u8]) -> Result<SavedArtifact, SaveError> {
    save_with(directory, artifact, &SystemHooks)
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Stage {
    Write,
    FileSync,
    Rename,
    DirectorySync,
}
trait Hooks {
    fn write(&self, file: &mut File, artifact: &[u8]) -> Result<(), SaveError> {
        file.write_all(artifact).map_err(|_| SaveError::Write)
    }
    fn names(&self) -> Result<(CString, CString), SaveError> {
        random_names()
    }
    fn after_open(&self) {}
    fn before(&self, _stage: Stage) -> Result<(), SaveError> {
        Ok(())
    }
}
struct SystemHooks;
impl Hooks for SystemHooks {}

fn save_with(
    directory: &Path,
    artifact: &[u8],
    hooks: &impl Hooks,
) -> Result<SavedArtifact, SaveError> {
    let dir = open_directory(directory, true)?;
    let original = stat_fd(dir.as_raw_fd())?;
    hooks.after_open();
    for _ in 0..8 {
        let (temporary, final_name) = hooks.names()?;
        let raw = unsafe {
            libc::openat(
                dir.as_raw_fd(),
                temporary.as_ptr(),
                libc::O_WRONLY
                    | libc::O_CREAT
                    | libc::O_EXCL
                    | libc::O_NOFOLLOW
                    | libc::O_NONBLOCK
                    | libc::O_CLOEXEC,
                0o600,
            )
        };
        if raw < 0 {
            if errno() == libc::EEXIST {
                continue;
            }
            return Err(SaveError::Create);
        }
        let mut cleanup = Temporary {
            directory: dir.as_raw_fd(),
            name: &temporary,
            armed: true,
        };
        // SAFETY: openat returned a new descriptor, consumed exactly once.
        let mut file = unsafe { File::from_raw_fd(raw) };
        let metadata = stat_fd(file.as_raw_fd())?;
        if metadata.st_mode & libc::S_IFMT != libc::S_IFREG
            || metadata.st_uid != effective_uid()
            || metadata.st_nlink != 1
            || metadata.st_mode & 0o7777 != 0o600
        {
            return Err(SaveError::Create);
        }
        hooks.before(Stage::Write)?;
        hooks.write(&mut file, artifact)?;
        hooks.before(Stage::FileSync)?;
        file.sync_all().map_err(|_| SaveError::Sync)?;
        // Refuse a substituted user-visible directory. Publication and cleanup always
        // use the original pinned FD, including if another swap races this check.
        let current = open_directory(directory, false)?;
        let current = stat_fd(current.as_raw_fd())?;
        if (original.st_dev, original.st_ino) != (current.st_dev, current.st_ino) {
            return Err(SaveError::UnsafeDirectory);
        }
        hooks.before(Stage::Rename)?;
        let renamed = unsafe {
            libc::renameat2(
                dir.as_raw_fd(),
                temporary.as_ptr(),
                dir.as_raw_fd(),
                final_name.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if renamed < 0 {
            match errno() {
                libc::EEXIST => continue,
                libc::ENOSYS | libc::EINVAL | libc::EOPNOTSUPP => {
                    return Err(SaveError::Unsupported)
                }
                _ => return Err(SaveError::Publish),
            }
        }
        // Rename is the commit point. No error, unlink, or retry may follow it.
        cleanup.armed = false;
        let durability = if hooks.before(Stage::DirectorySync).is_ok()
            && unsafe { libc::fsync(dir.as_raw_fd()) } == 0
        {
            Durability::Confirmed
        } else {
            Durability::Unconfirmed
        };
        return Ok(SavedArtifact {
            path: directory.join(std::ffi::OsStr::from_bytes(final_name.to_bytes())),
            durability,
        });
    }
    Err(SaveError::Collision)
}

struct Temporary<'a> {
    directory: RawFd,
    name: &'a CStr,
    armed: bool,
}
impl Drop for Temporary<'_> {
    fn drop(&mut self) {
        if self.armed {
            unsafe {
                libc::unlinkat(self.directory, self.name.as_ptr(), 0);
            }
        }
    }
}

fn open_directory(path: &Path, create: bool) -> Result<OwnedFd, SaveError> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(name) => components
                .push(CString::new(name.as_bytes()).map_err(|_| SaveError::UnsafeDirectory)?),
            Component::RootDir | Component::CurDir => {}
            _ => return Err(SaveError::UnsafeDirectory),
        }
    }
    if components.is_empty() {
        return Err(SaveError::UnsafeDirectory);
    }
    let start = if path.is_absolute() { c"/" } else { c"." };
    let raw = unsafe {
        libc::open(
            start.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )
    };
    if raw < 0 {
        return Err(SaveError::UnsafeDirectory);
    }
    let mut current = unsafe { OwnedFd::from_raw_fd(raw) };
    validate_directory(&stat_fd(current.as_raw_fd())?, false)?;
    for (index, component) in components.iter().enumerate() {
        let mut raw = open_child(current.as_raw_fd(), component);
        if raw < 0 && errno() == libc::ENOENT && create {
            let made = unsafe { libc::mkdirat(current.as_raw_fd(), component.as_ptr(), 0o700) };
            if made < 0 && errno() != libc::EEXIST {
                return Err(SaveError::UnsafeDirectory);
            }
            // Persist newly created directory entries before committing any artifact.
            if made == 0 && unsafe { libc::fsync(current.as_raw_fd()) } != 0 {
                return Err(SaveError::Sync);
            }
            raw = open_child(current.as_raw_fd(), component);
        }
        if raw < 0 {
            return Err(SaveError::UnsafeDirectory);
        }
        let next = unsafe { OwnedFd::from_raw_fd(raw) };
        validate_directory(&stat_fd(next.as_raw_fd())?, index + 1 == components.len())?;
        current = next;
    }
    Ok(current)
}
fn open_child(parent: RawFd, name: &CStr) -> RawFd {
    unsafe {
        libc::openat(
            parent,
            name.as_ptr(),
            libc::O_RDONLY
                | libc::O_DIRECTORY
                | libc::O_NOFOLLOW
                | libc::O_CLOEXEC
                | libc::O_NONBLOCK,
        )
    }
}
fn stat_fd(fd: RawFd) -> Result<libc::stat, SaveError> {
    let mut metadata = std::mem::MaybeUninit::<libc::stat>::uninit();
    if unsafe { libc::fstat(fd, metadata.as_mut_ptr()) } < 0 {
        return Err(SaveError::UnsafeDirectory);
    }
    Ok(unsafe { metadata.assume_init() })
}
fn validate_directory(metadata: &libc::stat, leaf: bool) -> Result<(), SaveError> {
    let mode = metadata.st_mode;
    let ours = metadata.st_uid == effective_uid();
    // Ancestor ownership may be unmapped in a user namespace. Sticky ancestors
    // protect directory entries; only the export leaf requires current ownership.
    let sticky = mode & libc::S_ISVTX != 0;
    if mode & libc::S_IFMT != libc::S_IFDIR
        || (mode & 0o022 != 0 && !sticky)
        || (leaf && (!ours || mode & 0o7777 != 0o700))
    {
        Err(SaveError::UnsafeDirectory)
    } else {
        Ok(())
    }
}
fn effective_uid() -> libc::uid_t {
    unsafe { libc::geteuid() }
}
fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}
fn random_names() -> Result<(CString, CString), SaveError> {
    let mut random = [0u8; 16];
    let mut offset = 0;
    while offset < random.len() {
        let count = unsafe {
            libc::getrandom(
                random[offset..].as_mut_ptr().cast(),
                random.len() - offset,
                0,
            )
        };
        if count < 0 {
            if errno() == libc::EINTR {
                continue;
            }
            return Err(SaveError::Random);
        }
        if count == 0 {
            return Err(SaveError::Random);
        }
        offset += count as usize;
    }
    let mut token = String::with_capacity(32);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in random {
        token.push(HEX[(byte >> 4) as usize] as char);
        token.push(HEX[(byte & 15) as usize] as char);
    }
    Ok((
        CString::new(format!(".cache-summary-{token}.tmp")).map_err(|_| SaveError::Random)?,
        CString::new(format!("cache-summary-{token}.json")).map_err(|_| SaveError::Random)?,
    ))
}

#[cfg(test)]
#[path = "save_tests.rs"]
mod tests;
