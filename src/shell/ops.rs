//! Shell file operations (IFileOperation): copy, move, delete, rename, create.
//!
//! All functions run on the UI thread (COM STA) and use the native shell
//! progress UI. Errors bubble up as `anyhow::Error`.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::anyhow;
#[cfg(test)]
use windows::Win32::Storage::FileSystem::FILE_ATTRIBUTE_DIRECTORY;
use windows::Win32::System::Com::{CLSCTX_ALL, CoCreateInstance, IBindCtx, IDataObject, IPersistFile};
use windows::Win32::System::Threading::CREATE_NO_WINDOW;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    BHID_SFUIObject, CMINVOKECOMMANDINFOEX, FOF_ALLOWUNDO, FileOperation, IContextMenu, IFileOperation,
    IFileOperationProgressSink, ILFindLastID, ILFree, IShellItem, IShellItemArray, IShellLinkW,
    OAIF_ALLOW_REGISTRATION, OAIF_EXEC, OPENASINFO, SHCreateDataObject, SHCreateItemFromIDList,
    SHCreateItemFromParsingName, SHCreateShellItemArrayFromIDLists, SHEmptyRecycleBinW, SHMultiFileProperties,
    SHOP_FILEPATH, SHObjectProperties, SHOpenWithDialog, SHParseDisplayName, ShellExecuteW, ShellLink,
};
#[cfg(test)]
use windows::Win32::UI::Shell::{
    FILEOPERATION_FLAGS, FOF_NOCONFIRMATION, FOF_RENAMEONCOLLISION, FOF_WANTNUKEWARNING, FOFX_RECYCLEONDELETE,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, Interface, PCSTR, PCWSTR};

#[cfg(test)]
pub fn copy(items: &[PathBuf], dest_dir: &Path) -> anyhow::Result<u32> {
    if items.is_empty() {
        return Ok(0);
    }
    // Pasting into the same folder duplicates with automatic rename ("foo - 副本.txt"),
    // matching Windows Explorer instead of triggering a self-replace conflict.
    let same_dir = items
        .iter()
        .any(|item| item.parent().is_some_and(|parent| parent.as_os_str().eq_ignore_ascii_case(dest_dir.as_os_str())));
    let flags = if same_dir { FOF_ALLOWUNDO | FOF_RENAMEONCOLLISION } else { FOF_ALLOWUNDO };
    let op = operation(flags)?;
    let dest = shell_item(dest_dir)?;
    for item in items {
        let src = shell_item(item)?;
        unsafe { op.CopyItem(&src, &dest, PCWSTR::null(), None::<&IFileOperationProgressSink>) }
            .map_err(|e| anyhow!("IFileOperation::CopyItem({}) failed: {e}", item.display()))?;
    }
    if same_dir {
        unsafe { op.PerformOperations() }.map_err(|e| anyhow!("IFileOperation::PerformOperations failed: {e}"))?;
        let aborted = unsafe { op.GetAnyOperationsAborted() }.map(|b| b.as_bool()).unwrap_or(false);
        return Ok(if aborted { 0 } else { items.len() as u32 });
    }
    perform(&op, items, |item| target_in(item, dest_dir).exists())
}

#[cfg(test)]
pub fn move_items(items: &[PathBuf], dest_dir: &Path) -> anyhow::Result<u32> {
    if items.is_empty() {
        return Ok(0);
    }
    let op = operation(FOF_ALLOWUNDO)?;
    let dest = shell_item(dest_dir)?;
    for item in items {
        let src = shell_item(item)?;
        unsafe { op.MoveItem(&src, &dest, PCWSTR::null(), None::<&IFileOperationProgressSink>) }
            .map_err(|e| anyhow!("IFileOperation::MoveItem({}) failed: {e}", item.display()))?;
    }
    perform(&op, items, |item| !item.exists() && target_in(item, dest_dir).exists())
}

/// Deletes through the shell, which owns the confirmation dialogs: the Recycle
/// Bin's own "confirm delete" setting, and the permanent-delete warning when a
/// file cannot be recycled or Shift+Delete was used. Suppressing those here would
/// either double-prompt (we already asked) or silently destroy data (we did not).
#[cfg(test)]
pub fn delete(items: &[PathBuf], permanent: bool) -> anyhow::Result<u32> {
    // 不传 FOF_NOCONFIRMATION：回收站删除让 shell 按系统设置决定是否询问，
    // 无法回收时由 FOF_WANTNUKEWARNING 弹出永久删除警告，FOFX_RECYCLEONDELETE 确保走回收站。
    let flags =
        if permanent { FOF_WANTNUKEWARNING } else { FOF_ALLOWUNDO | FOF_WANTNUKEWARNING | FOFX_RECYCLEONDELETE };
    delete_with_flags(items, flags)
}

