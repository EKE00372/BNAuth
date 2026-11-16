# BNAuth

BNAuth 是 Windows 戰網驗證器管理工具，以 Rust 與 egui／eframe 開發。可在本機管理驗證器、產生八位驗證碼，並將資料保存於程式所在目錄。

目前版本為 **0.2.0，仍在開發中**。

## 功能

- 手動匯入40位十六進位密鑰，或74位 WinAuth2 明文 `secretdata`。
- 匯入前預覽驗證器資料與目前驗證碼，方便與其他驗證器比對。
- 產生每30秒更新的八位驗證碼，支援複製與離線產碼。
- 所有驗證器共用校時；新密鑰匯入成功後會嘗試校時，也可手動校時。
- 分組、名稱搜尋、名稱與備註編輯、拖曳排序、批次移組及刪除。
- 以可直接閱讀、編輯的 JSON 資料檔自動保存，執行期間鎖住資料檔。
- 視窗置頂、拖動與調整大小；每次啟動使用固定尺寸及位置。

全庫最多1000筆驗證器、20個分組（含未分組），每組最多100筆。目前尚未提供密碼加密、程式內備份還原、XML 匯入／匯出，以及驗證器的線上建立、恢復或綁定。

## 下載

下載頁：[GitHub Releases](https://github.com/EKE00372/BNAuth/releases)。

目前尚未發佈 Release，也沒有可下載的 EXE；請依下方步驟自行編譯。

## 開始使用

1. 將 `BNAuth.exe` 放在本機 NTFS 磁碟中可寫入的資料夾，開啟程式。
2. 首次使用時按「建立資料」，建立空的 `BNAuth.bnauth` 資料檔。
3. 按「新增」，填寫名稱、選擇分組，貼上40位密鑰或74位明文資料。備註、序號與還原碼可選填；74位資料會自動帶入序號。
4. 按「預覽匯入」，核對資料及八位驗證碼，再按「確認匯入並保存」。相同密鑰不會重複新增，只會經確認後補齊缺少的資料。
5. 在清單中按複製圖示取得驗證碼，按時鐘圖示校時。離線時仍可產碼；尚未校時時使用本機時間，請保持電腦日期與時間正確。

### 資料保存

資料自動保存於 EXE 同目錄的 `BNAuth.bnauth`，**以明文 JSON 儲存，內含密鑰與還原資訊**。請妥善保管，不要公開或上傳資料檔。

需要備份時，先正常關閉程式並確認沒有保存或清理錯誤，再複製資料檔。若要用文字編輯器修改，也請先關閉程式並保留副本；修改後仍須符合資料格式與容量限制。

校時結果只供本次執行使用，不寫入資料檔。重新開啟程式後先使用本機時間，可再按時鐘圖示校時。

## 自行編譯

### 準備環境

- Windows x64。
- [Rust](https://www.rust-lang.org/tools/install) 1.95 或更新版本，使用 `x86_64-pc-windows-msvc` 工具鏈。
- [Microsoft C++ Build Tools](https://visualstudio.microsoft.com/visual-cpp-build-tools/)，安裝「使用 C++ 的桌面開發」（Desktop development with C++）工作負載及 Windows SDK。
- Git；也可從 GitHub 的「Code → Download ZIP」取得原始碼。

### 編譯 EXE

安裝完成後，開啟 Visual Studio 的 **x64 Native Tools Command Prompt**，執行：

```bat
git clone https://github.com/EKE00372/BNAuth.git
cd BNAuth
cargo build --release --locked
```

若使用 ZIP 取得原始碼，先切換到解壓縮後包含 `Cargo.toml` 的資料夾，再執行最後一行。

首次編譯需要連線下載依賴。完成後，執行檔預設位於 `target\release\BNAuth.exe`；可將它複製到其他可寫入的本機 NTFS 資料夾使用。

儲存庫中的 `build.cmd` 目前是維護者的本機建置腳本，包含固定工具鏈路徑。一般自行編譯請使用上述 Cargo 指令。

### 執行測試

在同一個開發命令提示字元、專案根目錄中執行：

```bat
cargo test --workspace --all-targets --locked
```

## 授權

BNAuth 代碼採 [MIT License](LICENSE)。第三方函式庫授權見 [THIRD_PARTY_NOTICES.txt](THIRD_PARTY_NOTICES.txt)。

介面字型來源為有愛黑體（Nowar Sans），採 [SIL Open Font License 1.1](assets/fonts/OFL-NowarSans.txt)；來源與版本見[字型說明](assets/fonts/NOTICE.md)。
