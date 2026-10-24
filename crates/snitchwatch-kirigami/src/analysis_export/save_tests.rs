use super::*;
use std::{
    fs,
    os::unix::fs::{symlink, MetadataExt, PermissionsExt},
};

fn private_dir(path: &Path) {
    fs::create_dir(path).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
fn private_save_has_exact_bytes_modes_and_no_replacement() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("application/analysis-exports");
    let bytes = "{\"escaped\":\"雪\\n\\\"\"}\n".as_bytes();
    let first = save_artifact(&directory, bytes).unwrap();
    let second = save_artifact(&directory, b"second").unwrap();
    assert_ne!(first.path, second.path);
    assert_eq!(fs::read(&first.path).unwrap(), bytes);
    assert_eq!(fs::read(&second.path).unwrap(), b"second");
    assert_eq!(first.durability, Durability::Confirmed);
    assert_eq!(fs::metadata(&directory).unwrap().mode() & 0o7777, 0o700);
    assert_eq!(fs::metadata(&first.path).unwrap().mode() & 0o7777, 0o600);
    assert_eq!(fs::metadata(&first.path).unwrap().nlink(), 1);
    assert_eq!(first.path.extension().unwrap(), "json");
    assert_eq!(fs::read_dir(directory).unwrap().count(), 2);
}

#[test]
fn private_save_refuses_links_unsafe_modes_and_fifo() {
    let root = tempfile::tempdir().unwrap();
    let safe = root.path().join("safe");
    private_dir(&safe);
    let link = root.path().join("link");
    symlink(&safe, &link).unwrap();
    assert_eq!(
        save_artifact(&link, b"secret").err(),
        Some(SaveError::UnsafeDirectory)
    );
    assert_eq!(
        save_artifact(&link.join("nested"), b"secret").err(),
        Some(SaveError::UnsafeDirectory)
    );
    let public = root.path().join("public");
    fs::create_dir(&public).unwrap();
    fs::set_permissions(&public, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(
        save_artifact(&public, b"secret").err(),
        Some(SaveError::UnsafeDirectory)
    );
    assert_eq!(fs::metadata(&public).unwrap().mode() & 0o7777, 0o755);
    let fifo = root.path().join("fifo");
    let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
    assert_eq!(
        save_artifact(&fifo, b"secret").err(),
        Some(SaveError::UnsafeDirectory)
    );
    assert_eq!(fs::read_dir(&safe).unwrap().count(), 0);
}

struct Inject {
    fail: Option<Stage>,
    fixed: bool,
    swap: Option<Box<dyn Fn()>>,
    names: std::cell::Cell<usize>,
}
impl Default for Inject {
    fn default() -> Self {
        Self {
            fail: None,
            fixed: false,
            swap: None,
            names: std::cell::Cell::new(0),
        }
    }
}
impl Hooks for Inject {
    fn names(&self) -> Result<(std::ffi::CString, std::ffi::CString), SaveError> {
        self.names.set(self.names.get() + 1);
        if self.fixed {
            Ok((
                std::ffi::CString::new(".fixed.tmp").unwrap(),
                std::ffi::CString::new("fixed.json").unwrap(),
            ))
        } else {
            random_names()
        }
    }
    fn after_open(&self) {
        if let Some(swap) = &self.swap {
            swap();
        }
    }
    fn before(&self, stage: Stage) -> Result<(), SaveError> {
        if self.fail == Some(stage) {
            Err(match stage {
                Stage::Write => SaveError::Write,
                Stage::FileSync => SaveError::Sync,
                Stage::Rename => SaveError::Unsupported,
                Stage::DirectorySync => SaveError::Sync,
            })
        } else {
            Ok(())
        }
    }
}

#[test]
fn prepublication_errors_cleanup_and_preserve_prior_exports() {
    for stage in [Stage::Write, Stage::FileSync, Stage::Rename] {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().join("exports");
        private_dir(&directory);
        fs::write(directory.join("prior.json"), b"prior").unwrap();
        let hook = Inject {
            fail: Some(stage),
            ..Inject::default()
        };
        assert!(save_with(&directory, b"body-canary", &hook).is_err());
        assert_eq!(fs::read(directory.join("prior.json")).unwrap(), b"prior");
        assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    }
}

#[test]
fn published_save_reports_unconfirmed_durability() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("exports");
    private_dir(&directory);
    fs::write(directory.join("prior.json"), b"prior").unwrap();
    let hook = Inject {
        fail: Some(Stage::DirectorySync),
        ..Inject::default()
    };
    let result = save_with(&directory, b"exact\n\0bytes", &hook).unwrap();
    assert_eq!(result.durability, Durability::Unconfirmed);
    assert_eq!(fs::read(result.path).unwrap(), b"exact\n\0bytes");
    assert_eq!(hook.names.get(), 1);
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
    assert_eq!(fs::read(directory.join("prior.json")).unwrap(), b"prior");
}

