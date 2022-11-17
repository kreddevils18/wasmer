//! WARNING: the API exposed here is unstable and very experimental.  Certain things are not ready
//! yet and may be broken in patch releases.  If you're using this and have any specific needs,
//! please [let us know here](https://github.com/wasmerio/wasmer/issues/583) or by filing an issue.
//!
//! Wasmer always has a virtual root directory located at `/` at which all pre-opened directories can
//! be found.  It's possible to traverse between preopened directories this way as well (for example
//! `preopen-dir1/../preopen-dir2`).
//!
//! A preopened directory is a directory or directory + name combination passed into the
//! `generate_import_object` function.  These are directories that the caller has given
//! the WASI module permission to access.
//!
//! You can implement `VirtualFile` for your own types to get custom behavior and extend WASI, see the
//! [WASI plugin example](https://github.com/wasmerio/wasmer/blob/master/examples/plugin.rs).

#![allow(clippy::cognitive_complexity, clippy::too_many_arguments)]

mod builder;
mod guard;
mod parking;
mod socket;
mod thread;
mod types;

pub use self::builder::*;
pub use self::guard::*;
pub use self::guard::*;
pub use self::parking::*;
pub use self::socket::*;
pub use self::thread::*;
pub use self::types::*;
use crate::bin_factory::BinaryPackage;
use crate::syscalls::types::*;
use crate::utils::map_io_err;
use crate::WasiCallingId;
use crate::WasiFunctionEnv;
use crate::WasiRuntimeImplementation;
use cooked_waker::ViaRawPointer;
use cooked_waker::Wake;
use cooked_waker::WakeRef;
use derivative::Derivative;
use generational_arena::Arena;
pub use generational_arena::Index as Inode;
#[cfg(feature = "enable-serde")]
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::HashSet;
use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::MutexGuard;
use std::task::Waker;
use std::time::Duration;
use std::{
    borrow::Borrow,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
        Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
};
use tracing::{debug, trace};
use wasmer::Store;
use wasmer_vbus::VirtualBusCalled;
use wasmer_vbus::VirtualBusInvocation;
use wasmer_vfs::AsyncWriteExt;
use wasmer_vfs::FileOpener;
use wasmer_vfs::WasiPipe;
use wasmer_wasi_types::wasi::Cid;
use wasmer_wasi_types::wasi::Clockid;
use wasmer_wasi_types::wasi::{
    Errno, Fd as WasiFd, Fdflags, Fdstat, Filesize, Filestat, Filetype, Preopentype, Rights,
};
use wasmer_wasi_types::wasi::{Prestat, PrestatEnum};

use wasmer_vfs::{FileSystem, FsError, OpenOptions, VirtualFile};

/// the fd value of the virtual root
pub const VIRTUAL_ROOT_FD: WasiFd = 3;
/// all the rights enabled
pub const ALL_RIGHTS: Rights = Rights::all();
const STDIN_DEFAULT_RIGHTS: Rights = {
    // This might seem a bit overenineered, but it's the only way I
    // discovered for getting the values in a const environment
    Rights::from_bits_truncate(
        Rights::FD_DATASYNC.bits()
            | Rights::FD_READ.bits()
            | Rights::FD_SYNC.bits()
            | Rights::FD_ADVISE.bits()
            | Rights::FD_FILESTAT_GET.bits()
            | Rights::POLL_FD_READWRITE.bits(),
    )
};
const STDOUT_DEFAULT_RIGHTS: Rights = {
    // This might seem a bit overenineered, but it's the only way I
    // discovered for getting the values in a const environment
    Rights::from_bits_truncate(
        Rights::FD_DATASYNC.bits()
            | Rights::FD_SYNC.bits()
            | Rights::FD_WRITE.bits()
            | Rights::FD_ADVISE.bits()
            | Rights::FD_FILESTAT_GET.bits()
            | Rights::POLL_FD_READWRITE.bits(),
    )
};
const STDERR_DEFAULT_RIGHTS: Rights = STDOUT_DEFAULT_RIGHTS;

/// A completely aribtrary "big enough" number used as the upper limit for
/// the number of symlinks that can be traversed when resolving a path
pub const MAX_SYMLINKS: u32 = 128;

/// A file that Wasi knows about that may or may not be open
#[derive(Debug)]
#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub struct InodeVal {
    pub stat: RwLock<Filestat>,
    pub is_preopened: bool,
    pub name: Cow<'static, str>,
    pub kind: RwLock<Kind>,
}

impl InodeVal {
    pub fn read(&self) -> RwLockReadGuard<Kind> {
        self.kind.read().unwrap()
    }

    pub fn write(&self) -> RwLockWriteGuard<Kind> {
        self.kind.write().unwrap()
    }
}

/// The core of the filesystem abstraction.  Includes directories,
/// files, and symlinks.
#[derive(Debug)]
#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub enum Kind {
    File {
        /// The open file, if it's open
        #[cfg_attr(feature = "enable-serde", serde(skip))]
        handle: Option<Arc<RwLock<Box<dyn VirtualFile + Send + Sync + 'static>>>>,
        /// The path on the host system where the file is located
        /// This is deprecated and will be removed soon
        path: PathBuf,
        /// Marks the file as a special file that only one `fd` can exist for
        /// This is useful when dealing with host-provided special files that
        /// should be looked up by path
        /// TOOD: clarify here?
        fd: Option<u32>,
    },
    #[cfg_attr(feature = "enable-serde", serde(skip))]
    Socket {
        /// Represents a networking socket
        socket: InodeSocket,
    },
    #[cfg_attr(feature = "enable-serde", serde(skip))]
    Pipe {
        /// Reference to the pipe
        pipe: WasiPipe,
    },
    Dir {
        /// Parent directory
        parent: Option<Inode>,
        /// The path on the host system where the directory is located
        // TODO: wrap it like VirtualFile
        path: PathBuf,
        /// The entries of a directory are lazily filled.
        entries: HashMap<String, Inode>,
    },
    /// The same as Dir but without the irrelevant bits
    /// The root is immutable after creation; generally the Kind::Root
    /// branch of whatever code you're writing will be a simpler version of
    /// your Kind::Dir logic
    Root {
        entries: HashMap<String, Inode>,
    },
    /// The first two fields are data _about_ the symlink
    /// the last field is the data _inside_ the symlink
    ///
    /// `base_po_dir` should never be the root because:
    /// - Right now symlinks are not allowed in the immutable root
    /// - There is always a closer pre-opened dir to the symlink file (by definition of the root being a collection of preopened dirs)
    Symlink {
        /// The preopened dir that this symlink file is relative to (via `path_to_symlink`)
        base_po_dir: WasiFd,
        /// The path to the symlink from the `base_po_dir`
        path_to_symlink: PathBuf,
        /// the value of the symlink as a relative path
        relative_path: PathBuf,
    },
    Buffer {
        buffer: Vec<u8>,
    },
    EventNotifications {
        /// Used for event notifications by the user application or operating system
        /// (positive number means there are events waiting to be processed)
        counter: Arc<AtomicU64>,
        /// Flag that indicates if this is operating
        is_semaphore: bool,
        /// Receiver that wakes sleeping threads
        #[cfg_attr(feature = "enable-serde", serde(skip))]
        wakers: Arc<Mutex<VecDeque<tokio::sync::mpsc::UnboundedSender<()>>>>,
        /// Immediate waker
        immediate: Arc<AtomicBool>,
    },
}

#[derive(Debug, Clone)]
#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub struct Fd {
    /// The reference count is only increased when the FD is
    /// duplicates - fd_close will not kill the inode until this reaches zero
    pub ref_cnt: Arc<AtomicU32>,
    pub rights: Rights,
    pub rights_inheriting: Rights,
    pub flags: Fdflags,
    pub offset: Arc<AtomicU64>,
    /// Flags that determine how the [`Fd`] can be used.
    ///
    /// Used when reopening a [`VirtualFile`] during [`WasiState`] deserialization.
    pub open_flags: u16,
    pub inode: Inode,
}

impl Fd {
    /// This [`Fd`] can be used with read system calls.
    pub const READ: u16 = 1;
    /// This [`Fd`] can be used with write system calls.
    pub const WRITE: u16 = 2;
    /// This [`Fd`] can append in write system calls. Note that the append
    /// permission implies the write permission.
    pub const APPEND: u16 = 4;
    /// This [`Fd`] will delete everything before writing. Note that truncate
    /// permissions require the write permission.
    ///
    /// This permission is currently unused when deserializing [`WasiState`].
    pub const TRUNCATE: u16 = 8;
    /// This [`Fd`] may create a file before writing to it. Note that create
    /// permissions require write permissions.
    ///
    /// This permission is currently unused when deserializing [`WasiState`].
    pub const CREATE: u16 = 16;
}

#[derive(Debug)]
#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub struct WasiInodes {
    pub arena: Arena<InodeVal>,
    pub orphan_fds: HashMap<Inode, InodeVal>,
}

impl WasiInodes {
    /// gets either a normal inode or an orphaned inode
    pub fn get_inodeval(&self, inode: generational_arena::Index) -> Result<&InodeVal, Errno> {
        if let Some(iv) = self.arena.get(inode) {
            Ok(iv)
        } else {
            self.orphan_fds.get(&inode).ok_or(Errno::Badf)
        }
    }

    /// gets either a normal inode or an orphaned inode
    pub fn get_inodeval_mut(
        &mut self,
        inode: generational_arena::Index,
    ) -> Result<&mut InodeVal, Errno> {
        if let Some(iv) = self.arena.get_mut(inode) {
            Ok(iv)
        } else {
            self.orphan_fds.get_mut(&inode).ok_or(Errno::Badf)
        }
    }

