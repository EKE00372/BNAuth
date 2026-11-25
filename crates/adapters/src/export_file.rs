//! Native Save As plus verified, atomic installation of plaintext export files.
use crate::operation_io_error;
#[cfg(windows)]
use crate::temp_cleanup_error;
use bnauth_core::ErrorCode;
#[cfg(windows)]
use bnauth_core::OperationError;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
};

pub const GROUP_IMPORT_TITLE: &str = "匯入單組備份";
pub const FULL_RESTORE_TITLE: &str = "還原完整備份";
const PRIMARY_OVERWRITE_NOTICE: &str = "請勿覆蓋設定檔。";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    WinAuth2Xml,
    Group,
    Full,
}

impl Format {
    pub fn extension(self) -> &'static str {
        match self {
            Self::WinAuth2Xml => "xml",
            Self::Group | Self::Full => "bnauth",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::WinAuth2Xml => "匯出 WinAuth2 XML",
            Self::Group => "匯出此分組",
            Self::Full => "備份完整資料庫",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::WinAuth2Xml => "XML",
            Self::Group => "分組檔",
            Self::Full => "完整備份",
        }
    }
}

fn write_error(error: std::io::Error) -> String {
    operation_io_error(&error, ErrorCode::WriteFailed)
        .notice()
        .to_string()
}

fn filename_stem(name: &str, fallback: &str) -> String {
    let mut title = String::new();
    let mut units = 0;
    for ch in name.chars() {
        if units + ch.len_utf16() > 100 {
            break;
        }
        units += ch.len_utf16();
        title.push(if ch.is_control() || "<>:\"/\\|?*".contains(ch) {
            '_'
        } else {
            ch
        });
    }
    let title = title.trim().trim_end_matches(['.', ' ']);
    let title = if title.is_empty() { fallback } else { title };
    if is_reserved_filename(title) {
        format!("_{title}")
    } else {
        title.to_owned()
    }
}

pub(crate) fn suggested_xml_filename(name: &str) -> String {
    format!("{}.xml", filename_stem(name, "驗證器"))
}

/// Append the local date to the suggested backup name.
pub fn suggested_backup_filename(name: &str, (year, month, day): (u16, u16, u16)) -> String {
    let title = filename_stem(name, "分組");
    format!("{title}-{:02}{month:02}{day:02}.bnauth", year % 100)
}