#[test]
fn private_save_refuses_directory_swaps_and_bounded_collisions() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("exports");
    let moved = root.path().join("moved");
    let outside = root.path().join("outside");
    private_dir(&directory);
    private_dir(&outside);
    let (from, to, target) = (directory.clone(), moved.clone(), outside.clone());
    let swap = Inject {
        swap: Some(Box::new(move || {
            fs::rename(&from, &to).unwrap();
            symlink(&target, &from).unwrap();
        })),
        ..Inject::default()
    };
    assert_eq!(
        save_with(&directory, b"secret", &swap).err(),
        Some(SaveError::UnsafeDirectory)
    );
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
    assert_eq!(fs::read_dir(&moved).unwrap().count(), 0);
    fs::remove_file(&directory).unwrap();
    fs::rename(&moved, &directory).unwrap();
    fs::write(directory.join("fixed.json"), b"prior").unwrap();
    let collision = Inject {
        fixed: true,
        ..Inject::default()
    };
    assert_eq!(
        save_with(&directory, b"secret", &collision).err(),
        Some(SaveError::Collision)
    );
    assert_eq!(collision.names.get(), 8);
    assert_eq!(fs::read(directory.join("fixed.json")).unwrap(), b"prior");
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    let planted = directory.join(".fixed.tmp");
    symlink(outside.join("absent"), &planted).unwrap();
    assert_eq!(
        save_with(&directory, b"secret", &collision).err(),
        Some(SaveError::Collision)
    );
    assert!(fs::symlink_metadata(planted)
        .unwrap()
        .file_type()
        .is_symlink());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
fn wrong_owner_metadata_is_rejected_without_changing_it() {
    use std::os::fd::{AsRawFd, FromRawFd};
    // Read-only root-owned metadata exercises ownership without privileged setup.
    let root = std::fs::File::open("/").unwrap();
    let metadata = stat_fd(root.as_raw_fd()).unwrap();
    if metadata.st_uid != unsafe { libc::geteuid() } {
        assert_eq!(
            validate_directory(&metadata, true).err(),
            Some(SaveError::UnsafeDirectory)
        );
    }
    // Exercise an otherwise safe 0700 leaf with an unexpected owner via the metadata seam.
    let temp = tempfile::tempdir().unwrap();
    let name = std::ffi::CString::new(temp.path().as_os_str().as_encoded_bytes()).unwrap();
    let fd = unsafe {
        libc::open(
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
        )
    };
    assert!(fd >= 0);
    let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
    let mut leaf = stat_fd(fd.as_raw_fd()).unwrap();
    let original_mode = leaf.st_mode & 0o7777;
    leaf.st_mode = libc::S_IFDIR | 0o700;
    leaf.st_uid = unsafe { libc::geteuid() }.wrapping_add(1);
    assert_eq!(
        validate_directory(&leaf, true).err(),
        Some(SaveError::UnsafeDirectory)
    );
    assert_eq!(
        fs::metadata(temp.path()).unwrap().mode() & 0o7777,
        original_mode
    );
}

#[test]
fn static_errors_omit_sensitive_canaries() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("path-canary");
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o755)).unwrap();
    let error = save_artifact(&directory, b"body-canary").err().unwrap();
    let message = format!("{error:?} {error}");
    assert!(!message.contains("path-canary"));
    assert!(!message.contains("body-canary"));
}

#[test]
fn permissive_umask_still_creates_private_files() {
    const CHILD: &str = "SNITCHWATCH_SAVE_TEST_UMASK_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let root = tempfile::tempdir().unwrap();
        unsafe {
            libc::umask(0);
        }
        let directory = root.path().join("exports");
        let result = save_artifact(&directory, b"private").unwrap();
        assert_eq!(fs::metadata(directory).unwrap().mode() & 0o7777, 0o700);
        assert_eq!(fs::metadata(result.path).unwrap().mode() & 0o7777, 0o600);
        return;
    }
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "analysis_export::save::tests::permissive_umask_still_creates_private_files",
            "--nocapture",
        ])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{} {}",
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn ancestor_ownership_is_namespace_independent_and_unsafe_writable_is_refused() {
    use std::os::fd::AsRawFd;
    let root = fs::File::open("/").unwrap();
    let mut metadata = stat_fd(root.as_raw_fd()).unwrap();
    metadata.st_uid = unsafe { libc::geteuid() }.wrapping_add(1);
    metadata.st_mode = libc::S_IFDIR | 0o755;
    assert!(validate_directory(&metadata, false).is_ok());
    assert_eq!(
        validate_directory(&metadata, true).err(),
        Some(SaveError::UnsafeDirectory)
    );
    metadata.st_mode = libc::S_IFDIR | 0o1777;
    assert!(validate_directory(&metadata, false).is_ok());
    metadata.st_mode = libc::S_IFDIR | 0o777;
    assert_eq!(
        validate_directory(&metadata, false).err(),
        Some(SaveError::UnsafeDirectory)
    );
    let root = tempfile::tempdir().unwrap();
    let unsafe_parent = root.path().join("writable");
    fs::create_dir(&unsafe_parent).unwrap();
    fs::set_permissions(&unsafe_parent, fs::Permissions::from_mode(0o777)).unwrap();
    assert_eq!(
        save_artifact(&unsafe_parent.join("exports"), b"secret").err(),
        Some(SaveError::UnsafeDirectory)
    );
    assert_eq!(fs::metadata(&unsafe_parent).unwrap().mode() & 0o7777, 0o777);
    assert_eq!(fs::read_dir(unsafe_parent).unwrap().count(), 0);
}

#[test]
fn partial_write_error_removes_only_the_temporary_file() {
    struct PartialWriter;
    impl Hooks for PartialWriter {
        fn write(&self, file: &mut fs::File, artifact: &[u8]) -> Result<(), SaveError> {
            use std::io::Write;
            file.write_all(&artifact[..2]).unwrap();
            Err(SaveError::Write)
        }
    }
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("exports");
    private_dir(&directory);
    fs::write(directory.join("prior.json"), b"prior").unwrap();
    assert_eq!(
        save_with(&directory, b"partial bytes", &PartialWriter).err(),
        Some(SaveError::Write)
    );
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 1);
    assert_eq!(fs::read(directory.join("prior.json")).unwrap(), b"prior");
}