    /// Get the `VirtualFile` object at stdout
    pub(crate) fn stdout(
        &self,
        fd_map: &RwLock<HashMap<u32, Fd>>,
    ) -> Result<InodeValFileReadGuard, FsError> {
        self.std_dev_get(fd_map, __WASI_STDOUT_FILENO)
    }
    /// Get the `VirtualFile` object at stdout mutably
    pub(crate) fn stdout_mut(
        &self,
        fd_map: &RwLock<HashMap<u32, Fd>>,
    ) -> Result<InodeValFileWriteGuard, FsError> {
        self.std_dev_get_mut(fd_map, __WASI_STDOUT_FILENO)
    }

    /// Get the `VirtualFile` object at stderr
    pub(crate) fn stderr(
        &self,
        fd_map: &RwLock<HashMap<u32, Fd>>,
    ) -> Result<InodeValFileReadGuard, FsError> {
        self.std_dev_get(fd_map, __WASI_STDERR_FILENO)
    }
    /// Get the `VirtualFile` object at stderr mutably
    pub(crate) fn stderr_mut(
        &self,
        fd_map: &RwLock<HashMap<u32, Fd>>,
    ) -> Result<InodeValFileWriteGuard, FsError> {
        self.std_dev_get_mut(fd_map, __WASI_STDERR_FILENO)
    }

    /// Get the `VirtualFile` object at stdin
    pub(crate) fn stdin(
        &self,
        fd_map: &RwLock<HashMap<u32, Fd>>,
    ) -> Result<InodeValFileReadGuard, FsError> {
        self.std_dev_get(fd_map, __WASI_STDIN_FILENO)
    }
    /// Get the `VirtualFile` object at stdin mutably
    pub(crate) fn stdin_mut(
        &self,
        fd_map: &RwLock<HashMap<u32, Fd>>,
    ) -> Result<InodeValFileWriteGuard, FsError> {
        self.std_dev_get_mut(fd_map, __WASI_STDIN_FILENO)
    }

    /// Internal helper function to get a standard device handle.
    /// Expects one of `__WASI_STDIN_FILENO`, `__WASI_STDOUT_FILENO`, `__WASI_STDERR_FILENO`.
    fn std_dev_get<'a>(
        &'a self,
        fd_map: &RwLock<HashMap<u32, Fd>>,
        fd: WasiFd,
    ) -> Result<InodeValFileReadGuard, FsError> {
        if let Some(fd) = fd_map.read().unwrap().get(&fd) {
            let guard = self.arena[fd.inode].read();
            if let Kind::File { handle, .. } = guard.deref() {
                if let Some(handle) = handle {
                    Ok(InodeValFileReadGuard::new(handle))
                } else {
                    Err(FsError::NotAFile)
                }
            } else {
                // Our public API should ensure that this is not possible
                Err(FsError::NotAFile)
            }
        } else {
            // this should only trigger if we made a mistake in this crate
            Err(FsError::NoDevice)
        }
    }
    /// Internal helper function to mutably get a standard device handle.
    /// Expects one of `__WASI_STDIN_FILENO`, `__WASI_STDOUT_FILENO`, `__WASI_STDERR_FILENO`.
    fn std_dev_get_mut<'a>(
        &'a self,
        fd_map: &RwLock<HashMap<u32, Fd>>,
        fd: WasiFd,
    ) -> Result<InodeValFileWriteGuard, FsError> {
        if let Some(fd) = fd_map.read().unwrap().get(&fd) {
            let guard = self.arena[fd.inode].read();
            if let Kind::File { handle, .. } = guard.deref() {
                if let Some(handle) = handle {
                    Ok(InodeValFileWriteGuard::new(handle))
                } else {
                    Err(FsError::NotAFile)
                }
            } else {
                // Our public API should ensure that this is not possible
                Err(FsError::NotAFile)
            }
        } else {
            // this should only trigger if we made a mistake in this crate
            Err(FsError::NoDevice)
        }
    }
}

#[derive(Debug, Clone)]
pub enum WasiFsRoot {
    Sandbox(Arc<wasmer_vfs::tmp_fs::TmpFileSystem>),
    Backing(Arc<Box<dyn FileSystem>>),
}

impl FileSystem for WasiFsRoot {
    fn read_dir(&self, path: &Path) -> wasmer_vfs::Result<wasmer_vfs::ReadDir> {
        match self {
            WasiFsRoot::Sandbox(fs) => fs.read_dir(path),
            WasiFsRoot::Backing(fs) => fs.read_dir(path),
        }
    }
    fn create_dir(&self, path: &Path) -> wasmer_vfs::Result<()> {
        match self {
            WasiFsRoot::Sandbox(fs) => fs.create_dir(path),
            WasiFsRoot::Backing(fs) => fs.create_dir(path),
        }
    }
    fn remove_dir(&self, path: &Path) -> wasmer_vfs::Result<()> {
        match self {
            WasiFsRoot::Sandbox(fs) => fs.remove_dir(path),
            WasiFsRoot::Backing(fs) => fs.remove_dir(path),
        }
    }
    fn rename(&self, from: &Path, to: &Path) -> wasmer_vfs::Result<()> {
        match self {
            WasiFsRoot::Sandbox(fs) => fs.rename(from, to),
            WasiFsRoot::Backing(fs) => fs.rename(from, to),
        }
    }
    fn metadata(&self, path: &Path) -> wasmer_vfs::Result<wasmer_vfs::Metadata> {
        match self {
            WasiFsRoot::Sandbox(fs) => fs.metadata(path),
            WasiFsRoot::Backing(fs) => fs.metadata(path),
        }
    }
    fn symlink_metadata(&self, path: &Path) -> wasmer_vfs::Result<wasmer_vfs::Metadata> {
        match self {
            WasiFsRoot::Sandbox(fs) => fs.symlink_metadata(path),
            WasiFsRoot::Backing(fs) => fs.symlink_metadata(path),
        }
    }
    fn remove_file(&self, path: &Path) -> wasmer_vfs::Result<()> {
        match self {
            WasiFsRoot::Sandbox(fs) => fs.remove_file(path),
            WasiFsRoot::Backing(fs) => fs.remove_file(path),
        }
    }
    fn new_open_options(&self) -> OpenOptions {
        match self {
            WasiFsRoot::Sandbox(fs) => fs.new_open_options(),
            WasiFsRoot::Backing(fs) => fs.new_open_options(),
        }
    }
}

/// Warning, modifying these fields directly may cause invariants to break and
/// should be considered unsafe.  These fields may be made private in a future release
#[derive(Debug)]
#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub struct WasiFs {
    //pub repo: Repo,
    pub preopen_fds: RwLock<Vec<u32>>,
    pub name_map: HashMap<String, Inode>,
    pub fd_map: Arc<RwLock<HashMap<WasiFd, Fd>>>,
    pub next_fd: AtomicU32,
    inode_counter: AtomicU64,
    pub current_dir: Mutex<String>,
    #[cfg(feature = "wasix")]
    pub is_wasix: AtomicBool,
    #[cfg_attr(feature = "enable-serde", serde(skip, default))]
    pub root_fs: WasiFsRoot,
    pub has_unioned: Arc<Mutex<HashSet<String>>>,
}

impl WasiFs {
    /// Forking the WasiState is used when either fork or vfork is called
    pub fn fork(&self) -> Self {
        let fd_map = self.fd_map.read().unwrap().clone();
        for fd in fd_map.values() {
            fd.ref_cnt.fetch_add(1, Ordering::Relaxed);
        }
        Self {
            preopen_fds: RwLock::new(self.preopen_fds.read().unwrap().clone()),
            name_map: self.name_map.clone(),
            fd_map: Arc::new(RwLock::new(fd_map)),
            next_fd: AtomicU32::new(self.next_fd.load(Ordering::Acquire)),
            inode_counter: AtomicU64::new(self.inode_counter.load(Ordering::Acquire)),
            current_dir: Mutex::new(self.current_dir.lock().unwrap().clone()),
            #[cfg(feature = "wasix")]
            is_wasix: AtomicBool::new(self.is_wasix.load(Ordering::Acquire)),
            root_fs: self.root_fs.clone(),
            has_unioned: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// Closes all the file handles
    pub fn close_all(&self, inodes: &WasiInodes) {
        let mut guard = self.fd_map.write().unwrap();
        let fds = { guard.iter().map(|a| *a.0).collect::<Vec<_>>() };

        for fd in fds {
            _ = self.close_fd_ext(inodes, &mut guard, fd);
        }
    }

    /// Will conditionally union the binary file system with this one
    /// if it has not already been unioned
    pub fn conditional_union(&self, binary: &BinaryPackage) -> bool {
        let sandbox_fs = match &self.root_fs {
            WasiFsRoot::Sandbox(fs) => fs,
            WasiFsRoot::Backing(_) => {
                tracing::error!("can not perform a union on a backing file system");
                return false;
            }
        };
        let package_name = binary.package_name.to_string();
        let mut guard = self.has_unioned.lock().unwrap();
        if guard.contains(&package_name) == false {
            guard.insert(package_name);

            if let Some(fs) = binary.webc_fs.clone() {
                sandbox_fs.union(&fs);
            }
        }
        true
    }
}

/// Returns the default filesystem backing
pub fn default_fs_backing() -> Box<dyn wasmer_vfs::FileSystem + Send + Sync> {
    cfg_if::cfg_if! {
        if #[cfg(feature = "host-fs")] {
            Box::new(wasmer_vfs::host_fs::FileSystem::default())
        } else if #[cfg(not(feature = "host-fs"))] {
            Box::new(wasmer_vfs::mem_fs::FileSystem::default())
        } else {
            Box::new(FallbackFileSystem::default())
        }
    }
}

#[derive(Debug, Default)]
pub struct FallbackFileSystem;

impl FallbackFileSystem {
    fn fail() -> ! {
        panic!("No filesystem set for wasmer-wasi, please enable either the `host-fs` or `mem-fs` feature or set your custom filesystem with `WasiStateBuilder::set_fs`");
    }
}