/// Device-name stems that require protection in ordinary Windows paths.
pub fn is_reserved_filename(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ["COM", "LPT"].iter().any(|prefix| {
            stem.strip_prefix(prefix).is_some_and(|suffix| {
                matches!(
                    suffix,
                    "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            })
        })
}

pub fn write(
    path: &Path,
    bytes: &[u8],
    format: Format,
    protected_path: Option<&Path>,
) -> Result<PathBuf, String> {
    write_with(path, format, protected_path, || Ok(bytes))
}

/// Keep the selected directory bound while preparing and installing the contents.
pub fn write_with<B: AsRef<[u8]>>(
    path: &Path,
    format: Format,
    protected_path: Option<&Path>,
    encode: impl FnOnce() -> Result<B, String>,
) -> Result<PathBuf, String> {
    write_with_cleanup(
        &mut ExportCleanup::default(),
        path,
        format,
        protected_path,
        encode,
    )
}

/// Export temporaries that this session could not remove.
#[derive(Default)]
pub struct ExportCleanup {
    #[cfg(windows)]
    pending: Vec<PendingExport>,
}

impl ExportCleanup {
    #[cfg(windows)]
    fn retry(&mut self, parent: &ExportDirectory) -> Result<(), String> {
        let Some(index) = self
            .pending
            .iter()
            .position(|pending| pending.directory == parent.identity)
        else {
            return Ok(());
        };
        self.pending[index]
            .remove(parent)
            .map_err(|error| error.to_string())?;
        self.pending.swap_remove(index);
        Ok(())
    }
}

/// Retry cleanup for the selected directory before creating a new export there.
pub fn write_with_cleanup<B: AsRef<[u8]>>(
    cleanup: &mut ExportCleanup,
    path: &Path,
    format: Format,
    protected_path: Option<&Path>,
    encode: impl FnOnce() -> Result<B, String>,
) -> Result<PathBuf, String> {
    if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(format.extension()))
    {
        return Err(format!("匯出檔案的副檔名必須是 .{}。", format.extension()));
    }
    if let Some(protected) = protected_path {
        let same_target = path.as_os_str().eq_ignore_ascii_case(protected.as_os_str())
            || fs::canonicalize(path).is_ok_and(|target| {
                fs::canonicalize(protected).is_ok_and(|primary| {
                    target.as_os_str().eq_ignore_ascii_case(primary.as_os_str())
                })
            });
        if same_target {
            return Err(PRIMARY_OVERWRITE_NOTICE.into());
        }
    }
    let parent = path
        .parent()
        .ok_or_else(|| ErrorCode::WriteFailed.notice().to_string())?;
    #[cfg(windows)]
    let parent = ExportDirectory::open(parent).map_err(write_error)?;
    // A junction can change between the initial path check and opening the
    // destination directory. Check the directory actually bound by the handle
    // before writing any sensitive bytes, rather than resolving the alias again.
    #[cfg(windows)]
    if let Some(protected) = protected_path
        && parent.protects(path, protected).map_err(write_error)?
    {
        return Err(PRIMARY_OVERWRITE_NOTICE.into());
    }
    #[cfg(windows)]
    cleanup.retry(&parent)?;
    #[cfg(not(windows))]
    let _ = cleanup;
    let encoded = encode()?;
    let bytes = encoded.as_ref();
    if bytes.len() > crate::native::MAX_FILE {
        return Err(ErrorCode::FileTooLarge.notice().to_string());
    }
    let name = format!(".BNAuth-export-{}.tmp", uuid::Uuid::new_v4());
    #[cfg(windows)]
    let temporary = ExportTemporary::create_in(parent, std::ffi::OsStr::new(&name));
    #[cfg(not(windows))]
    let temporary = ExportTemporary::create(&parent.join(name));
    let mut temporary = temporary.map_err(write_error)?;
    let result = (|| {
        let result = (|| {
            let file = temporary.file_mut();
            file.write_all(bytes)?;
            file.sync_all()?;
            temporary.verify(bytes)
        })();
        result.map_err(write_error)?;
        // On Windows, rename the already verified file through the same guarded handle.
        temporary.install(path).map_err(write_error)?;
        // Resolving the selected path again could follow a retargeted junction.
        #[cfg(windows)]
        let installed = temporary.installed_path();
        #[cfg(not(windows))]
        let installed = fs::canonicalize(path);
        installed.map_err(write_error)
    })();
    #[cfg(windows)]
    if result.is_err() {
        temporary.cleanup_after_failure(cleanup);
    }
    result
}

struct ExportTemporary {
    file: Option<File>,
    #[cfg(windows)]
    parent: ExportDirectory,
    #[cfg(windows)]
    residue: Option<PendingExport>,
    #[cfg(not(windows))]
    path: PathBuf,
    installed: bool,
}

impl ExportTemporary {
    #[cfg(any(not(windows), test))]
    fn create(path: &Path) -> std::io::Result<Self> {
        #[cfg(windows)]
        {
            let parent = ExportDirectory::open(path.parent().ok_or_else(invalid_filename)?)?;
            Self::create_in(parent, path.file_name().ok_or_else(invalid_filename)?)
        }
        #[cfg(not(windows))]
        {
            let mut options = OpenOptions::new();
            options.read(true).write(true).create_new(true);
            Ok(Self {
                file: Some(options.open(path)?),
                path: path.to_path_buf(),
                installed: false,
            })
        }
    }

    #[cfg(windows)]
    fn create_in(parent: ExportDirectory, name: &std::ffi::OsStr) -> std::io::Result<Self> {
        let file = parent.create(name)?;
        // Capture the file identity before writing to a removable or network drive.
        let residue = PendingExport {
            directory: parent.identity,
            name: name.to_owned(),
            identity: export_file_identity(&file).ok(),
        };
        Ok(Self {
            file: Some(file),
            parent,
            residue: Some(residue),
            installed: false,
        })
    }

    #[cfg(windows)]
    fn cleanup_after_failure(&mut self, cleanup: &mut ExportCleanup) {
        if self.installed {
            return;
        }
        // Keep the write error in view. Retry will report a cleanup error if it
        // still blocks exporting to this directory.
        let file = self
            .file
            .take()
            .expect("uninstalled temporary owns its file");
        if delete_export_file(&file).is_err() {
            let residue = self
                .residue
                .take()
                .expect("uninstalled temporary has its identity");
            cleanup.pending.push(residue);
        }
    }

    fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("temporary owns its file")
    }

    fn verify(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        if crate::file_matches_bytes(self.file_mut(), bytes)? {
            Ok(())
        } else {
            Err(std::io::Error::other("匯出檔案讀回核對失敗"))
        }
    }

    fn install(&mut self, path: &Path) -> std::io::Result<()> {
        #[cfg(windows)]
        self.parent.rename(
            self.file.as_ref().expect("temporary owns its file"),
            path,
            true,
        )?;
        #[cfg(not(windows))]
        {
            drop(self.file.take());
            fs::rename(&self.path, path)?;
        }
        // Disarm temporary cleanup as soon as the destination is installed.
        self.installed = true;
        Ok(())
    }

    #[cfg(windows)]
    fn installed_path(&self) -> std::io::Result<PathBuf> {
        path_from_handle(self.file.as_ref().expect("temporary owns its file"))
    }
}

#[cfg(windows)]
fn path_from_handle(file: &File) -> std::io::Result<PathBuf> {
    use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS, VOLUME_NAME_GUID,
    };
    let query = |volume| {
        let mut buffer = vec![0_u16; 1024];
        loop {
            let length = unsafe {
                GetFinalPathNameByHandleW(
                    file.as_raw_handle(),
                    buffer.as_mut_ptr(),
                    buffer.len() as u32,
                    volume | FILE_NAME_NORMALIZED,
                )
            };
            if length == 0 {
                return Err(std::io::Error::last_os_error());
            }
            if length as usize >= buffer.len() {
                if length > 32768 {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        "匯出檔案路徑過長",
                    ));
                }
                buffer.resize(length as usize + 1, 0);
                continue;
            }
            return Ok(PathBuf::from(std::ffi::OsString::from_wide(
                &buffer[..length as usize],
            )));
        }
    };
    // DOS form also covers UNC shares. A volume without a drive letter can
    // still have a usable volume-GUID path; keep that form intact for I/O.
    query(VOLUME_NAME_DOS).or_else(|_| query(VOLUME_NAME_GUID))
}