/// 直接删除、不进回收站。用于撤销"新建/复制"产生的文件：这些操作由用户
/// 刚刚发起，重放方向相反，不应再弹一次确认。
#[cfg(test)]
pub fn delete_direct(items: &[PathBuf]) -> anyhow::Result<u32> {
    delete_with_flags(items, FOF_NOCONFIRMATION | FOF_WANTNUKEWARNING)
}

#[cfg(test)]
fn delete_with_flags(items: &[PathBuf], flags: FILEOPERATION_FLAGS) -> anyhow::Result<u32> {
    if items.is_empty() {
        return Ok(0);
    }
    let op = operation(flags)?;
    for item in items {
        let shell = shell_item(item)?;
        unsafe { op.DeleteItem(&shell, None::<&IFileOperationProgressSink>) }
            .map_err(|e| anyhow!("IFileOperation::DeleteItem({}) failed: {e}", item.display()))?;
    }
    perform(&op, items, |item| !item.exists())
}

#[cfg(test)]
pub fn rename(item: &Path, new_name: &str) -> anyhow::Result<PathBuf> {
    validate_name(new_name)?;
    let parent = item.parent().ok_or_else(|| anyhow!("rename: {} has no parent folder", item.display()))?;
    let op = operation(FILEOPERATION_FLAGS(0))?;
    let shell = shell_item(item)?;
    let wide = HSTRING::from(new_name);
    unsafe { op.RenameItem(&shell, &wide, None::<&IFileOperationProgressSink>) }
        .map_err(|e| anyhow!("IFileOperation::RenameItem({}) failed: {e}", item.display()))?;
    finish_single(&op, &format!("rename {}", item.display()))?;
    let new_path = parent.join(new_name);
    if !new_path.exists() {
        return Err(anyhow!("rename failed: {} is missing", new_path.display()));
    }
    Ok(new_path)
}

#[cfg(test)]
pub fn new_folder(parent: &Path, name: &str) -> anyhow::Result<PathBuf> {
    validate_name(name)?;
    let op = operation(FILEOPERATION_FLAGS(0))?;
    let dir = shell_item(parent)?;
    let wide = HSTRING::from(name);
    let path = parent.join(name);
    unsafe { op.NewItem(&dir, FILE_ATTRIBUTE_DIRECTORY.0, &wide, PCWSTR::null(), None::<&IFileOperationProgressSink>) }
        .map_err(|e| anyhow!("IFileOperation::NewItem({}) failed: {e}", path.display()))?;
    finish_single(&op, &format!("create folder {}", path.display()))?;
    if !path.is_dir() {
        return Err(anyhow!("create folder failed: {} is missing", path.display()));
    }
    Ok(path)
}

pub fn new_file(parent: &Path, name: &str, content: &[u8]) -> anyhow::Result<PathBuf> {
    validate_name(name)?;
    let path = parent.join(name);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(|e| anyhow!("create file {} failed: {e}", path.display()))?;
    file.write_all(content).map_err(|e| anyhow!("write {} failed: {e}", path.display()))?;
    Ok(path)
}

pub fn open(item: &Path) -> anyhow::Result<()> {
    shell_execute("open", item)
}

pub fn run_as_admin(item: &Path) -> anyhow::Result<()> {
    shell_execute("runas", item)
}

pub fn empty_recycle_bin() -> anyhow::Result<bool> {
    match unsafe { SHEmptyRecycleBinW(Some(crate::platform::hwnd()), PCWSTR::null(), 0) } {
        Ok(()) => Ok(true),
        Err(e) if is_user_cancelled_hr(e.code().0 as u32) => Ok(false),
        Err(e) => Err(anyhow!("清空回收站失败: {e}")),
    }
}