impl FileSystem for FallbackFileSystem {
    fn read_dir(&self, _path: &Path) -> Result<wasmer_vfs::ReadDir, FsError> {
        Self::fail();
    }
    fn create_dir(&self, _path: &Path) -> Result<(), FsError> {
        Self::fail();
    }
    fn remove_dir(&self, _path: &Path) -> Result<(), FsError> {
        Self::fail();
    }
    fn rename(&self, _from: &Path, _to: &Path) -> Result<(), FsError> {
        Self::fail();
    }
    fn metadata(&self, _path: &Path) -> Result<wasmer_vfs::Metadata, FsError> {
        Self::fail();
    }
    fn symlink_metadata(&self, _path: &Path) -> Result<wasmer_vfs::Metadata, FsError> {
        Self::fail();
    }
    fn remove_file(&self, _path: &Path) -> Result<(), FsError> {
        Self::fail();
    }
    fn new_open_options(&self) -> wasmer_vfs::OpenOptions {
        Self::fail();
    }
}

impl WasiFs {
    /// Created for the builder API. like `new` but with more information
    pub(crate) fn new_with_preopen(
        inodes: &mut WasiInodes,
        preopens: &[PreopenedDir],
        vfs_preopens: &[String],
        fs_backing: WasiFsRoot,
    ) -> Result<Self, String> {
        let (wasi_fs, root_inode) = Self::new_init(fs_backing, inodes)?;

        for preopen_name in vfs_preopens {
            let kind = Kind::Dir {
                parent: Some(root_inode),
                path: PathBuf::from(preopen_name),
                entries: Default::default(),
            };
            let rights = Rights::FD_ADVISE
                | Rights::FD_TELL
                | Rights::FD_SEEK
                | Rights::FD_READ
                | Rights::PATH_OPEN
                | Rights::FD_READDIR
                | Rights::PATH_READLINK
                | Rights::PATH_FILESTAT_GET
                | Rights::FD_FILESTAT_GET
                | Rights::PATH_LINK_SOURCE
                | Rights::PATH_RENAME_SOURCE
                | Rights::POLL_FD_READWRITE
                | Rights::SOCK_SHUTDOWN;
            let inode = wasi_fs
                .create_inode(inodes, kind, true, preopen_name.clone())
                .map_err(|e| {
                    format!(
                        "Failed to create inode for preopened dir (name `{}`): WASI error code: {}",
                        preopen_name, e
                    )
                })?;
            let fd_flags = Fd::READ;
            let fd = wasi_fs
                .create_fd(rights, rights, Fdflags::empty(), fd_flags, inode)
                .map_err(|e| format!("Could not open fd for file {:?}: {}", preopen_name, e))?;
            {
                let mut guard = inodes.arena[root_inode].write();
                if let Kind::Root { entries } = guard.deref_mut() {
                    let existing_entry = entries.insert(preopen_name.clone(), inode);
                    if existing_entry.is_some() {
                        return Err(format!(
                            "Found duplicate entry for alias `{}`",
                            preopen_name
                        ));
                    }
                    assert!(existing_entry.is_none())
                }
            }
            wasi_fs.preopen_fds.write().unwrap().push(fd);
        }

        for PreopenedDir {
            path,
            alias,
            read,
            write,
            create,
        } in preopens
        {
            debug!(
                "Attempting to preopen {} with alias {:?}",
                &path.to_string_lossy(),
                &alias
            );
            let cur_dir_metadata = wasi_fs
                .root_fs
                .metadata(path)
                .map_err(|e| format!("Could not get metadata for file {:?}: {}", path, e))?;

            let kind = if cur_dir_metadata.is_dir() {
                Kind::Dir {
                    parent: Some(root_inode),
                    path: path.clone(),
                    entries: Default::default(),
                }
            } else {
                return Err(format!(
                    "WASI only supports pre-opened directories right now; found \"{}\"",
                    &path.to_string_lossy()
                ));
            };

            let rights = {
                // TODO: review tell' and fd_readwrite
                let mut rights = Rights::FD_ADVISE | Rights::FD_TELL | Rights::FD_SEEK;
                if *read {
                    rights |= Rights::FD_READ
                        | Rights::PATH_OPEN
                        | Rights::FD_READDIR
                        | Rights::PATH_READLINK
                        | Rights::PATH_FILESTAT_GET
                        | Rights::FD_FILESTAT_GET
                        | Rights::PATH_LINK_SOURCE
                        | Rights::PATH_RENAME_SOURCE
                        | Rights::POLL_FD_READWRITE
                        | Rights::SOCK_SHUTDOWN;
                }
                if *write {
                    rights |= Rights::FD_DATASYNC
                        | Rights::FD_FDSTAT_SET_FLAGS
                        | Rights::FD_WRITE
                        | Rights::FD_SYNC
                        | Rights::FD_ALLOCATE
                        | Rights::PATH_OPEN
                        | Rights::PATH_RENAME_TARGET
                        | Rights::PATH_FILESTAT_SET_SIZE
                        | Rights::PATH_FILESTAT_SET_TIMES
                        | Rights::FD_FILESTAT_SET_SIZE
                        | Rights::FD_FILESTAT_SET_TIMES
                        | Rights::PATH_REMOVE_DIRECTORY
                        | Rights::PATH_UNLINK_FILE
                        | Rights::POLL_FD_READWRITE
                        | Rights::SOCK_SHUTDOWN;
                }
                if *create {
                    rights |= Rights::PATH_CREATE_DIRECTORY
                        | Rights::PATH_CREATE_FILE
                        | Rights::PATH_LINK_TARGET
                        | Rights::PATH_OPEN
                        | Rights::PATH_RENAME_TARGET
                        | Rights::PATH_SYMLINK;
                }

                rights
            };
            let inode = if let Some(alias) = &alias {
                wasi_fs.create_inode(inodes, kind, true, alias.clone())
            } else {
                wasi_fs.create_inode(inodes, kind, true, path.to_string_lossy().into_owned())
            }
            .map_err(|e| {
                format!(
                    "Failed to create inode for preopened dir: WASI error code: {}",
                    e
                )
            })?;
            let fd_flags = {
                let mut fd_flags = 0;
                if *read {
                    fd_flags |= Fd::READ;
                }
                if *write {
                    // TODO: introduce API for finer grained control
                    fd_flags |= Fd::WRITE | Fd::APPEND | Fd::TRUNCATE;
                }
                if *create {
                    fd_flags |= Fd::CREATE;
                }
                fd_flags
            };
            let fd = wasi_fs
                .create_fd(rights, rights, Fdflags::empty(), fd_flags, inode)
                .map_err(|e| format!("Could not open fd for file {:?}: {}", path, e))?;
            {
                let mut guard = inodes.arena[root_inode].write();
                if let Kind::Root { entries } = guard.deref_mut() {
                    let key = if let Some(alias) = &alias {
                        alias.clone()
                    } else {
                        path.to_string_lossy().into_owned()
                    };
                    let existing_entry = entries.insert(key.clone(), inode);
                    if existing_entry.is_some() {
                        return Err(format!("Found duplicate entry for alias `{}`", key));
                    }
                    assert!(existing_entry.is_none())
                }
            }
            wasi_fs.preopen_fds.write().unwrap().push(fd);
        }

        Ok(wasi_fs)
    }

    /// Converts a relative path into an absolute path
    pub(crate) fn relative_path_to_absolute(&self, mut path: String) -> String {
        if path.starts_with("./") {
            let current_dir = self.current_dir.lock().unwrap();
            path = format!("{}{}", current_dir.as_str(), &path[1..]);
            path = path.replace("//", "/").to_string();
        }
        path
    }

    /// Private helper function to init the filesystem, called in `new` and
    /// `new_with_preopen`
    fn new_init(fs_backing: WasiFsRoot, inodes: &mut WasiInodes) -> Result<(Self, Inode), String> {
        debug!("Initializing WASI filesystem");

        let wasi_fs = Self {
            preopen_fds: RwLock::new(vec![]),
            name_map: HashMap::new(),
            fd_map: Arc::new(RwLock::new(HashMap::new())),
            next_fd: AtomicU32::new(3),
            inode_counter: AtomicU64::new(1024),
            current_dir: Mutex::new("/".to_string()),
            #[cfg(feature = "wasix")]
            is_wasix: AtomicBool::new(false),
            root_fs: fs_backing,
            has_unioned: Arc::new(Mutex::new(HashSet::new())),
        };
        wasi_fs.create_stdin(inodes);
        wasi_fs.create_stdout(inodes);
        wasi_fs.create_stderr(inodes);

        // create virtual root
        let root_inode = {
            let all_rights = ALL_RIGHTS;
            // TODO: make this a list of positive rigths instead of negative ones
            // root gets all right for now
            let root_rights = all_rights
                /*
                & (!Rights::FD_WRITE)
                & (!Rights::FD_ALLOCATE)
                & (!Rights::PATH_CREATE_DIRECTORY)
                & (!Rights::PATH_CREATE_FILE)
                & (!Rights::PATH_LINK_SOURCE)
                & (!Rights::PATH_RENAME_SOURCE)
                & (!Rights::PATH_RENAME_TARGET)
                & (!Rights::PATH_FILESTAT_SET_SIZE)
                & (!Rights::PATH_FILESTAT_SET_TIMES)
                & (!Rights::FD_FILESTAT_SET_SIZE)
                & (!Rights::FD_FILESTAT_SET_TIMES)
                & (!Rights::PATH_SYMLINK)
                & (!Rights::PATH_UNLINK_FILE)
                & (!Rights::PATH_REMOVE_DIRECTORY)
                */;
            let inode = wasi_fs.create_virtual_root(inodes);
            let fd = wasi_fs
                .create_fd(root_rights, root_rights, Fdflags::empty(), Fd::READ, inode)
                .map_err(|e| format!("Could not create root fd: {}", e))?;
            wasi_fs.preopen_fds.write().unwrap().push(fd);
            inode
        };

        Ok((wasi_fs, root_inode))
    }

    /// Returns the next available inode index for creating a new inode.
    fn get_next_inode_index(&self) -> u64 {
        self.inode_counter.fetch_add(1, Ordering::AcqRel)
    }