impl Drop for ExportTemporary {
    fn drop(&mut self) {
        if self.installed {
            return;
        }
        #[cfg(windows)]
        if let Some(file) = &self.file {
            let _ = delete_export_file(file);
        }
        #[cfg(not(windows))]
        {
            drop(self.file.take());
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[cfg(windows)]
fn delete_export_file(file: &File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
    };
    let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
    // Delete the verified handle, since a path could now name a different file.
    if unsafe {
        SetFileInformationByHandle(
            file.as_raw_handle(),
            FileDispositionInfo,
            (&disposition as *const FILE_DISPOSITION_INFO).cast(),
            std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
        )
    } == 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
struct PendingExport {
    directory: (u32, u64),
    name: std::ffi::OsString,
    identity: Option<ExportFileIdentity>,
}

#[cfg(windows)]
impl PendingExport {
    fn remove(&self, parent: &ExportDirectory) -> Result<(), OperationError> {
        let current_path = || {
            path_from_handle(&parent.file)
                .unwrap_or_else(|_| parent.requested.clone())
                .join(&self.name)
        };
        let check_detail = |code: ErrorCode| {
            if code == ErrorCode::DiskFull {
                code.notice()
            } else {
                code.with_detail(format!("請檢查下列暫存檔：\n{}", current_path().display()))
            }
        };
        let Some(file) = parent
            .open_existing(&self.name)
            .map_err(|error| check_detail(operation_io_error(&error, ErrorCode::DeleteFailed)))?
        else {
            return Ok(());
        };
        let identity = export_file_identity(&file)
            .map_err(|error| check_detail(operation_io_error(&error, ErrorCode::DeleteFailed)))?;
        if self.identity.as_ref() != Some(&identity) {
            return Err(check_detail(ErrorCode::DeleteFailed));
        }
        delete_export_file(&file).map_err(|error| {
            let code = temp_cleanup_error(&error);
            match code {
                ErrorCode::FileInUse => code.manual_delete(current_path().display()),
                _ => check_detail(code),
            }
        })
    }
}

#[cfg(windows)]
#[derive(PartialEq, Eq)]
struct ExportFileIdentity {
    volume: u64,
    id: [u8; 16],
    created_at: u64,
}

#[cfg(windows)]
fn handle_info(
    file: &File,
) -> std::io::Result<windows_sys::Win32::Storage::FileSystem::BY_HANDLE_FILE_INFORMATION> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut info = BY_HANDLE_FILE_INFORMATION::default();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(info)
}

#[cfg(windows)]
fn export_file_identity(file: &File) -> std::io::Result<ExportFileIdentity> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
    };
    let basic = handle_info(file)?;
    if basic.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(std::io::Error::other("暫存檔位置已被連結取代"));
    }
    let mut info = FILE_ID_INFO::default();
    if unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            std::mem::size_of::<FILE_ID_INFO>() as u32,
        )
    } == 0
    {
        // Some removable filesystems only report the older 64-bit index.
        let index = (u64::from(basic.nFileIndexHigh) << 32) | u64::from(basic.nFileIndexLow);
        info.VolumeSerialNumber = u64::from(basic.dwVolumeSerialNumber);
        info.FileId.Identifier = [0; 16];
        info.FileId.Identifier[..8].copy_from_slice(&index.to_le_bytes());
    }
    if info.FileId.Identifier == [0; 16] {
        return Err(std::io::Error::other("Windows 未提供暫存檔識別碼"));
    }
    Ok(ExportFileIdentity {
        volume: info.VolumeSerialNumber,
        id: info.FileId.Identifier,
        created_at: (u64::from(basic.ftCreationTime.dwHighDateTime) << 32)
            | u64::from(basic.ftCreationTime.dwLowDateTime),
    })
}