/// True for standard Windows Shell user-cancellation HRESULTs (`ERROR_CANCELLED`,
/// `COPYENGINE_E_USER_CANCELLED`, or `E_UNEXPECTED` returned by `SHEmptyRecycleBinW`
/// when the user dismisses the confirmation dialog).
pub fn is_user_cancelled_hr(code: u32) -> bool {
    matches!(code, 0x8027_0000 | 0x8007_04C7 | 0x8000_FFFF)
}

/// Invokes a shell canonical verb (e.g. `"undelete"`, `"eject"`, `"properties"`,
/// `"delete"`) on one or more absolute PIDLs (Recycle Bin items, Drives, etc.).
pub fn invoke_pidl_verb(pidls: &[std::sync::Arc<crate::shell::Pidl>], verb: &str) -> anyhow::Result<()> {
    if pidls.is_empty() {
        return Ok(());
    }
    let raw: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| p.as_ptr()).collect();
    let array: IShellItemArray = unsafe { SHCreateShellItemArrayFromIDLists(&raw) }
        .map_err(|e| anyhow!("SHCreateShellItemArrayFromIDLists failed: {e}"))?;
    let cm: IContextMenu = unsafe { array.BindToHandler(None::<&IBindCtx>, &BHID_SFUIObject) }
        .map_err(|e| anyhow!("BindToHandler(IContextMenu) failed: {e}"))?;
    let verb_c = std::ffi::CString::new(verb).map_err(|_| anyhow!("invalid verb"))?;
    let info = CMINVOKECOMMANDINFOEX {
        cbSize: std::mem::size_of::<CMINVOKECOMMANDINFOEX>() as u32,
        hwnd: crate::platform::hwnd(),
        lpVerb: PCSTR(verb_c.as_ptr().cast::<u8>()),
        nShow: SW_SHOWNORMAL.0,
        ..Default::default()
    };
    unsafe { cm.InvokeCommand(&info as *const _ as *const _) }
        .map_err(|e| anyhow!("Shell verb {verb:?} failed: {e}"))?;
    Ok(())
}

/// Invokes `"eject"` on a removable drive root (e.g. `E:\`).
pub fn eject_drive(root: &Path) -> anyhow::Result<()> {
    let loc = crate::shell::location::Location::from_path(root)?;
    let pidl = crate::shell::Pidl::from_bytes(&loc.pidl_bytes())
        .ok_or_else(|| anyhow!("无法定位驱动器 {}", root.display()))?;
    invoke_pidl_verb(&[std::sync::Arc::new(pidl)], "eject")
}

/// Restores only the recycled Shell items captured by the delete operation.
pub fn restore_from_recycle_bin(items: &[crate::shell::op_worker::RecycledItem]) -> anyhow::Result<usize> {
    if items.is_empty() {
        return Ok(0);
    }
    let op: IFileOperation = unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }?;
    unsafe { op.SetOperationFlags(FOF_ALLOWUNDO) }?;
    let hwnd = crate::platform::hwnd();
    if !hwnd.0.is_null() {
        unsafe { op.SetOwnerWindow(hwnd) }?;
    }
    for item in items {
        let parent = item.original.parent().ok_or_else(|| anyhow!("无法确定还原目录"))?;
        let name = item.original.file_name().ok_or_else(|| anyhow!("无法确定还原文件名"))?;
        let pidl = crate::shell::Pidl::from_bytes(&item.pidl).ok_or_else(|| anyhow!("回收站项目标识无效"))?;
        let source: IShellItem = unsafe { SHCreateItemFromIDList(pidl.as_ptr()) }?;
        let destination: IShellItem =
            unsafe { SHCreateItemFromParsingName(&HSTRING::from(parent), None::<&IBindCtx>) }?;
        unsafe { op.MoveItem(&source, &destination, &HSTRING::from(name), None::<&IFileOperationProgressSink>) }?;
    }
    unsafe { op.PerformOperations() }?;
    if unsafe { op.GetAnyOperationsAborted() }?.as_bool() {
        return Err(anyhow!("还原已取消，可再次按 Ctrl+Z 重试"));
    }
    Ok(items.len())
}