    /// This function is like create dir all, but it also opens it.
    /// Function is unsafe because it may break invariants and hasn't been tested.
    /// This is an experimental function and may be removed
    ///
    /// # Safety
    /// - Virtual directories created with this function must not conflict with
    ///   the standard operation of the WASI filesystem.  This is vague and
    ///   unlikely in pratice.  [Join the discussion](https://github.com/wasmerio/wasmer/issues/1219)
    ///   for what the newer, safer WASI FS APIs should look like.
    #[allow(dead_code)]
    pub unsafe fn open_dir_all(
        &mut self,
        inodes: &mut WasiInodes,
        base: WasiFd,
        name: String,
        rights: Rights,
        rights_inheriting: Rights,
        flags: Fdflags,
    ) -> Result<WasiFd, FsError> {
        // TODO: check permissions here? probably not, but this should be
        // an explicit choice, so justify it in a comment when we remove this one
        let mut cur_inode = self.get_fd_inode(base).map_err(fs_error_from_wasi_err)?;

        let path: &Path = Path::new(&name);
        //let n_components = path.components().count();
        for c in path.components() {
            let segment_name = c.as_os_str().to_string_lossy().to_string();
            let guard = inodes.arena[cur_inode].read();
            match guard.deref() {
                Kind::Dir { ref entries, .. } | Kind::Root { ref entries } => {
                    if let Some(_entry) = entries.get(&segment_name) {
                        // TODO: this should be fixed
                        return Err(FsError::AlreadyExists);
                    }

                    let kind = Kind::Dir {
                        parent: Some(cur_inode),
                        path: PathBuf::from(""),
                        entries: HashMap::new(),
                    };

                    drop(guard);
                    let inode = self.create_inode_with_default_stat(
                        inodes,
                        kind,
                        false,
                        segment_name.clone().into(),
                    );

                    // reborrow to insert
                    {
                        let mut guard = inodes.arena[cur_inode].write();
                        match guard.deref_mut() {
                            Kind::Dir {
                                ref mut entries, ..
                            }
                            | Kind::Root { ref mut entries } => {
                                entries.insert(segment_name, inode);
                            }
                            _ => unreachable!("Dir or Root became not Dir or Root"),
                        }
                    }
                    cur_inode = inode;
                }
                _ => return Err(FsError::BaseNotDirectory),
            }
        }

        // TODO: review open flags (read, write); they were added without consideration
        self.create_fd(
            rights,
            rights_inheriting,
            flags,
            Fd::READ | Fd::WRITE,
            cur_inode,
        )
        .map_err(fs_error_from_wasi_err)
    }

    /// Opens a user-supplied file in the directory specified with the
    /// name and flags given
    // dead code because this is an API for external use
    #[allow(dead_code)]
    pub fn open_file_at(
        &mut self,
        inodes: &mut WasiInodes,
        base: WasiFd,
        file: Box<dyn VirtualFile + Send + Sync + 'static>,
        open_flags: u16,
        name: String,
        rights: Rights,
        rights_inheriting: Rights,
        flags: Fdflags,
    ) -> Result<WasiFd, FsError> {
        // TODO: check permissions here? probably not, but this should be
        // an explicit choice, so justify it in a comment when we remove this one
        let base_inode = self.get_fd_inode(base).map_err(fs_error_from_wasi_err)?;

        let guard = inodes.arena[base_inode].read();
        match guard.deref() {
            Kind::Dir { ref entries, .. } | Kind::Root { ref entries } => {
                if let Some(_entry) = entries.get(&name) {
                    // TODO: eventually change the logic here to allow overwrites
                    return Err(FsError::AlreadyExists);
                }

                let kind = Kind::File {
                    handle: Some(Arc::new(RwLock::new(file))),
                    path: PathBuf::from(""),
                    fd: Some(self.next_fd.load(Ordering::Acquire)),
                };

                drop(guard);
                let inode = self
                    .create_inode(inodes, kind, false, name.clone())
                    .map_err(|_| FsError::IOError)?;

                {
                    let mut guard = inodes.arena[base_inode].write();
                    match guard.deref_mut() {
                        Kind::Dir {
                            ref mut entries, ..
                        }
                        | Kind::Root { ref mut entries } => {
                            entries.insert(name, inode);
                        }
                        _ => unreachable!("Dir or Root became not Dir or Root"),
                    }
                }

                self.create_fd(rights, rights_inheriting, flags, open_flags, inode)
                    .map_err(fs_error_from_wasi_err)
            }
            _ => Err(FsError::BaseNotDirectory),
        }
    }