#[cfg(windows)]
struct ExportDirectory {
    file: File,
    requested: PathBuf,
    identity: (u32, u64),
}

#[cfg(windows)]
fn invalid_filename() -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidInput, "匯出檔名或目錄無效")
}

#[cfg(windows)]
fn absolute_directory(path: &Path) -> std::io::Result<PathBuf> {
    // Path::parent("export.xml") is Some(""). That denotes the current
    // directory, although std::path::absolute rejects an empty input.
    std::path::absolute(if path.as_os_str().is_empty() {
        Path::new(".")
    } else {
        path
    })
}

#[cfg(windows)]
fn relative_name(name: &std::ffi::OsStr) -> std::io::Result<Vec<u16>> {
    use std::os::windows::ffi::OsStrExt;
    let mut units: Vec<_> = name.encode_wide().collect();
    // Native RootDirectory names must be one ordinary entry. In particular,
    // reject absolute/relative paths and ADS syntax that could escape the bound
    // directory or change which stream gets installed.
    if units.is_empty()
        || name == "."
        || name == ".."
        || units.iter().any(|unit| matches!(*unit, 0 | 47 | 58 | 92))
        || units
            .len()
            .checked_add(1)
            .and_then(|n| n.checked_mul(2))
            .and_then(|n| u16::try_from(n).ok())
            .is_none()
    {
        return Err(invalid_filename());
    }
    units.push(0);
    Ok(units)
}

#[cfg(windows)]
fn native_result(status: windows_sys::Win32::Foundation::NTSTATUS) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::{RtlNtStatusToDosError, STATUS_PENDING};
    // Handles are synchronous; an unexpected pending status is not a completed
    // create/rename and must never be treated as an installed export.
    if status >= 0 && status != STATUS_PENDING {
        Ok(())
    } else {
        Err(std::io::Error::from_raw_os_error(
            unsafe { RtlNtStatusToDosError(status) } as i32,
        ))
    }
}

