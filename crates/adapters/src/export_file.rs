//! Native Save As plus verified, atomic installation of plaintext export files.
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
};
use zeroize::Zeroizing;

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

pub fn suggested_filename(name: &str, format: Format) -> String {
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
    let title = if title.is_empty() {
        match format {
            Format::WinAuth2Xml => "驗證器",
            Format::Group => "分組",
            Format::Full => "BNAuth-full",
        }
    } else {
        title
    };
    format!(
        "{}{title}.{}",
        if is_reserved_filename(title) { "_" } else { "" },
        format.extension()
    )
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
    if !path
        .extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case(format.extension()))
    {
        return Err(format!("匯出檔案的副檔名必須是 .{}。", format.extension()));
    }
    if let Some(protected) = protected_path {
        let same_path = path.as_os_str().eq_ignore_ascii_case(protected.as_os_str());
        let same_resolved = match (fs::canonicalize(path), fs::canonicalize(protected)) {
            (Ok(target), Ok(primary)) => {
                target.as_os_str().eq_ignore_ascii_case(primary.as_os_str())
            }
            _ => false,
        };
        if same_path || same_resolved {
            return Err("匯出檔案不能覆蓋目前使用中的 BNAuth.bnauth，請另選檔名或位置。".into());
        }
    }
    if bytes.len() > crate::native::MAX_FILE {
        return Err("匯出檔案超過 32 MiB 上限。".into());
    }
    let parent = path.parent().ok_or("無法取得匯出目錄。")?;
    let temporary = parent.join(format!(".BNAuth-export-{}.tmp", uuid::Uuid::new_v4()));
    let mut temporary = ExportTemporary::create(&temporary)
        .map_err(|error| format!("無法建立匯出暫存檔（{error}）。"))?;
    // A junction can change between the initial path check and opening the
    // destination directory. Check the directory actually bound by the handle
    // before writing any sensitive bytes, rather than resolving the alias again.
    #[cfg(windows)]
    if let Some(protected) = protected_path
        && temporary
            .parent
            .protects(path, protected)
            .map_err(|error| format!("無法確認匯出目的地（{error}）。"))?
    {
        return Err("匯出檔案不能覆蓋目前使用中的 BNAuth.bnauth，請另選檔名或位置。".into());
    }
    let result = (|| {
        let file = temporary.file_mut();
        file.write_all(bytes)?;
        file.sync_all()?;
        temporary.verify(bytes)
    })();
    result.map_err(|error: std::io::Error| format!("檔案匯出失敗（{error}）。"))?;
    // On Windows the verified file stays protected against writes and rename
    // through installation. Rename the exact open handle, never a reopened path.
    temporary
        .install(path)
        .map_err(|error| format!("無法完成檔案匯出，原有目標檔案未被取代（{error}）。"))?;
    #[cfg(windows)]
    temporary.verify(bytes).map_err(|error| {
        format!("檔案已匯出，但保存後讀回核對失敗，請保留並檢查目標檔案（{error}）。")
    })?;
    // Resolve the installed file while this exact handle is still protected.
    // Reopening the selected path could resolve a junction to a different file.
    #[cfg(windows)]
    let installed = temporary.installed_path();
    #[cfg(not(windows))]
    let installed = fs::canonicalize(path);
    installed
        .map_err(|error| format!("檔案已匯出，但無法確認實際保存位置，請檢查目標檔案（{error}）。"))
}

struct ExportTemporary {
    file: Option<File>,
    #[cfg(windows)]
    parent: ExportDirectory,
    #[cfg(not(windows))]
    path: PathBuf,
    installed: bool,
}