    /// Change the backing of a given file descriptor
    /// Returns the old backing
    /// TODO: add examples
    #[allow(dead_code)]
    pub fn swap_file(
        &self,
        inodes: &WasiInodes,
        fd: WasiFd,
        mut file: Box<dyn VirtualFile + Send + Sync + 'static>,
    ) -> Result<Option<Box<dyn VirtualFile + Send + Sync + 'static>>, FsError> {
        match fd {
            __WASI_STDIN_FILENO => {
                let mut target = inodes.stdin_mut(&self.fd_map)?;
                Ok(Some(target.swap(file)))
            }
            __WASI_STDOUT_FILENO => {
                let mut target = inodes.stdout_mut(&self.fd_map)?;
                Ok(Some(target.swap(file)))
            }
            __WASI_STDERR_FILENO => {
                let mut target = inodes.stderr_mut(&self.fd_map)?;
                Ok(Some(target.swap(file)))
            }
            _ => {
                let base_inode = self.get_fd_inode(fd).map_err(fs_error_from_wasi_err)?;
                {
                    // happy path
                    let guard = inodes.arena[base_inode].read();
                    match guard.deref() {
                        Kind::File { ref handle, .. } => {
                            if let Some(handle) = handle {
                                let mut handle = handle.write().unwrap();
                                std::mem::swap(handle.deref_mut(), &mut file);
                                return Ok(Some(file));
                            }
                        }
                        _ => return Err(FsError::NotAFile),
                    }
                }
                // slow path
                let mut guard = inodes.arena[base_inode].write();
                match guard.deref_mut() {
                    Kind::File { ref mut handle, .. } => {
                        if let Some(handle) = handle {
                            let mut handle = handle.write().unwrap();
                            std::mem::swap(handle.deref_mut(), &mut file);
                            return Ok(Some(file));
                        } else {
                            handle.replace(Arc::new(RwLock::new(file)));
                            Ok(None)
                        }
                    }
                    _ => return Err(FsError::NotAFile),
                }
            }
        }
    }

    /// refresh size from filesystem
    pub(crate) fn filestat_resync_size(
        &self,
        inodes: &WasiInodes,
        fd: WasiFd,
    ) -> Result<Filesize, Errno> {
        let inode = self.get_fd_inode(fd)?;
        let mut guard = inodes.arena[inode].write();
        match guard.deref_mut() {
            Kind::File { handle, .. } => {
                if let Some(h) = handle {
                    let h = h.read().unwrap();
                    let new_size = h.size().clone();
                    drop(h);
                    drop(guard);

                    inodes.arena[inode].stat.write().unwrap().st_size = new_size;
                    Ok(new_size as Filesize)
                } else {
                    Err(Errno::Badf)
                }
            }
            Kind::Dir { .. } | Kind::Root { .. } => Err(Errno::Isdir),
            _ => Err(Errno::Inval),
        }
    }

    /// Changes the current directory
    pub fn set_current_dir(&self, path: &str) {
        let mut guard = self.current_dir.lock().unwrap();
        *guard = path.to_string();
    }

    /// Gets the current directory
    pub fn get_current_dir(
        &self,
        inodes: &mut WasiInodes,
        base: WasiFd,
    ) -> Result<(Inode, String), Errno> {
        self.get_current_dir_inner(inodes, base, 0)
    }

    pub(crate) fn get_current_dir_inner(
        &self,
        inodes: &mut WasiInodes,
        base: WasiFd,
        symlink_count: u32,
    ) -> Result<(Inode, String), Errno> {
        let current_dir = {
            let guard = self.current_dir.lock().unwrap();
            guard.clone()
        };
        let cur_inode = self.get_fd_inode(base)?;
        let inode = self.get_inode_at_path_inner(
            inodes,
            cur_inode,
            current_dir.as_str(),
            symlink_count,
            true,
        )?;
        Ok((inode, current_dir))
    }

    /// Internal part of the core path resolution function which implements path
    /// traversal logic such as resolving relative path segments (such as
    /// `.` and `..`) and resolving symlinks (while preventing infinite
    /// loops/stack overflows).
    ///
    /// TODO: expand upon exactly what the state of the returned value is,
    /// explaining lazy-loading from the real file system and synchronizing
    /// between them.
    ///
    /// This is where a lot of the magic happens, be very careful when editing
    /// this code.
    ///
    /// TODO: write more tests for this code
    fn get_inode_at_path_inner(
        &self,
        inodes: &mut WasiInodes,
        mut cur_inode: generational_arena::Index,
        path: &str,
        mut symlink_count: u32,
        follow_symlinks: bool,
    ) -> Result<Inode, Errno> {
        if symlink_count > MAX_SYMLINKS {
            return Err(Errno::Mlink);
        }

        let path: &Path = Path::new(path);
        let n_components = path.components().count();

        // TODO: rights checks
        'path_iter: for (i, component) in path.components().enumerate() {
            // used to terminate symlink resolution properly
            let last_component = i + 1 == n_components;
            // for each component traverse file structure
            // loading inodes as necessary
            'symlink_resolution: while symlink_count < MAX_SYMLINKS {
                let mut guard = inodes.arena[cur_inode].write();
                match guard.deref_mut() {
                    Kind::Buffer { .. } => unimplemented!("state::get_inode_at_path for buffers"),
                    Kind::Dir {
                        ref mut entries,
                        ref path,
                        ref parent,
                        ..
                    } => {
                        match component.as_os_str().to_string_lossy().borrow() {
                            ".." => {
                                if let Some(p) = parent {
                                    cur_inode = *p;
                                    continue 'path_iter;
                                } else {
                                    return Err(Errno::Access);
                                }
                            }
                            "." => continue 'path_iter,
                            _ => (),
                        }
                        // used for full resolution of symlinks
                        let mut loop_for_symlink = false;
                        if let Some(entry) =
                            entries.get(component.as_os_str().to_string_lossy().as_ref())
                        {
                            cur_inode = *entry;
                        } else {
                            let file = {
                                let mut cd = path.clone();
                                cd.push(component);
                                cd
                            };
                            let metadata = self
                                .root_fs
                                .symlink_metadata(&file)
                                .ok()
                                .ok_or(Errno::Noent)?;
                            let file_type = metadata.file_type();
                            // we want to insert newly opened dirs and files, but not transient symlinks
                            // TODO: explain why (think about this deeply when well rested)
                            let should_insert;

                            let kind = if file_type.is_dir() {
                                should_insert = true;
                                // load DIR
                                Kind::Dir {
                                    parent: Some(cur_inode),
                                    path: file.clone(),
                                    entries: Default::default(),
                                }
                            } else if file_type.is_file() {
                                should_insert = true;
                                // load file
                                Kind::File {
                                    handle: None,
                                    path: file.clone(),
                                    fd: None,
                                }
                            } else if file_type.is_symlink() {
                                should_insert = false;
                                let link_value = file.read_link().map_err(map_io_err)?;
                                debug!("attempting to decompose path {:?}", link_value);

                                let (pre_open_dir_fd, relative_path) = if link_value.is_relative() {
                                    self.path_into_pre_open_and_relative_path(inodes, &file)?
                                } else {
                                    unimplemented!("Absolute symlinks are not yet supported");
                                };
                                loop_for_symlink = true;
                                symlink_count += 1;
                                Kind::Symlink {
                                    base_po_dir: pre_open_dir_fd,
                                    path_to_symlink: relative_path.to_owned(),
                                    relative_path: link_value,
                                }
                            } else {
                                #[cfg(unix)]
                                {
                                    //use std::os::unix::fs::FileTypeExt;
                                    let file_type: Filetype = if file_type.is_char_device() {
                                        Filetype::CharacterDevice
                                    } else if file_type.is_block_device() {
                                        Filetype::BlockDevice
                                    } else if file_type.is_fifo() {
                                        // FIFO doesn't seem to fit any other type, so unknown
                                        Filetype::Unknown
                                    } else if file_type.is_socket() {
                                        // TODO: how do we know if it's a `SocketStream` or
                                        // a `SocketDgram`?
                                        Filetype::SocketStream
                                    } else {
                                        unimplemented!("state::get_inode_at_path unknown file type: not file, directory, symlink, char device, block device, fifo, or socket");
                                    };

                                    let kind = Kind::File {
                                        handle: None,
                                        path: file.clone(),
                                        fd: None,
                                    };
                                    drop(guard);
                                    let new_inode = self.create_inode_with_stat(
                                        inodes,
                                        kind,
                                        false,
                                        file.to_string_lossy().to_string().into(),
                                        Filestat {
                                            st_filetype: file_type,
                                            ..Filestat::default()
                                        },
                                    );

                                    let mut guard = inodes.arena[cur_inode].write();
                                    if let Kind::Dir {
                                        ref mut entries, ..
                                    } = guard.deref_mut()
                                    {
                                        entries.insert(
                                            component.as_os_str().to_string_lossy().to_string(),
                                            new_inode,
                                        );
                                    } else {
                                        unreachable!(
                                            "Attempted to insert special device into non-directory"
                                        );
                                    }
                                    // perhaps just continue with symlink resolution and return at the end
                                    return Ok(new_inode);
                                }
                                #[cfg(not(unix))]
                                unimplemented!("state::get_inode_at_path unknown file type: not file, directory, or symlink");
                            };

                            drop(guard);
                            let new_inode = self.create_inode(
                                inodes,
                                kind,
                                false,
                                file.to_string_lossy().to_string(),
                            )?;
                            if should_insert {
                                let mut guard = inodes.arena[cur_inode].write();
                                if let Kind::Dir {
                                    ref mut entries, ..
                                } = guard.deref_mut()
                                {
                                    entries.insert(
                                        component.as_os_str().to_string_lossy().to_string(),
                                        new_inode,
                                    );
                                }
                            }
                            cur_inode = new_inode;

                            if loop_for_symlink && follow_symlinks {
                                debug!("Following symlink to {:?}", cur_inode);
                                continue 'symlink_resolution;
                            }
                        }
                    }
                    Kind::Root { entries } => {
                        match component.as_os_str().to_string_lossy().borrow() {
                            // the root's parent is the root
                            ".." => continue 'path_iter,
                            // the root's current directory is the root
                            "." => continue 'path_iter,
                            _ => (),
                        }

                        if let Some(entry) =
                            entries.get(component.as_os_str().to_string_lossy().as_ref())
                        {
                            cur_inode = *entry;
                        } else {
                            // Root is not capable of having something other then preopenned folders
                            return Err(Errno::Notcapable);
                        }
                    }
                    Kind::File { .. }
                    | Kind::Socket { .. }
                    | Kind::Pipe { .. }
                    | Kind::EventNotifications { .. } => {
                        return Err(Errno::Notdir);
                    }
                    Kind::Symlink {
                        base_po_dir,
                        path_to_symlink,
                        relative_path,
                    } => {
                        let new_base_dir = *base_po_dir;
                        let new_base_inode = self.get_fd_inode(new_base_dir)?;

                        // allocate to reborrow mutabily to recur
                        let new_path = {
                            /*if let Kind::Root { .. } = self.inodes[base_po_dir].kind {
                                assert!(false, "symlinks should never be relative to the root");
                            }*/
                            let mut base = path_to_symlink.clone();
                            // remove the symlink file itself from the path, leaving just the path from the base
                            // to the dir containing the symlink
                            base.pop();
                            base.push(relative_path);
                            base.to_string_lossy().to_string()
                        };
                        debug!("Following symlink recursively");
                        drop(guard);
                        let symlink_inode = self.get_inode_at_path_inner(
                            inodes,
                            new_base_inode,
                            &new_path,
                            symlink_count + 1,
                            follow_symlinks,
                        )?;
                        cur_inode = symlink_inode;
                        // if we're at the very end and we found a file, then we're done
                        // TODO: figure out if this should also happen for directories?
                        let guard = inodes.arena[cur_inode].read();
                        if let Kind::File { .. } = guard.deref() {
                            // check if on last step
                            if last_component {
                                break 'symlink_resolution;
                            }
                        }
                        continue 'symlink_resolution;
                    }
                }
                break 'symlink_resolution;
            }
        }

        Ok(cur_inode)
    }

    /// Finds the preopened directory that is the "best match" for the given path and
    /// returns a path relative to this preopened directory.
    ///
    /// The "best match" is the preopened directory that has the longest prefix of the
    /// given path. For example, given preopened directories [`a`, `a/b`, `a/c`] and
    /// the path `a/b/c/file`, we will return the fd corresponding to the preopened
    /// directory, `a/b` and the relative path `c/file`.
    ///
    /// In the case of a tie, the later preopened fd is preferred.
    fn path_into_pre_open_and_relative_path<'path>(
        &self,
        inodes: &WasiInodes,
        path: &'path Path,
    ) -> Result<(WasiFd, &'path Path), Errno> {
        enum BaseFdAndRelPath<'a> {
            None,
            BestMatch {
                fd: WasiFd,
                rel_path: &'a Path,
                max_seen: usize,
            },
        }

        impl<'a> BaseFdAndRelPath<'a> {
            const fn max_seen(&self) -> usize {
                match self {
                    Self::None => 0,
                    Self::BestMatch { max_seen, .. } => *max_seen,
                }
            }
        }
        let mut res = BaseFdAndRelPath::None;
        // for each preopened directory
        let preopen_fds = self.preopen_fds.read().unwrap();
        for po_fd in preopen_fds.deref() {
            let po_inode = self.fd_map.read().unwrap()[po_fd].inode;
            let guard = inodes.arena[po_inode].read();
            let po_path = match guard.deref() {
                Kind::Dir { path, .. } => &**path,
                Kind::Root { .. } => Path::new("/"),
                _ => unreachable!("Preopened FD that's not a directory or the root"),
            };
            // stem path based on it
            if let Ok(stripped_path) = path.strip_prefix(po_path) {
                // find the max
                let new_prefix_len = po_path.as_os_str().len();
                // we use >= to favor later preopens because we iterate in order
                // whereas WASI libc iterates in reverse to get this behavior.
                if new_prefix_len >= res.max_seen() {
                    res = BaseFdAndRelPath::BestMatch {
                        fd: *po_fd,
                        rel_path: stripped_path,
                        max_seen: new_prefix_len,
                    };
                }
            }
        }
        match res {
            // this error may not make sense depending on where it's called
            BaseFdAndRelPath::None => Err(Errno::Inval),
            BaseFdAndRelPath::BestMatch { fd, rel_path, .. } => Ok((fd, rel_path)),
        }
    }

    /// finds the number of directories between the fd and the inode if they're connected
    /// expects inode to point to a directory
    pub(crate) fn path_depth_from_fd(
        &self,
        inodes: &WasiInodes,
        fd: WasiFd,
        inode: Inode,
    ) -> Result<usize, Errno> {
        let mut counter = 0;
        let base_inode = self.get_fd_inode(fd)?;
        let mut cur_inode = inode;

        while cur_inode != base_inode {
            counter += 1;
            let guard = inodes.arena[cur_inode].read();
            match guard.deref() {
                Kind::Dir { parent, .. } => {
                    if let Some(p) = parent {
                        cur_inode = *p;
                    }
                }
                _ => return Err(Errno::Inval),
            }
        }

        Ok(counter)
    }

    /// gets a host file from a base directory and a path
    /// this function ensures the fs remains sandboxed
    // NOTE: follow symlinks is super weird right now
    // even if it's false, it still follows symlinks, just not the last
    // symlink so
    // This will be resolved when we have tests asserting the correct behavior
    pub(crate) fn get_inode_at_path(
        &self,
        inodes: &mut WasiInodes,
        base: WasiFd,
        path: &str,
        follow_symlinks: bool,
    ) -> Result<Inode, Errno> {
        #[cfg(feature = "wasix")]
        let start_inode = if !path.starts_with('/') && self.is_wasix.load(Ordering::Acquire) {
            let (cur_inode, _) = self.get_current_dir(inodes, base)?;
            cur_inode
        } else {
            self.get_fd_inode(base)?
        };
        #[cfg(not(feature = "wasix"))]
        let start_inode = self.get_fd_inode(base)?;

        self.get_inode_at_path_inner(inodes, start_inode, path, 0, follow_symlinks)
    }

    /// Returns the parent Dir or Root that the file at a given path is in and the file name
    /// stripped off
    pub(crate) fn get_parent_inode_at_path(
        &self,
        inodes: &mut WasiInodes,
        base: WasiFd,
        path: &Path,
        follow_symlinks: bool,
    ) -> Result<(Inode, String), Errno> {
        let mut parent_dir = std::path::PathBuf::new();
        let mut components = path.components().rev();
        let new_entity_name = components
            .next()
            .ok_or(Errno::Inval)?
            .as_os_str()
            .to_string_lossy()
            .to_string();
        for comp in components.rev() {
            parent_dir.push(comp);
        }
        self.get_inode_at_path(inodes, base, &parent_dir.to_string_lossy(), follow_symlinks)
            .map(|v| (v, new_entity_name))
    }

    pub fn get_fd(&self, fd: WasiFd) -> Result<Fd, Errno> {
        self.fd_map
            .read()
            .unwrap()
            .get(&fd)
            .ok_or(Errno::Badf)
            .map(|a| a.clone())
    }

    pub fn get_fd_inode(&self, fd: WasiFd) -> Result<generational_arena::Index, Errno> {
        self.fd_map
            .read()
            .unwrap()
            .get(&fd)
            .ok_or(Errno::Badf)
            .map(|a| a.inode)
    }

    pub fn filestat_fd(&self, inodes: &WasiInodes, fd: WasiFd) -> Result<Filestat, Errno> {
        let inode = self.get_fd_inode(fd)?;
        Ok(*inodes.arena[inode].stat.read().unwrap().deref())
    }

    pub fn fdstat(&self, inodes: &WasiInodes, fd: WasiFd) -> Result<Fdstat, Errno> {
        match fd {
            __WASI_STDIN_FILENO => {
                return Ok(Fdstat {
                    fs_filetype: Filetype::CharacterDevice,
                    fs_flags: Fdflags::empty(),
                    fs_rights_base: STDIN_DEFAULT_RIGHTS,
                    fs_rights_inheriting: Rights::empty(),
                })
            }
            __WASI_STDOUT_FILENO => {
                return Ok(Fdstat {
                    fs_filetype: Filetype::CharacterDevice,
                    fs_flags: Fdflags::APPEND,
                    fs_rights_base: STDOUT_DEFAULT_RIGHTS,
                    fs_rights_inheriting: Rights::empty(),
                })
            }
            __WASI_STDERR_FILENO => {
                return Ok(Fdstat {
                    fs_filetype: Filetype::CharacterDevice,
                    fs_flags: Fdflags::APPEND,
                    fs_rights_base: STDERR_DEFAULT_RIGHTS,
                    fs_rights_inheriting: Rights::empty(),
                })
            }
            VIRTUAL_ROOT_FD => {
                return Ok(Fdstat {
                    fs_filetype: Filetype::Directory,
                    fs_flags: Fdflags::empty(),
                    // TODO: fix this
                    fs_rights_base: ALL_RIGHTS,
                    fs_rights_inheriting: ALL_RIGHTS,
                });
            }
            _ => (),
        }
        let fd = self.get_fd(fd)?;
        debug!("fdstat: {:?}", fd);

        let guard = inodes.arena[fd.inode].read();
        let deref = guard.deref();
        Ok(Fdstat {
            fs_filetype: match deref {
                Kind::File { .. } => Filetype::RegularFile,
                Kind::Dir { .. } => Filetype::Directory,
                Kind::Symlink { .. } => Filetype::SymbolicLink,
                _ => Filetype::Unknown,
            },
            fs_flags: fd.flags,
            fs_rights_base: fd.rights,
            fs_rights_inheriting: fd.rights_inheriting, // TODO(lachlan): Is this right?
        })
    }

    pub fn prestat_fd(&self, inodes: &WasiInodes, fd: WasiFd) -> Result<Prestat, Errno> {
        let inode = self.get_fd_inode(fd)?;
        //trace!("in prestat_fd {:?}", self.get_fd(fd)?);

        let inode_val = &inodes.arena[inode];

        if inode_val.is_preopened {
            Ok(self.prestat_fd_inner(inode_val))
        } else {
            Err(Errno::Badf)
        }
    }

    pub(crate) fn prestat_fd_inner(&self, inode_val: &InodeVal) -> Prestat {
        Prestat {
            pr_type: Preopentype::Dir,
            u: PrestatEnum::Dir {
                // REVIEW:
                // no need for +1, because there is no 0 end-of-string marker
                // john: removing the +1 seems cause regression issues
                pr_name_len: inode_val.name.len() as u32 + 1,
            }
            .untagged(),
        }
    }

    pub async fn flush(&self, inodes: &WasiInodes, fd: WasiFd) -> Result<(), Errno> {
        match fd {
            __WASI_STDIN_FILENO => (),
            __WASI_STDOUT_FILENO => inodes
                .stdout_mut(&self.fd_map)
                .map_err(fs_error_into_wasi_err)?
                .flush()
                .await
                .map_err(map_io_err)?,
            __WASI_STDERR_FILENO => inodes
                .stderr_mut(&self.fd_map)
                .map_err(fs_error_into_wasi_err)?
                .flush()
                .await
                .map_err(map_io_err)?,
            _ => {
                let fd = self.get_fd(fd)?;
                if fd.rights.contains(Rights::FD_DATASYNC) {
                    return Err(Errno::Access);
                }

                let guard = inodes.arena[fd.inode].read();
                match guard.deref() {
                    Kind::File {
                        handle: Some(file), ..
                    } => {
                        let mut file = file.write().unwrap();
                        file.flush().await.map_err(|_| Errno::Io)?
                    }
                    // TODO: verify this behavior
                    Kind::Dir { .. } => return Err(Errno::Isdir),
                    Kind::Symlink { .. } => unimplemented!("WasiFs::flush Kind::Symlink"),
                    Kind::Buffer { .. } => (),
                    _ => return Err(Errno::Io),
                }
            }
        }
        Ok(())
    }

    /// Creates an inode and inserts it given a Kind and some extra data
    pub(crate) fn create_inode(
        &self,
        inodes: &mut WasiInodes,
        kind: Kind,
        is_preopened: bool,
        name: String,
    ) -> Result<Inode, Errno> {
        let stat = self.get_stat_for_kind(inodes, &kind)?;
        Ok(self.create_inode_with_stat(inodes, kind, is_preopened, name.into(), stat))
    }

    /// Creates an inode and inserts it given a Kind, does not assume the file exists.
    pub(crate) fn create_inode_with_default_stat(
        &self,
        inodes: &mut WasiInodes,
        kind: Kind,
        is_preopened: bool,
        name: Cow<'static, str>,
    ) -> Inode {
        let stat = Filestat::default();
        self.create_inode_with_stat(inodes, kind, is_preopened, name, stat)
    }

    /// Creates an inode with the given filestat and inserts it.
    pub(crate) fn create_inode_with_stat(
        &self,
        inodes: &mut WasiInodes,
        kind: Kind,
        is_preopened: bool,
        name: Cow<'static, str>,
        mut stat: Filestat,
    ) -> Inode {
        stat.st_ino = self.get_next_inode_index();

        inodes.arena.insert(InodeVal {
            stat: RwLock::new(stat),
            is_preopened,
            name,
            kind: RwLock::new(kind),
        })
    }

    pub fn create_fd(
        &self,
        rights: Rights,
        rights_inheriting: Rights,
        flags: Fdflags,
        open_flags: u16,
        inode: Inode,
    ) -> Result<WasiFd, Errno> {
        let idx = self.next_fd.fetch_add(1, Ordering::AcqRel);
        self.create_fd_ext(rights, rights_inheriting, flags, open_flags, inode, idx)?;
        Ok(idx)
    }

    pub fn create_fd_ext(
        &self,
        rights: Rights,
        rights_inheriting: Rights,
        flags: Fdflags,
        open_flags: u16,
        inode: Inode,
        idx: WasiFd,
    ) -> Result<(), Errno> {
        self.fd_map.write().unwrap().insert(
            idx,
            Fd {
                ref_cnt: Arc::new(AtomicU32::new(1)),
                rights,
                rights_inheriting,
                flags,
                offset: Arc::new(AtomicU64::new(0)),
                open_flags,
                inode,
            },
        );
        Ok(())
    }

    pub fn clone_fd(&self, fd: WasiFd) -> Result<WasiFd, Errno> {
        let fd = self.get_fd(fd)?;
        let idx = self.next_fd.fetch_add(1, Ordering::AcqRel);
        fd.ref_cnt.fetch_add(1, Ordering::Acquire);
        self.fd_map.write().unwrap().insert(
            idx,
            Fd {
                ref_cnt: fd.ref_cnt.clone(),
                rights: fd.rights,
                rights_inheriting: fd.rights_inheriting,
                flags: fd.flags,
                offset: fd.offset.clone(),
                open_flags: fd.open_flags,
                inode: fd.inode,
            },
        );
        Ok(idx)
    }

    /// Low level function to remove an inode, that is it deletes the WASI FS's
    /// knowledge of a file.
    ///
    /// This function returns the inode if it existed and was removed.
    ///
    /// # Safety
    /// - The caller must ensure that all references to the specified inode have
    ///   been removed from the filesystem.
    pub unsafe fn remove_inode(&self, inodes: &mut WasiInodes, inode: Inode) -> Option<InodeVal> {
        inodes.arena.remove(inode)
    }

    fn create_virtual_root(&self, inodes: &mut WasiInodes) -> Inode {
        let stat = Filestat {
            st_filetype: Filetype::Directory,
            st_ino: self.get_next_inode_index(),
            ..Filestat::default()
        };
        let root_kind = Kind::Root {
            entries: HashMap::new(),
        };

        inodes.arena.insert(InodeVal {
            stat: RwLock::new(stat),
            is_preopened: true,
            name: "/".into(),
            kind: RwLock::new(root_kind),
        })
    }

    fn create_stdout(&self, inodes: &mut WasiInodes) {
        self.create_std_dev_inner(
            inodes,
            Box::new(Stdout::default()),
            "stdout",
            __WASI_STDOUT_FILENO,
            STDOUT_DEFAULT_RIGHTS,
            Fdflags::APPEND,
        );
    }
    fn create_stdin(&self, inodes: &mut WasiInodes) {
        self.create_std_dev_inner(
            inodes,
            Box::new(Stdin::default()),
            "stdin",
            __WASI_STDIN_FILENO,
            STDIN_DEFAULT_RIGHTS,
            Fdflags::empty(),
        );
    }
    fn create_stderr(&self, inodes: &mut WasiInodes) {
        self.create_std_dev_inner(
            inodes,
            Box::new(Stderr::default()),
            "stderr",
            __WASI_STDERR_FILENO,
            STDERR_DEFAULT_RIGHTS,
            Fdflags::APPEND,
        );
    }

    fn create_std_dev_inner(
        &self,
        inodes: &mut WasiInodes,
        handle: Box<dyn VirtualFile + Send + Sync + 'static>,
        name: &'static str,
        raw_fd: WasiFd,
        rights: Rights,
        fd_flags: Fdflags,
    ) {
        let stat = Filestat {
            st_filetype: Filetype::CharacterDevice,
            st_ino: self.get_next_inode_index(),
            ..Filestat::default()
        };
        let kind = Kind::File {
            fd: Some(raw_fd),
            handle: Some(Arc::new(RwLock::new(handle))),
            path: "".into(),
        };
        let inode = {
            inodes.arena.insert(InodeVal {
                stat: RwLock::new(stat),
                is_preopened: true,
                name: name.to_string().into(),
                kind: RwLock::new(kind),
            })
        };
        self.fd_map.write().unwrap().insert(
            raw_fd,
            Fd {
                ref_cnt: Arc::new(AtomicU32::new(1)),
                rights,
                rights_inheriting: Rights::empty(),
                flags: fd_flags,
                // since we're not calling open on this, we don't need open flags
                open_flags: 0,
                offset: Arc::new(AtomicU64::new(0)),
                inode,
            },
        );
    }

    pub fn get_stat_for_kind(&self, inodes: &WasiInodes, kind: &Kind) -> Result<Filestat, Errno> {
        let md = match kind {
            Kind::File { handle, path, .. } => match handle {
                Some(wf) => {
                    let wf = wf.read().unwrap();
                    return Ok(Filestat {
                        st_filetype: Filetype::RegularFile,
                        st_size: wf.size(),
                        st_atim: wf.last_accessed(),
                        st_mtim: wf.last_modified(),
                        st_ctim: wf.created_time(),

                        ..Filestat::default()
                    });
                }
                None => self
                    .root_fs
                    .metadata(path)
                    .map_err(fs_error_into_wasi_err)?,
            },
            Kind::Dir { path, .. } => self
                .root_fs
                .metadata(path)
                .map_err(fs_error_into_wasi_err)?,
            Kind::Symlink {
                base_po_dir,
                path_to_symlink,
                ..
            } => {
                let base_po_inode = &self.fd_map.read().unwrap()[base_po_dir].inode;
                let base_po_inode_v = &inodes.arena[*base_po_inode];
                let guard = base_po_inode_v.read();
                match guard.deref() {
                    Kind::Root { .. } => {
                        self.root_fs.symlink_metadata(path_to_symlink).map_err(fs_error_into_wasi_err)?
                    }
                    Kind::Dir { path, .. } => {
                        let mut real_path = path.clone();
                        // PHASE 1: ignore all possible symlinks in `relative_path`
                        // TODO: walk the segments of `relative_path` via the entries of the Dir
                        //       use helper function to avoid duplicating this logic (walking this will require
                        //       &self to be &mut sel
                        // TODO: adjust size of symlink, too
                        //      for all paths adjusted think about this
                        real_path.push(path_to_symlink);
                        self.root_fs.symlink_metadata(&real_path).map_err(fs_error_into_wasi_err)?
                    }
                    // if this triggers, there's a bug in the symlink code
                    _ => unreachable!("Symlink pointing to something that's not a directory as its base preopened directory"),
                }
            }
            _ => return Err(Errno::Io),
        };
        Ok(Filestat {
            st_filetype: virtual_file_type_to_wasi_file_type(md.file_type()),
            st_size: md.len(),
            st_atim: md.accessed(),
            st_mtim: md.modified(),
            st_ctim: md.created(),
            ..Filestat::default()
        })
    }

    /// Closes an open FD, handling all details such as FD being preopen
    pub(crate) fn close_fd(&self, inodes: &WasiInodes, fd: WasiFd) -> Result<(), Errno> {
        let mut fd_map = self.fd_map.write().unwrap();
        self.close_fd_ext(inodes, &mut fd_map, fd)
    }

    /// Closes an open FD, handling all details such as FD being preopen
    pub(crate) fn close_fd_ext(
        &self,
        inodes: &WasiInodes,
        fd_map: &mut RwLockWriteGuard<HashMap<WasiFd, Fd>>,
        fd: WasiFd,
    ) -> Result<(), Errno> {
        let pfd = fd_map.get(&fd).ok_or(Errno::Badf)?;
        if pfd.ref_cnt.fetch_sub(1, Ordering::AcqRel) > 1 {
            trace!("closing file descriptor({}) - ref-cnt", fd);
            fd_map.remove(&fd);
            return Ok(());
        }
        trace!("closing file descriptor({}) - inode", fd);

        let inode = pfd.inode;
        let inodeval = inodes.get_inodeval(inode)?;
        let is_preopened = inodeval.is_preopened;

        let mut guard = inodeval.write();
        match guard.deref_mut() {
            Kind::File { ref mut handle, .. } => {
                let mut empty_handle = None;
                std::mem::swap(handle, &mut empty_handle);
            }
            Kind::Socket { ref mut socket, .. } => {
                let mut closed_socket = InodeSocket::new(InodeSocketKind::Closed);
                std::mem::swap(socket, &mut closed_socket);
            }
            Kind::Pipe { ref mut pipe } => {
                pipe.close();
            }
            Kind::Dir { parent, path, .. } => {
                debug!("Closing dir {:?}", &path);
                let key = path
                    .file_name()
                    .ok_or(Errno::Inval)?
                    .to_string_lossy()
                    .to_string();
                if let Some(p) = *parent {
                    drop(guard);
                    let mut guard = inodes.arena[p].write();
                    match guard.deref_mut() {
                        Kind::Dir { entries, .. } | Kind::Root { entries } => {
                            fd_map.remove(&fd).unwrap();
                            if is_preopened {
                                let mut idx = None;
                                {
                                    let preopen_fds = self.preopen_fds.read().unwrap();
                                    for (i, po_fd) in preopen_fds.iter().enumerate() {
                                        if *po_fd == fd {
                                            idx = Some(i);
                                            break;
                                        }
                                    }
                                }
                                if let Some(i) = idx {
                                    // only remove entry properly if this is the original preopen FD
                                    // calling `path_open` can give you an fd to the same inode as a preopen fd
                                    entries.remove(&key);
                                    self.preopen_fds.write().unwrap().remove(i);
                                    // Maybe recursively closes fds if original preopen?
                                }
                            }
                        }
                        _ => unreachable!(
                            "Fatal internal logic error, directory's parent is not a directory"
                        ),
                    }
                } else {
                    // this shouldn't be possible anymore due to Root
                    debug!("HIT UNREACHABLE CODE! Non-root directory does not have a parent");
                    return Err(Errno::Inval);
                }
            }
            Kind::EventNotifications { .. } => {}
            Kind::Root { .. } => return Err(Errno::Access),
            Kind::Symlink { .. } | Kind::Buffer { .. } => return Err(Errno::Inval),
        }

        Ok(())
    }
}