#[cfg(windows)]
impl ExportDirectory {
    fn open(path: &Path) -> std::io::Result<Self> {
        use std::os::windows::fs::OpenOptionsExt;
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_ATTRIBUTE_DIRECTORY, FILE_FLAG_BACKUP_SEMANTICS, FILE_READ_ATTRIBUTES,
            FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
        };
        let requested = absolute_directory(path)?;
        // Follow the user's initial junction to its actual directory, then deny
        // deletion/rename of that directory while the export owns this handle.
        let file = OpenOptions::new()
            .read(true)
            .access_mode(FILE_TRAVERSE | FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(&requested)?;
        let info = handle_info(&file)?;
        if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0 {
            return Err(invalid_filename());
        }
        Ok(Self {
            file,
            requested,
            identity: (
                info.dwVolumeSerialNumber,
                (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
            ),
        })
    }

    fn protects(&self, destination: &Path, protected: &Path) -> std::io::Result<bool> {
        if !destination
            .file_name()
            .ok_or_else(invalid_filename)?
            .eq_ignore_ascii_case(protected.file_name().ok_or_else(invalid_filename)?)
        {
            return Ok(false);
        }
        let protected_parent = Self::open(protected.parent().ok_or_else(invalid_filename)?)?;
        Ok(self.identity == protected_parent.identity)
    }

    fn create(&self, name: &std::ffi::OsStr) -> std::io::Result<File> {
        use windows_sys::{
            Wdk::Storage::FileSystem::FILE_CREATE,
            Win32::{
                Foundation::{GENERIC_READ, GENERIC_WRITE},
                Storage::FileSystem::{DELETE, FILE_SHARE_READ, SYNCHRONIZE},
            },
        };
        self.open_relative(
            name,
            GENERIC_READ | GENERIC_WRITE | DELETE | SYNCHRONIZE,
            FILE_SHARE_READ,
            FILE_CREATE,
        )
    }

    fn open_existing(&self, name: &std::ffi::OsStr) -> std::io::Result<Option<File>> {
        use windows_sys::{
            Wdk::Storage::FileSystem::FILE_OPEN,
            Win32::{
                Foundation::ERROR_FILE_NOT_FOUND,
                Storage::FileSystem::{DELETE, FILE_READ_ATTRIBUTES, FILE_SHARE_READ, SYNCHRONIZE},
            },
        };
        match self.open_relative(
            name,
            FILE_READ_ATTRIBUTES | DELETE | SYNCHRONIZE,
            FILE_SHARE_READ,
            FILE_OPEN,
        ) {
            Ok(file) => Ok(Some(file)),
            Err(error) if error.raw_os_error() == Some(ERROR_FILE_NOT_FOUND as i32) => Ok(None),
            Err(error) => Err(error),
        }
    }

    fn open_relative(
        &self,
        name: &std::ffi::OsStr,
        access: u32,
        share: u32,
        disposition: u32,
    ) -> std::io::Result<File> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        use windows_sys::{
            Wdk::{
                Foundation::OBJECT_ATTRIBUTES,
                Storage::FileSystem::{
                    FILE_NON_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT, FILE_SYNCHRONOUS_IO_NONALERT,
                    NtCreateFile,
                },
            },
            Win32::{
                Foundation::{CloseHandle, OBJ_CASE_INSENSITIVE, UNICODE_STRING},
                Storage::FileSystem::FILE_ATTRIBUTE_NORMAL,
                System::IO::IO_STATUS_BLOCK,
            },
        };
        let name = relative_name(name)?;
        let object_name = UNICODE_STRING {
            Length: ((name.len() - 1) * 2) as u16,
            MaximumLength: (name.len() * 2) as u16,
            Buffer: name.as_ptr().cast_mut(),
        };
        let attributes = OBJECT_ATTRIBUTES {
            Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: self.file.as_raw_handle(),
            ObjectName: &object_name,
            Attributes: OBJ_CASE_INSENSITIVE,
            ..Default::default()
        };
        let mut status_block = IO_STATUS_BLOCK::default();
        let mut handle = std::ptr::null_mut();
        // User-mode ntdll bindings keep both creation and installation relative
        // to the same directory object. No junction/ancestor path is re-opened.
        let status = unsafe {
            NtCreateFile(
                &mut handle,
                access,
                &attributes,
                &mut status_block,
                std::ptr::null(),
                FILE_ATTRIBUTE_NORMAL,
                share,
                disposition,
                FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
                std::ptr::null(),
                0,
            )
        };
        if let Err(error) = native_result(status) {
            if !handle.is_null() {
                unsafe {
                    CloseHandle(handle);
                }
            }
            return Err(error);
        }
        // NtCreateFile returned a synchronous owned handle.
        Ok(unsafe { File::from_raw_handle(handle) })
    }