/// Creates a Windows `.lnk` shortcut pointing to `target` inside `dest_dir`.
pub fn create_shortcut(target: &Path, dest_dir: &Path) -> anyhow::Result<PathBuf> {
    let base = target
        .file_stem()
        .or_else(|| target.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "快捷方式".to_string());
    let mut name = format!("{base} - 快捷方式.lnk");
    let mut counter = 2usize;
    while dest_dir.join(&name).exists() && counter < 10_000 {
        name = format!("{base} - 快捷方式 ({counter}).lnk");
        counter += 1;
    }
    let link_path = dest_dir.join(&name);
    let link: IShellLinkW = unsafe { CoCreateInstance(&ShellLink, None, CLSCTX_ALL) }
        .map_err(|e| anyhow!("CoCreateInstance(ShellLink) failed: {e}"))?;
    let target_wide = HSTRING::from(target);
    unsafe { link.SetPath(&target_wide) }.map_err(|e| anyhow!("IShellLinkW::SetPath failed: {e}"))?;
    if let Some(parent) = target.parent() {
        let dir_wide = HSTRING::from(parent);
        let _ = unsafe { link.SetWorkingDirectory(&dir_wide) };
    }
    let persist: IPersistFile = link.cast().map_err(|e| anyhow!("QueryInterface(IPersistFile) failed: {e}"))?;
    let out_wide = HSTRING::from(link_path.as_path());
    unsafe { persist.Save(&out_wide, true) }.map_err(|e| anyhow!("IPersistFile::Save failed: {e}"))?;
    Ok(link_path)
}

pub fn open_with(item: &Path) -> anyhow::Result<()> {
    if shell_execute("openas", item).is_ok() {
        return Ok(());
    }
    let wide = HSTRING::from(item);
    let info = OPENASINFO {
        pcszFile: PCWSTR(wide.as_ptr()),
        pcszClass: PCWSTR::null(),
        oaifInFlags: OAIF_ALLOW_REGISTRATION | OAIF_EXEC,
    };
    unsafe { SHOpenWithDialog(Some(crate::platform::hwnd()), &info) }
        .map_err(|e| anyhow!("open with failed for {}: {e}", item.display()))
}

pub fn properties(items: &[PathBuf]) -> anyhow::Result<()> {
    match items {
        [] => Err(anyhow!("properties: no items given")),
        [item] => {
            let wide = HSTRING::from(item.as_path());
            let shown =
                unsafe { SHObjectProperties(Some(crate::platform::hwnd()), SHOP_FILEPATH, &wide, PCWSTR::null()) };
            if !shown.as_bool() {
                return Err(anyhow!("SHObjectProperties({}) failed", item.display()));
            }
            Ok(())
        }
        many => multi_properties(many),
    }
}

pub fn open_terminal(dir: &Path) -> anyhow::Result<()> {
    use std::os::windows::process::CommandExt;

    let mut failures: Vec<String> = Vec::new();
    for wt in wt_candidates() {
        match std::process::Command::new(&wt).arg("-d").arg(dir).creation_flags(CREATE_NO_WINDOW.0).spawn() {
            Ok(_) => return Ok(()),
            Err(e) => failures.push(format!("{}: {e}", wt.display())),
        }
    }
    // 回退终端靠子进程继承工作目录：Windows PowerShell 5.1 接受 -WorkingDirectory 却忽略它
    // （Win11 实测），拼 -Command 脚本又会在引号/空格上出错。
    for (program, args) in [("powershell.exe", &["-NoProfile", "-NoExit"][..]), ("cmd.exe", &["/K"][..])] {
        match std::process::Command::new(program).args(args).current_dir(dir).spawn() {
            Ok(_) => return Ok(()),
            Err(e) => failures.push(format!("{program}: {e}")),
        }
    }
    Err(anyhow!("open terminal in {} failed: {}", dir.display(), failures.join("; ")))
}

fn multi_properties(items: &[PathBuf]) -> anyhow::Result<()> {
    let parent = items[0].parent().ok_or_else(|| anyhow!("properties: {} has no parent folder", items[0].display()))?;
    for item in items {
        if item.parent() != Some(parent) {
            return Err(anyhow!("properties: {} is not inside {}", item.display(), parent.display()));
        }
    }
    let folder = Pidl::parse(parent)?;
    let mut full = Vec::with_capacity(items.len());
    for item in items {
        full.push(Pidl::parse(item)?);
    }
    let children: Vec<*const ITEMIDLIST> =
        full.iter().map(|pidl| unsafe { ILFindLastID(pidl.0) } as *const ITEMIDLIST).collect();
    let data: IDataObject =
        unsafe { SHCreateDataObject(Some(folder.0 as *const ITEMIDLIST), Some(&children), None::<&IDataObject>) }
            .map_err(|e| anyhow!("SHCreateDataObject({}) failed: {e}", parent.display()))?;
    unsafe { SHMultiFileProperties(&data, 0) }
        .map_err(|e| anyhow!("SHMultiFileProperties failed for {} item(s): {e}", items.len()))
}