// Implementations of direct to FS calls so that we can easily change their implementation
impl WasiState {
    pub(crate) fn fs_read_dir<P: AsRef<Path>>(
        &self,
        path: P,
    ) -> Result<wasmer_vfs::ReadDir, Errno> {
        self.fs
            .root_fs
            .read_dir(path.as_ref())
            .map_err(fs_error_into_wasi_err)
    }

    pub(crate) fn fs_create_dir<P: AsRef<Path>>(&self, path: P) -> Result<(), Errno> {
        self.fs
            .root_fs
            .create_dir(path.as_ref())
            .map_err(fs_error_into_wasi_err)
    }

    pub(crate) fn fs_remove_dir<P: AsRef<Path>>(&self, path: P) -> Result<(), Errno> {
        self.fs
            .root_fs
            .remove_dir(path.as_ref())
            .map_err(fs_error_into_wasi_err)
    }

    pub(crate) fn fs_rename<P: AsRef<Path>, Q: AsRef<Path>>(
        &self,
        from: P,
        to: Q,
    ) -> Result<(), Errno> {
        self.fs
            .root_fs
            .rename(from.as_ref(), to.as_ref())
            .map_err(fs_error_into_wasi_err)
    }

    pub(crate) fn fs_remove_file<P: AsRef<Path>>(&self, path: P) -> Result<(), Errno> {
        self.fs
            .root_fs
            .remove_file(path.as_ref())
            .map_err(fs_error_into_wasi_err)
    }