impl ExportTemporary {
    fn create(path: &Path) -> std::io::Result<Self> {
        #[cfg(windows)]
        {
            let parent = ExportDirectory::open(path.parent().ok_or_else(invalid_filename)?)?;
            let file = parent.create(path.file_name().ok_or_else(invalid_filename)?)?;
            Ok(Self {
                file: Some(file),
                parent,
                installed: false,
            })
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

    fn file_mut(&mut self) -> &mut File {
        self.file.as_mut().expect("temporary owns its file")
    }

    fn verify(&mut self, bytes: &[u8]) -> std::io::Result<()> {
        let file = self.file_mut();
        file.seek(SeekFrom::Start(0))?;
        let mut actual = Zeroizing::new(Vec::with_capacity(bytes.len()));
        file.take(crate::native::MAX_FILE as u64 + 1)
            .read_to_end(&mut actual)?;
        if actual.as_slice() != bytes {
            return Err(std::io::Error::other("匯出檔案讀回核對失敗"));
        }
        Ok(())
    }

    fn install(&mut self, path: &Path) -> std::io::Result<()> {
        #[cfg(windows)]
        self.parent
            .rename(self.file.as_ref().expect("temporary owns its file"), path)?;
        #[cfg(not(windows))]
        {
            drop(self.file.take());
            fs::rename(&self.path, path)?;
        }
        // Disarm cleanup immediately after the successful rename. A later
        // readback failure must never delete the installed destination.
        self.installed = true;
        Ok(())
    }

    #[cfg(windows)]
    fn installed_path(&self) -> std::io::Result<PathBuf> {
        use std::os::windows::{ffi::OsStringExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::{
            FILE_NAME_NORMALIZED, GetFinalPathNameByHandleW, VOLUME_NAME_DOS, VOLUME_NAME_GUID,
        };
        let query = |volume| {
            let mut buffer = vec![0_u16; 1024];
            loop {
                let length = unsafe {
                    GetFinalPathNameByHandleW(
                        self.file
                            .as_ref()
                            .expect("temporary owns its file")
                            .as_raw_handle(),
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
}

impl Drop for ExportTemporary {
    fn drop(&mut self) {
        if self.installed {
            return;
        }
        #[cfg(windows)]
        if let Some(file) = &self.file {
            use std::os::windows::io::AsRawHandle;
            use windows_sys::Win32::Storage::FileSystem::{
                FILE_DISPOSITION_INFO, FileDispositionInfo, SetFileInformationByHandle,
            };
            let disposition = FILE_DISPOSITION_INFO { DeleteFile: true };
            // Mark exactly the owned handle for deletion before closing it.
            // Never remove a path that could have been replaced after a close.
            unsafe {
                SetFileInformationByHandle(
                    file.as_raw_handle(),
                    FileDispositionInfo,
                    (&disposition as *const FILE_DISPOSITION_INFO).cast(),
                    std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32,
                );
            }
        }
        #[cfg(not(windows))]
        {
            drop(self.file.take());
            let _ = fs::remove_file(&self.path);
        }
    }
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
        use std::os::windows::{fs::OpenOptionsExt, io::AsRawHandle};
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, FILE_ATTRIBUTE_DIRECTORY, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_READ_ATTRIBUTES, FILE_SHARE_READ, FILE_SHARE_WRITE, FILE_TRAVERSE,
            GetFileInformationByHandle,
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
        let mut info = BY_HANDLE_FILE_INFORMATION::default();
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
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
        let protected_parent = Self::open(protected.parent().ok_or_else(invalid_filename)?)?;
        Ok(self.identity == protected_parent.identity
            && destination
                .file_name()
                .ok_or_else(invalid_filename)?
                .eq_ignore_ascii_case(protected.file_name().ok_or_else(invalid_filename)?))
    }

    fn create(&self, name: &std::ffi::OsStr) -> std::io::Result<File> {
        use std::os::windows::io::{AsRawHandle, FromRawHandle};
        use windows_sys::{
            Wdk::{
                Foundation::OBJECT_ATTRIBUTES,
                Storage::FileSystem::{
                    FILE_CREATE, FILE_NON_DIRECTORY_FILE, FILE_OPEN_REPARSE_POINT,
                    FILE_SYNCHRONOUS_IO_NONALERT, NtCreateFile,
                },
            },
            Win32::{
                Foundation::{
                    CloseHandle, GENERIC_READ, GENERIC_WRITE, OBJ_CASE_INSENSITIVE, UNICODE_STRING,
                },
                Storage::FileSystem::{
                    DELETE, FILE_ATTRIBUTE_NORMAL, FILE_SHARE_READ, SYNCHRONIZE,
                },
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
                GENERIC_READ | GENERIC_WRITE | DELETE | SYNCHRONIZE,
                &attributes,
                &mut status_block,
                std::ptr::null(),
                FILE_ATTRIBUTE_NORMAL,
                FILE_SHARE_READ,
                FILE_CREATE,
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
        // NtCreateFile returned a newly created synchronous owned handle.
        Ok(unsafe { File::from_raw_handle(handle) })
    }

    fn rename(&self, file: &File, path: &Path) -> std::io::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::{
            Wdk::Storage::FileSystem::{
                FILE_RENAME_INFORMATION, FileRenameInformation, NtSetInformationFile,
            },
            Win32::System::IO::IO_STATUS_BLOCK,
        };
        let requested = absolute_directory(path.parent().ok_or_else(invalid_filename)?)?;
        if !requested
            .as_os_str()
            .eq_ignore_ascii_case(self.requested.as_os_str())
        {
            return Err(invalid_filename());
        }
        let name = relative_name(path.file_name().ok_or_else(invalid_filename)?)?;
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
            (*info).Anonymous.ReplaceIfExists = true;
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
        native_result(status)
    }
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
    filename: &str,
    owner: isize,
    format: Format,
) -> Result<Option<PathBuf>, String> {
    choose_file(directory, filename, owner, format, true)
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
    let title: Vec<_> = if save {
        format.title()
    } else {
        match format {
            Format::Full => "還原完整資料庫",
            Format::Group => "匯入分組檔",
            Format::WinAuth2Xml => "開啟 WinAuth2 XML",
        }
    }
    .encode_utf16()
    .chain(Some(0))
    .collect();
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
            Err(format!("無法開啟檔案選擇視窗（錯誤碼 0x{error:04X}）。"))
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
    _filename: &str,
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