#[cfg(test)]
fn operation(flags: FILEOPERATION_FLAGS) -> anyhow::Result<IFileOperation> {
    let op: IFileOperation = unsafe { CoCreateInstance(&FileOperation, None, CLSCTX_ALL) }
        .map_err(|e| anyhow!("CoCreateInstance(FileOperation) failed: {e}"))?;
    unsafe { op.SetOperationFlags(flags) }
        .map_err(|e| anyhow!("IFileOperation::SetOperationFlags({:#x}) failed: {e}", flags.0))?;
    unsafe { op.SetOwnerWindow(crate::platform::hwnd()) }
        .map_err(|e| anyhow!("IFileOperation::SetOwnerWindow failed: {e}"))?;
    Ok(op)
}

#[cfg(test)]
fn perform(op: &IFileOperation, items: &[PathBuf], landed: impl Fn(&Path) -> bool) -> anyhow::Result<u32> {
    unsafe { op.PerformOperations() }.map_err(|e| anyhow!("IFileOperation::PerformOperations failed: {e}"))?;
    Ok(items.iter().filter(|item| landed(item)).count() as u32)
}

#[cfg(test)]
fn finish_single(op: &IFileOperation, what: &str) -> anyhow::Result<()> {
    unsafe { op.PerformOperations() }.map_err(|e| anyhow!("{what} failed: {e}"))?;
    let aborted = unsafe { op.GetAnyOperationsAborted() }
        .map_err(|e| anyhow!("{what}: GetAnyOperationsAborted failed: {e}"))?
        .as_bool();
    if aborted {
        return Err(anyhow!("{what} was cancelled"));
    }
    Ok(())
}

#[cfg(test)]
fn shell_item(path: &Path) -> anyhow::Result<IShellItem> {
    let wide = HSTRING::from(path);
    unsafe { SHCreateItemFromParsingName(&wide, None::<&IBindCtx>) }
        .map_err(|e| anyhow!("SHCreateItemFromParsingName({}) failed: {e}", path.display()))
}