    pub(crate) fn fs_new_open_options(&self) -> OpenOptions {
        OpenOptions::new(Box::new(WasiStateOpener {
            root_fs: self.fs.root_fs.clone(),
        }))
    }
}

struct WasiStateOpener {
    root_fs: WasiFsRoot,
}

impl FileOpener for WasiStateOpener {
    fn open(
        &mut self,
        path: &Path,
        conf: &wasmer_vfs::OpenOptionsConfig,
    ) -> wasmer_vfs::Result<Box<dyn VirtualFile + Send + Sync + 'static>> {
        let mut new_options = self.root_fs.new_open_options();
        new_options.options(conf.clone());
        new_options.open(path)
    }
}

// TODO: review allow...
#[allow(dead_code)]
pub(crate) struct WasiThreadContext {
    pub ctx: WasiFunctionEnv,
    pub store: RefCell<Store>,
}

/// The code itself makes safe use of the struct so multiple threads don't access
/// it (without this the JS code prevents the reference to the module from being stored
/// which is needed for the multithreading mode)
unsafe impl Send for WasiThreadContext {}
unsafe impl Sync for WasiThreadContext {}

/// Structures used for the threading and sub-processes
///
/// These internal implementation details are hidden away from the
/// consumer who should instead implement the vbus trait on the runtime
#[derive(Derivative, Default)]
// TODO: review allow...
#[allow(dead_code)]
#[derivative(Debug)]
#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub(crate) struct WasiStateThreading {
    #[derivative(Debug = "ignore")]
    pub thread_ctx: HashMap<WasiCallingId, Arc<WasiThreadContext>>,
}