    fn rename(&self, file: &File, path: &Path, replace_existing: bool) -> std::io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::{
            Wdk::Storage::FileSystem::{
                FILE_OPEN, FILE_RENAME_INFORMATION, FileRenameInformation, NtSetInformationFile,
            },
            Win32::{
                Storage::FileSystem::{
                    DELETE, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE, SYNCHRONIZE,
                },
                System::IO::IO_STATUS_BLOCK,
            },
        };
        let requested = absolute_directory(path.parent().ok_or_else(invalid_filename)?)?;
        if !requested
            .as_os_str()
            .eq_ignore_ascii_case(self.requested.as_os_str())
        {
            return Err(invalid_filename());
        }
        let filename = path.file_name().ok_or_else(invalid_filename)?;
        let name = relative_name(filename)?;
        let name_bytes = ((name.len() - 1) * 2) as u32;
        let size = (std::mem::offset_of!(FILE_RENAME_INFORMATION, FileName) + name.len() * 2)
            .max(std::mem::size_of::<FILE_RENAME_INFORMATION>());
        let mut buffer = vec![0_usize; size.div_ceil(std::mem::size_of::<usize>())];
        let mut status_block = IO_STATUS_BLOCK::default();
        // Win32 FileRenameInfo rejected a valid non-null RootDirectory with
        // ERROR_INVALID_PARAMETER on the tested Windows 11. Use the generated
        // user-mode ntdll API with its documented relative-directory layout.
        let status = unsafe {
            let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
            (*info).Anonymous.ReplaceIfExists = replace_existing;
            (*info).RootDirectory = self.file.as_raw_handle();
            (*info).FileNameLength = name_bytes;
            std::ptr::copy_nonoverlapping(
                name.as_ptr(),
                std::ptr::addr_of_mut!((*info).FileName).cast::<u16>(),
                name.len(),
            );
            NtSetInformationFile(
                file.as_raw_handle(),
                &mut status_block,
                info.cast(),
                size as u32,
                FileRenameInformation,
            )
        };
        let result = native_result(status);
        // NTFS may return access denied for an occupied replacement target.
        // Probe only after that error, sharing all access so only the target's
        // refusal to share DELETE can classify the result as a sharing error.
        if replace_existing
            && matches!(&result, Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied)
            && let Err(error) = self.open_relative(
                filename,
                DELETE | SYNCHRONIZE,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                FILE_OPEN,
            )
            && operation_io_error(&error, ErrorCode::WriteFailed) == ErrorCode::FileInUse
        {
            return Err(error);
        }
        result
    }
}

/// Install a verified recovery file through its existing DELETE-capable handle.
#[cfg(windows)]
pub(crate) fn rename_open_file_without_replace(file: &File, path: &Path) -> std::io::Result<()> {
    let parent = path.parent().ok_or_else(invalid_filename)?;
    ExportDirectory::open(parent)?.rename(file, path, false)
}

#[cfg(not(windows))]
pub(crate) fn rename_open_file_without_replace(_: &File, _: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "不支援的平台。",
    ))
}

#[cfg(windows)]
pub fn active_window() -> isize {
    // Capture on the UI thread, before the file-dialog worker starts.
    unsafe { windows_sys::Win32::UI::Input::KeyboardAndMouse::GetActiveWindow() as isize }
}

#[cfg(not(windows))]
pub fn active_window() -> isize {
    0
}

#[cfg(windows)]
pub fn choose_path(
    directory: &Path,
    name: &str,
    owner: isize,
    format: Format,
) -> Result<Option<PathBuf>, String> {
    let filename = match format {
        Format::WinAuth2Xml => suggested_xml_filename(name),
        Format::Group | Format::Full => {
            let mut date = windows_sys::Win32::Foundation::SYSTEMTIME::default();
            // SAFETY: GetLocalTime fills the caller-owned structure synchronously.
            unsafe { windows_sys::Win32::System::SystemInformation::GetLocalTime(&mut date) };
            suggested_backup_filename(name, (date.wYear, date.wMonth, date.wDay))
        }
    };
    choose_file(directory, &filename, owner, format, true)
}

#[cfg(windows)]
pub fn choose_group_import_path(directory: &Path, owner: isize) -> Result<Option<PathBuf>, String> {
    choose_file(directory, "", owner, Format::Group, false)
}

#[cfg(windows)]
pub fn choose_full_restore_path(directory: &Path, owner: isize) -> Result<Option<PathBuf>, String> {
    choose_file(directory, "", owner, Format::Full, false)
}