fn shell_execute(verb: &str, item: &Path) -> anyhow::Result<()> {
    let verb_wide = HSTRING::from(verb);
    let item_wide = HSTRING::from(item);
    let executed = unsafe {
        ShellExecuteW(
            Some(crate::platform::hwnd()),
            &verb_wide,
            &item_wide,
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    let code = executed.0 as isize;
    if code <= 32 {
        return Err(anyhow!("ShellExecuteW({verb}) failed for {}: code {code}", item.display()));
    }
    Ok(())
}

pub(crate) fn validate_name(name: &str) -> anyhow::Result<()> {
    if name.is_empty() {
        return Err(anyhow!("name must not be empty"));
    }
    if let Some(bad) =
        name.chars().find(|c| matches!(c, '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || *c < ' ')
    {
        return Err(anyhow!("name {name:?} contains the invalid character {bad:?}"));
    }
    if name.ends_with(' ') || name.ends_with('.') {
        return Err(anyhow!("name {name:?} must not end with a space or a dot"));
    }
    if name.encode_utf16().count() > 255 {
        return Err(anyhow!("name {name:?} is longer than 255 UTF-16 units"));
    }
    if reserved_device_name(name) {
        return Err(anyhow!("name {name:?} is a reserved Windows device name"));
    }
    Ok(())
}

/// CON / PRN / AUX / NUL / COM1-9 / LPT1-9 以及带扩展名的形式（CON.txt）都是设备名。
fn reserved_device_name(name: &str) -> bool {
    let stem = name.split('.').next().unwrap_or_default().to_ascii_uppercase();
    if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") {
        return true;
    }
    let bytes = stem.as_bytes();
    bytes.len() == 4 && matches!(&bytes[..3], b"COM" | b"LPT") && matches!(bytes[3], b'1'..=b'9')
}

#[cfg(test)]
fn target_in(item: &Path, dir: &Path) -> PathBuf {
    dir.join(item.file_name().unwrap_or_default())
}

fn wt_candidates() -> Vec<PathBuf> {
    let mut out = vec![PathBuf::from("wt.exe")];
    if let Ok(local) = std::env::var("LOCALAPPDATA") {
        out.push(Path::new(&local).join("Microsoft\\WindowsApps\\wt.exe"));
    }
    out
}

struct Pidl(*mut ITEMIDLIST);

impl Pidl {
    fn parse(path: &Path) -> anyhow::Result<Self> {
        let wide = HSTRING::from(path);
        let mut pidl = std::ptr::null_mut();
        unsafe { SHParseDisplayName(&wide, None::<&IBindCtx>, &mut pidl, 0, None) }
            .map_err(|e| anyhow!("SHParseDisplayName({}) failed: {e}", path.display()))?;
        Ok(Self(pidl))
    }
}

impl Drop for Pidl {
    fn drop(&mut self) {
        unsafe { ILFree(Some(self.0 as *const ITEMIDLIST)) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};

    struct ComApartment;

    impl ComApartment {
        fn init() -> Self {
            let hr = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
            hr.ok().expect("CoInitializeEx failed");
            Self
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            unsafe { CoUninitialize() };
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("clock").as_nanos();
        let dir = std::env::temp_dir().join(format!("lumen-ops-{tag}-{}-{stamp}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    #[test]
    fn create_folder_and_file() {
        let _com = ComApartment::init();
        let root = temp_dir("create");
        let folder = new_folder(&root, "nested").expect("new_folder");
        assert!(folder.is_dir());
        assert_eq!(folder, root.join("nested"));
        let file = new_file(&folder, "note.txt", b"lumen").expect("new_file");
        assert_eq!(std::fs::read(&file).expect("read"), b"lumen");
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn new_file_refuses_to_overwrite() {
        let _com = ComApartment::init();
        let root = temp_dir("create-existing");
        let file = new_file(&root, "keep.txt", b"original").expect("new_file");
        assert!(new_file(&root, "keep.txt", b"clobber").is_err());
        assert_eq!(std::fs::read(&file).expect("read"), b"original");
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn rename_roundtrip_and_invalid_names() {
        let _com = ComApartment::init();
        let root = temp_dir("rename");
        let file = new_file(&root, "before.txt", b"x").expect("new_file");
        let renamed = rename(&file, "after.txt").expect("rename");
        assert_eq!(renamed, root.join("after.txt"));
        assert!(!file.exists());
        assert_eq!(std::fs::read(&renamed).expect("read"), b"x");

        let folder = new_folder(&root, "old-name").expect("new_folder");
        let moved = rename(&folder, "new-name").expect("rename folder");
        assert!(moved.is_dir());

        for bad in [
            "",
            "a\\b",
            "a/b",
            "a:b",
            "a*b",
            "a?b",
            "a\"b",
            "a<b",
            "a>b",
            "a|b",
            "CON",
            "CON.txt",
            "con.txt",
            "NUL",
            "COM1",
            "LPT9.txt",
            "trailing.",
            "trailing ",
            "a\nb",
        ] {
            assert!(rename(&renamed, bad).is_err(), "{bad:?} should be rejected");
        }
        assert!(rename(&renamed, &"x".repeat(256)).is_err());
        assert!(renamed.exists());
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn copy_and_move_roundtrip() {
        let _com = ComApartment::init();
        let root = temp_dir("copy");
        let src = root.join("src");
        let dst = root.join("dst");
        let moved_to = root.join("moved");
        std::fs::create_dir_all(&src).expect("src");
        std::fs::create_dir_all(&dst).expect("dst");
        std::fs::create_dir_all(&moved_to).expect("moved");
        let a = new_file(&src, "a.txt", b"alpha").expect("a");
        let b = new_file(&src, "b.txt", b"beta").expect("b");

        assert_eq!(copy(&[a.clone(), b.clone()], &dst).expect("copy"), 2);
        assert_eq!(std::fs::read(dst.join("a.txt")).expect("read"), b"alpha");
        assert_eq!(std::fs::read(dst.join("b.txt")).expect("read"), b"beta");
        assert!(a.exists() && b.exists());

        let items = [dst.join("a.txt"), dst.join("b.txt")];
        assert_eq!(move_items(&items, &moved_to).expect("move"), 2);
        assert!(!items[0].exists() && !items[1].exists());
        assert_eq!(std::fs::read(moved_to.join("a.txt")).expect("read"), b"alpha");
        assert_eq!(std::fs::read(moved_to.join("b.txt")).expect("read"), b"beta");
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn delete_roundtrip() {
        let _com = ComApartment::init();
        let root = temp_dir("delete");
        let folder = new_folder(&root, "trashme").expect("new_folder");
        let nested = new_file(&folder, "junk.txt", b"junk").expect("new_file");
        let loose = new_file(&root, "loose.txt", b"x").expect("new_file");
        assert_eq!(delete(&[nested.clone(), loose.clone()], false).expect("delete"), 2);
        assert!(!nested.exists());
        assert!(!loose.exists());
        assert_eq!(delete(std::slice::from_ref(&folder), false).expect("delete folder"), 1);
        assert!(!folder.exists());
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn delete_direct_removes_permanently() {
        let _com = ComApartment::init();
        let root = temp_dir("delete-direct");
        let folder = new_folder(&root, "undone-folder").expect("new_folder");
        let nested = new_file(&folder, "created.txt", b"x").expect("new_file");
        let file = new_file(&root, "copied.txt", b"x").expect("new_file");

        assert_eq!(delete_direct(&[nested.clone(), file.clone()]).expect("delete_direct"), 2);
        assert!(!nested.exists());
        assert!(!file.exists());
        assert_eq!(delete_direct(std::slice::from_ref(&folder)).expect("delete folder"), 1);
        assert!(!folder.exists());
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn same_dir_copy_creates_renamed_duplicate_and_shortcut() {
        let _com = ComApartment::init();
        let root = temp_dir("dup-link");
        let file = new_file(&root, "note.txt", b"hello").expect("new_file");
        assert_eq!(copy(std::slice::from_ref(&file), &root).expect("same dir copy"), 1);
        let count = std::fs::read_dir(&root).unwrap().count();
        assert_eq!(count, 2, "copying into the same directory must create a renamed copy");

        let link = create_shortcut(&file, &root).expect("create_shortcut");
        assert!(link.exists());
        assert_eq!(link.extension().and_then(|e| e.to_str()), Some("lnk"));
        std::fs::remove_dir_all(&root).expect("cleanup");
    }

    #[test]
    fn restore_from_recycle_bin_only_restores_matching_directory() {
        let _com = ComApartment::init();
        let root = temp_dir("recycle-restore");
        let dir_a = root.join("dir_a");
        let dir_b = root.join("dir_b");
        std::fs::create_dir_all(&dir_a).unwrap();
        std::fs::create_dir_all(&dir_b).unwrap();

        let file_a = new_file(&dir_a, "same_name.txt", b"from-a").unwrap();
        let file_b = new_file(&dir_b, "same_name.txt", b"from-b").unwrap();

        let worker = crate::shell::op_worker::OpWorker::new();
        let recycle = |path: &PathBuf| {
            use crate::shell::op_worker::{OpRequest, OpResult};
            worker.submit(OpRequest::Delete { items: vec![path.clone()], permanent: false });
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
            loop {
                if let Some(finished) = worker.poll().into_iter().next() {
                    let OpResult::Deleted { items, count, .. } = finished.result.unwrap() else {
                        panic!("expected delete result")
                    };
                    assert_eq!(count, 1);
                    return items;
                }
                assert!(std::time::Instant::now() < deadline, "delete timed out");
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        };
        let recycled_a = recycle(&file_a);
        let recycled_b = recycle(&file_b);
        assert!(!file_a.exists() && !file_b.exists());

        // Restore ONLY dir_b/same_name.txt — dir_a/same_name.txt must NOT be restored!
        let restored = restore_from_recycle_bin(&recycled_b).expect("restore dir_b");
        assert_eq!(restored, 1);
        assert!(file_b.exists(), "expected dir_b/same_name.txt to be restored");
        assert_eq!(std::fs::read(&file_b).unwrap(), b"from-b");
        assert!(!file_a.exists(), "unrelated dir_a/same_name.txt must stay in Recycle Bin");

        // Restore dir_a/same_name.txt too so we don't leave test junk in the Recycle Bin.
        let restored_a = restore_from_recycle_bin(&recycled_a).expect("restore dir_a");
        assert_eq!(restored_a, 1);
        assert!(file_a.exists());
        assert_eq!(std::fs::read(&file_a).unwrap(), b"from-a");

        // Clean up permanently.
        let _ = delete_direct(&[file_a, file_b]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