/// Represents a futex which will make threads wait for completion in a more
/// CPU efficient manner
#[derive(Debug, Clone)]
pub struct WasiFutex {
    pub(crate) refcnt: Arc<AtomicU32>,
    pub(crate) inner: Arc<Mutex<tokio::sync::broadcast::Sender<()>>>,
}

#[derive(Debug)]
pub struct WasiBusCall {
    pub bid: WasiProcessId,
    pub invocation: Box<dyn VirtualBusInvocation + Sync>,
}

/// Protected area of the BUS state
#[derive(Debug, Default)]
pub struct WasiBusProtectedState {
    pub call_seed: u64,
    pub called: HashMap<Cid, Box<dyn VirtualBusCalled + Sync + Unpin>>,
    pub calls: HashMap<Cid, WasiBusCall>,
}

/// Structure that holds the state of BUS calls to this process and from
/// this process. BUS calls are the equivalent of RPC's with support
/// for all the major serializers
#[derive(Debug, Default)]
pub struct WasiBusState {
    protected: Mutex<WasiBusProtectedState>,
    poll_waker: WasiParkingLot,
}

impl WasiBusState {
    /// Gets a reference to the waker that can be used for
    /// asynchronous calls
    // TODO: review allow...
    #[allow(dead_code)]
    pub fn get_poll_waker(&self) -> Waker {
        self.poll_waker.get_waker()
    }

    /// Wakes one of the reactors thats currently waiting
    // TODO: review allow...
    #[allow(dead_code)]
    pub fn poll_wake(&self) {
        self.poll_waker.wake()
    }

    /// Will wait until either the reactor is triggered
    /// or the timeout occurs
    // TODO: review allow...
    #[allow(dead_code)]
    pub fn poll_wait(&self, timeout: Duration) -> bool {
        self.poll_waker.wait(timeout)
    }

    /// Locks the protected area of the BUS and returns a guard that
    /// can be used to access it
    pub fn protected<'a>(&'a self) -> MutexGuard<'a, WasiBusProtectedState> {
        self.protected.lock().unwrap()
    }
}

/// Top level data type containing all* the state with which WASI can
/// interact.
///
/// * The contents of files are not stored and may be modified by
/// other, concurrently running programs.  Data such as the contents
/// of directories are lazily loaded.
///
/// Usage:
///
/// ```no_run
/// # use wasmer_wasi::{WasiState, WasiStateCreationError};
/// # fn main() -> Result<(), WasiStateCreationError> {
/// WasiState::new("program_name")
///    .env(b"HOME", "/home/home".to_string())
///    .arg("--help")
///    .envs({
///        let mut hm = std::collections::HashMap::new();
///        hm.insert("COLOR_OUTPUT", "TRUE");
///        hm.insert("PATH", "/usr/bin");
///        hm
///    })
///    .args(&["--verbose", "list"])
///    .preopen(|p| p.directory("src").read(true).write(true).create(true))?
///    .preopen(|p| p.directory(".").alias("dot").read(true))?
///    .build()?;
/// # Ok(())
/// # }
/// ```
#[derive(Debug)]
#[cfg_attr(feature = "enable-serde", derive(Serialize, Deserialize))]
pub struct WasiState {
    pub fs: WasiFs,
    pub secret: [u8; 32],
    pub inodes: Arc<RwLock<WasiInodes>>,
    // TODO: review allow...
    #[allow(dead_code)]
    pub(crate) threading: RwLock<WasiStateThreading>,
    pub(crate) futexs: Mutex<HashMap<u64, WasiFutex>>,
    pub(crate) clock_offset: Mutex<HashMap<Clockid, i64>>,
    pub(crate) bus: WasiBusState,
    pub args: Vec<String>,
    pub envs: Vec<Vec<u8>>,
    pub preopen: Vec<String>,
    pub(crate) runtime: Arc<dyn WasiRuntimeImplementation + Send + Sync>,
}

impl WasiState {
    /// Create a [`WasiStateBuilder`] to construct a validated instance of
    /// [`WasiState`].
    #[allow(clippy::new_ret_no_self)]
    pub fn new(program_name: impl AsRef<str>) -> WasiStateBuilder {
        create_wasi_state(program_name.as_ref())
    }

    /// Turn the WasiState into bytes
    #[cfg(feature = "enable-serde")]
    pub fn freeze(&self) -> Option<Vec<u8>> {
        bincode::serialize(self).ok()
    }

    /// Get a WasiState from bytes
    #[cfg(feature = "enable-serde")]
    pub fn unfreeze(bytes: &[u8]) -> Option<Self> {
        bincode::deserialize(bytes).ok()
    }

    /// Get the `VirtualFile` object at stdout
    pub fn stdout(&self) -> Result<Option<Box<dyn VirtualFile + Send + Sync + 'static>>, FsError> {
        self.std_dev_get(__WASI_STDOUT_FILENO)
    }

    #[deprecated(
        since = "3.0.0",
        note = "stdout_mut() is no longer needed - just use stdout() instead"
    )]
    pub fn stdout_mut(
        &self,
    ) -> Result<Option<Box<dyn VirtualFile + Send + Sync + 'static>>, FsError> {
        self.stdout()
    }

    /// Get the `VirtualFile` object at stderr
    pub fn stderr(&self) -> Result<Option<Box<dyn VirtualFile + Send + Sync + 'static>>, FsError> {
        self.std_dev_get(__WASI_STDERR_FILENO)
    }

    #[deprecated(
        since = "3.0.0",
        note = "stderr_mut() is no longer needed - just use stderr() instead"
    )]
    pub fn stderr_mut(
        &self,
    ) -> Result<Option<Box<dyn VirtualFile + Send + Sync + 'static>>, FsError> {
        self.stderr()
    }

    /// Get the `VirtualFile` object at stdin
    pub fn stdin(&self) -> Result<Option<Box<dyn VirtualFile + Send + Sync + 'static>>, FsError> {
        self.std_dev_get(__WASI_STDIN_FILENO)
    }

    #[deprecated(
        since = "3.0.0",
        note = "stdin_mut() is no longer needed - just use stdin() instead"
    )]
    pub fn stdin_mut(
        &self,
    ) -> Result<Option<Box<dyn VirtualFile + Send + Sync + 'static>>, FsError> {
        self.stdin()
    }

    /// Internal helper function to get a standard device handle.
    /// Expects one of `__WASI_STDIN_FILENO`, `__WASI_STDOUT_FILENO`, `__WASI_STDERR_FILENO`.
    fn std_dev_get(
        &self,
        fd: WasiFd,
    ) -> Result<Option<Box<dyn VirtualFile + Send + Sync + 'static>>, FsError> {
        let ret = WasiStateFileGuard::new(self, fd)?.map(|a| {
            let ret = Box::new(a);
            let ret: Box<dyn VirtualFile + Send + Sync + 'static> = ret;
            ret
        });
        Ok(ret)
    }

    /// Forking the WasiState is used when either fork or vfork is called
    pub fn fork(&self) -> Self {
        WasiState {
            fs: self.fs.fork(),
            secret: self.secret.clone(),
            inodes: self.inodes.clone(),
            threading: Default::default(),
            futexs: Default::default(),
            clock_offset: Mutex::new(self.clock_offset.lock().unwrap().clone()),
            bus: Default::default(),
            args: self.args.clone(),
            envs: self.envs.clone(),
            preopen: self.preopen.clone(),
            runtime: self.runtime.clone(),
        }
    }
}

pub fn virtual_file_type_to_wasi_file_type(file_type: wasmer_vfs::FileType) -> Filetype {
    // TODO: handle other file types
    if file_type.is_dir() {
        Filetype::Directory
    } else if file_type.is_file() {
        Filetype::RegularFile
    } else if file_type.is_symlink() {
        Filetype::SymbolicLink
    } else {
        Filetype::Unknown
    }
}

#[derive(Debug, Clone)]
pub struct WasiDummyWaker;

impl WakeRef for WasiDummyWaker {
    fn wake_by_ref(&self) {}
}

impl Wake for WasiDummyWaker {
    fn wake(self) {}
}

unsafe impl ViaRawPointer for WasiDummyWaker {
    type Target = ();
    fn into_raw(self) -> *mut () {
        std::mem::forget(self);
        std::ptr::null_mut()
    }
    unsafe fn from_raw(_ptr: *mut ()) -> Self {
        WasiDummyWaker
    }
}