#[cfg(windows)]
fn choose_file(
    directory: &Path,
    filename: &str,
    owner: isize,
    format: Format,
    save: bool,
) -> Result<Option<PathBuf>, String> {
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows_sys::Win32::UI::Controls::Dialogs::{
        CommDlgExtendedError, GetOpenFileNameW, GetSaveFileNameW, OFN_DONTADDTORECENT,
        OFN_EXPLORER, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_OVERWRITEPROMPT, OFN_PATHMUSTEXIST,
        OPENFILENAMEW,
    };
    let mut buffer = vec![0_u16; 32_768];
    let name: Vec<_> = filename.encode_utf16().collect();
    if name.len() >= buffer.len() || name.contains(&0) {
        return Err("匯出檔名無效或過長。".into());
    }
    buffer[..name.len()].copy_from_slice(&name);
    let directory: Vec<_> = directory.as_os_str().encode_wide().chain(Some(0)).collect();
    let filter: Vec<_> = match format {
        Format::WinAuth2Xml => "WinAuth2 XML (*.xml)\0*.xml\0\0",
        Format::Group => "BNAuth 分組檔 (*.bnauth)\0*.bnauth\0\0",
        Format::Full => "BNAuth 完整備份 (*.bnauth)\0*.bnauth\0\0",
    }
    .encode_utf16()
    .collect();
    let action = if save {
        format.title()
    } else {
        match format {
            Format::Full => FULL_RESTORE_TITLE,
            Format::Group => GROUP_IMPORT_TITLE,
            Format::WinAuth2Xml => "開啟 WinAuth2 XML",
        }
    };
    let title: Vec<_> = action.encode_utf16().chain(Some(0)).collect();
    let extension: Vec<_> = format.extension().encode_utf16().chain(Some(0)).collect();
    let mut dialog = OPENFILENAMEW {
        lStructSize: std::mem::size_of::<OPENFILENAMEW>() as u32,
        hwndOwner: owner as _,
        lpstrFile: buffer.as_mut_ptr(),
        nMaxFile: buffer.len() as u32,
        lpstrFilter: filter.as_ptr(),
        nFilterIndex: 1,
        lpstrInitialDir: directory.as_ptr(),
        lpstrTitle: title.as_ptr(),
        lpstrDefExt: extension.as_ptr(),
        Flags: OFN_EXPLORER
            | OFN_NOCHANGEDIR
            | OFN_PATHMUSTEXIST
            | if save {
                OFN_OVERWRITEPROMPT
            } else {
                OFN_FILEMUSTEXIST
            }
            | OFN_DONTADDTORECENT,
        ..Default::default()
    };
    // All buffers remain alive and writable throughout this modal call. Its
    // Win32 owner remains alive because the runtime blocks closing during file IO.
    let accepted = unsafe {
        if save {
            GetSaveFileNameW(&mut dialog)
        } else {
            GetOpenFileNameW(&mut dialog)
        }
    };
    if accepted == 0 {
        let error = unsafe { CommDlgExtendedError() };
        return if error == 0 {
            Ok(None)
        } else {
            Err(ErrorCode::ReadFailed.notice().to_string())
        };
    }
    let end = buffer.iter().position(|&unit| unit == 0).unwrap();
    Ok(Some(PathBuf::from(std::ffi::OsString::from_wide(
        &buffer[..end],
    ))))
}

#[cfg(not(windows))]
pub fn choose_path(
    _directory: &Path,
    _name: &str,
    _owner: isize,
    _format: Format,
) -> Result<Option<PathBuf>, String> {
    Err("檔案另存視窗僅支援 Windows。".into())
}

#[cfg(not(windows))]
pub fn choose_group_import_path(
    _directory: &Path,
    _owner: isize,
) -> Result<Option<PathBuf>, String> {
    Err("分組檔選擇視窗僅支援 Windows。".into())
}

#[cfg(not(windows))]
pub fn choose_full_restore_path(
    _directory: &Path,
    _owner: isize,
) -> Result<Option<PathBuf>, String> {
    Err("完整備份選擇視窗僅支援 Windows。".into())
}

#[cfg(all(test, windows))]
#[path = "export_file_tests.rs"]
mod tests;
