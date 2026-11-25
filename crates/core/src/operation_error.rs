use std::fmt::{self, Display};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum ErrorCode {
    PermissionDenied = 1,
    FileInUse,
    DiskFull,
    CorruptFile,
    FullBackupSelected,
    GroupBackupSelected,
    UnsupportedFilesystem,
    ReadFailed,
    WriteFailed,
    DeleteFailed,
    ConnectionFailed,
    HttpError,
    TimeError,
    RateLimited,
    FileTooLarge = 15,
}

impl ErrorCode {
    pub const fn description(self) -> &'static str {
        match self {
            Self::PermissionDenied => "權限不足",
            Self::FileInUse => "檔案被占用",
            Self::DiskFull => "磁碟空間不足",
            Self::CorruptFile => "檔案毀損",
            Self::FullBackupSelected | Self::GroupBackupSelected => "類型錯誤",
            Self::UnsupportedFilesystem => "磁碟格式不支援",
            Self::ReadFailed => "讀取失敗",
            Self::WriteFailed => "寫入失敗",
            Self::DeleteFailed => "刪除失敗",
            Self::ConnectionFailed => "連線異常",
            Self::HttpError => "HTTP 錯誤",
            Self::TimeError => "時間異常",
            Self::RateLimited => "請求次數過多",
            Self::FileTooLarge => "檔案過大",
        }
    }

    pub const fn default_detail(self) -> Option<&'static str> {
        match self {
            Self::PermissionDenied => Some("設定檔唯讀，或沒有存取檔案、寫入資料夾的權限。"),
            Self::FileInUse => Some("設定檔或暫存檔被其他程式占用。"),
            Self::CorruptFile => Some("檔案無法解析、內容缺失或格式不符。"),
            Self::FullBackupSelected => Some("完整備份檔案請使用「還原完整備份」。"),
            Self::GroupBackupSelected => Some("單組備份檔案請使用「匯入單組備份」。"),
            Self::UnsupportedFilesystem => {
                Some("請將整個資料夾移動到 NTFS 格式的磁碟再進行操作。")
            }
            Self::TimeError => Some("時間偏差過大，無法產生正確驗證碼。"),
            Self::FileTooLarge => Some("超過 32 MiB 上限。"),
            _ => None,
        }
    }

    pub const fn notice(self) -> OperationError {
        OperationError {
            code: self,
            description: None,
            detail: None,
        }
    }

    pub fn with_detail(self, detail: impl Into<String>) -> OperationError {
        OperationError {
            detail: Some(detail.into()),
            ..self.notice()
        }
    }

    pub fn manual_delete(self, paths: impl Display) -> OperationError {
        self.with_detail(format!("請手動刪除下列檔案：\n{paths}"))
    }

    pub const fn retry_connection() -> OperationError {
        Self::ConnectionFailed
            .notice()
            .with_description("連線異常，請重試")
    }

    pub const fn rate_limit_retry() -> OperationError {
        Self::RateLimited
            .notice()
            .with_description("請求次數過多，請稍後重試")
    }

    pub fn http(status: u16) -> OperationError {
        match status {
            200..=299 => Self::retry_connection(),
            429 => Self::rate_limit_retry(),
            _ => Self::HttpError.with_detail(format!("服務回傳 HTTP {status}。")),
        }
    }

    pub fn matches_message(self, message: &str) -> bool {
        split_error_code(message).is_some_and(|(code, _)| code == self as u8)
    }

    pub fn time_deviation() -> OperationError {
        Self::TimeError
            .with_detail("")
            .with_description("時間偏差過大，請重新校時")
    }

    pub fn saved_offset_failure() -> OperationError {
        Self::TimeError
            .with_detail("")
            .with_description("校時偏移異常，請重新校時")
    }
}

#[derive(Debug)]
pub struct OperationError {
    code: ErrorCode,
    description: Option<&'static str>,
    detail: Option<String>,
}

impl OperationError {
    pub const fn with_description(mut self, description: &'static str) -> Self {
        self.description = Some(description);
        self
    }

    /// Existing workers carry rendered errors as strings. Split their first
    /// line for dialogs that use the description itself as the title.
    pub fn display_parts(message: &str) -> Option<(&str, Option<&str>)> {
        let (line, detail) = message
            .split_once('\n')
            .map_or((message, None), |(line, detail)| (line, Some(detail)));
        let (code, description) = split_error_code(line)?;
        if !(ErrorCode::PermissionDenied as u8..=ErrorCode::FileTooLarge as u8).contains(&code) {
            return None;
        }
        Some((description, detail.filter(|text| !text.is_empty())))
    }
}

fn split_error_code(message: &str) -> Option<(u8, &str)> {
    let (code, description) = message.split_once('：')?;
    let digits = code.strip_prefix('E')?;
    if digits.len() != 2 {
        return None;
    }
    Some((digits.parse().ok()?, description))
}

impl Display for OperationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let description = self.description.unwrap_or_else(|| self.code.description());
        write!(formatter, "E{:02}：{description}", self.code as u8)?;
        let detail = self
            .detail
            .as_deref()
            .or_else(|| self.code.default_detail());
        if let Some(detail) = detail.filter(|text| !text.is_empty()) {
            let separator = if self.code == ErrorCode::HttpError {
                '，'
            } else {
                '\n'
            };
            write!(formatter, "{separator}{detail}")?;
        }
        Ok(())
    }
}
