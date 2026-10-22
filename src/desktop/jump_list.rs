use std::{
    collections::BTreeSet,
    ffi::OsString,
    fs,
    os::windows::ffi::{OsStrExt, OsStringExt},
    path::{Path, PathBuf},
    sync::OnceLock,
};

use anyhow::{Context, Result, anyhow};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use sha2::{Digest, Sha256};
use windows::{
    Win32::{
        Foundation::{
            E_ACCESSDENIED, ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, HWND, PROPERTYKEY,
            RPC_E_CHANGED_MODE,
        },
        Storage::EnhancedStorage::{
            PKEY_AppUserModel_ID, PKEY_AppUserModel_RelaunchCommand,
            PKEY_AppUserModel_RelaunchDisplayNameResource, PKEY_AppUserModel_RelaunchIconResource,
            PKEY_Title,
        },
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx,
                CoUninitialize, StructuredStorage::PROPVARIANT,
            },
            Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RRF_SUBKEY_WOW6464KEY, RegGetValueW},
        },
        UI::Shell::{
            Common::{IObjectArray, IObjectCollection},
            DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW,
            PropertiesSystem::{
                IPropertyStore, PSCoerceToCanonicalValue, SHGetPropertyStoreForWindow,
            },
            SetCurrentProcessExplicitAppUserModelID, ShellLink,
        },
    },
    core::{BSTR, HSTRING, Interface, PCWSTR, w},
};

const APP_ID: &str = "Regressify.Desktop";
static APP_IDENTITY: OnceLock<String> = OnceLock::new();

fn installed_executable() -> Result<Option<PathBuf>> {
    let mut buffer = vec![0u16; 32768];
    let mut length = (buffer.len() * size_of::<u16>()) as u32;
    let result = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(
                "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\{ED6F0908-7E11-4608-A532-E43482AA117B}_is1"
            ),
            w!("InstallLocation"),
            RRF_RT_REG_SZ | RRF_SUBKEY_WOW6464KEY,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut length),
        )
    };
    if result == ERROR_FILE_NOT_FOUND || result == ERROR_PATH_NOT_FOUND {
        return Ok(None);
    }
    result
        .ok()
        .context("Cannot read the desktop installation location")?;
    let end = buffer
        .iter()
        .position(|unit| *unit == 0)
        .unwrap_or(buffer.len());
    Ok((end > 0)
        .then(|| PathBuf::from(OsString::from_wide(&buffer[..end])).join("regressify-desktop.exe")))
}

fn application_id(executable: &Path, installed: Option<&Path>) -> String {
    let normalize = |path: &Path| {
        fs::canonicalize(path)
            .unwrap_or_else(|_| path.to_path_buf())
            .as_os_str()
            .to_ascii_lowercase()
    };
    let executable = normalize(executable);
    if installed.is_some_and(|installed| normalize(installed) == executable) {
        return APP_ID.into();
    }
    let mut hash = Sha256::new();
    for unit in executable.encode_wide() {
        hash.update(unit.to_le_bytes());
    }
    format!("{APP_ID}.Portable.{:x}", hash.finalize())
}

const TASKBAR_PROPERTIES: [PROPERTYKEY; 4] = [
    PKEY_AppUserModel_RelaunchCommand,
    PKEY_AppUserModel_RelaunchIconResource,
    PKEY_AppUserModel_RelaunchDisplayNameResource,
    PKEY_AppUserModel_ID,
];

pub struct WindowTaskbar {
    properties: IPropertyStore,
}

impl WindowTaskbar {
    fn new(properties: IPropertyStore, executable: &Path, app_id: &str) -> Result<Self> {
        let taskbar = Self { properties };
        let path: Vec<_> = executable.as_os_str().encode_wide().collect();
        let mut command = vec![b'"' as u16];
        command.extend_from_slice(&path);
        command.push(b'"' as u16);
        let mut display_name = vec![b'@' as u16];
        display_name.extend_from_slice(&path);
        display_name.extend(",-101".encode_utf16());
        let mut icon = path;
        icon.extend(",-1".encode_utf16());
        let values = [
            PROPVARIANT::from(BSTR::from_wide(&command)),
            PROPVARIANT::from(BSTR::from_wide(&icon)),
            PROPVARIANT::from(BSTR::from_wide(&display_name)),
            PROPVARIANT::from(app_id),
        ];
        for (key, value) in TASKBAR_PROPERTIES.iter().zip(values) {
            unsafe { taskbar.properties.SetValue(key, &value)? };
        }
        Ok(taskbar)
    }
}

impl Drop for WindowTaskbar {
    fn drop(&mut self) {
        for key in TASKBAR_PROPERTIES {
            if let Err(error) = unsafe { self.properties.SetValue(&key, &PROPVARIANT::default()) } {
                crate::diagnostics::startup::warning(
                    crate::diagnostics::record::SafeValue::Shell,
                    &error.into(),
                );
            }
        }
    }
}

pub fn initialize_window(window: &gpui_kit::Window) -> Result<WindowTaskbar> {
    let handle = HasWindowHandle::window_handle(window)
        .map_err(|error| anyhow!("Cannot access Windows window handle: {error:?}"))?;
    let RawWindowHandle::Win32(handle) = handle.as_raw() else {
        return Err(anyhow!("The desktop window has no Windows handle"));
    };
    let properties = unsafe { SHGetPropertyStoreForWindow(HWND(handle.hwnd.get() as _))? };
    let app_id = APP_IDENTITY
        .get()
        .ok_or_else(|| anyhow!("Windows shell identity is not initialized"))?;
    WindowTaskbar::new(properties, &std::env::current_exe()?, app_id)
}

pub fn initialize() -> Result<()> {
    let app_id = application_id(
        &std::env::current_exe()?,
        installed_executable()?.as_deref(),
    );
    unsafe { SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(&app_id))? };
    APP_IDENTITY
        .set(app_id)
        .map_err(|_| anyhow!("Windows shell identity is already initialized"))?;
    Ok(())
}

pub fn record_workspace(workspace: &Path) {
    refresh(Some(workspace));
}

pub fn refresh(reopened: Option<&Path>) {
    let Some(app_id) = APP_IDENTITY.get() else {
        return;
    };
    let result = super::workspace::Preferences::path()
        .and_then(|path| refresh_for(&path, reopened, &std::env::current_exe()?, app_id));
    if let Err(error) = result {
        crate::diagnostics::startup::warning(crate::diagnostics::record::SafeValue::Shell, &error);
    }
}

struct ComApartment(bool);

impl ComApartment {
    fn enter() -> Result<Self> {
        let result = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        if result == RPC_E_CHANGED_MODE {
            return Ok(Self(false));
        }
        result.ok()?;
        Ok(Self(true))
    }
}

impl Drop for ComApartment {
    fn drop(&mut self) {
        if self.0 {
            unsafe { CoUninitialize() };
        }
    }
}

fn workspace_arguments(workspace: &Path) -> HSTRING {
    let path: Vec<_> = workspace.as_os_str().encode_wide().collect();
    let mut arguments: Vec<_> = "-- \"".encode_utf16().collect();
    arguments.extend_from_slice(&path);
    arguments.extend(path.iter().rev().take_while(|unit| **unit == b'\\' as u16));
    arguments.push(b'"' as u16);
    HSTRING::from_wide(&arguments)
}

fn workspace_link(executable: &Path, workspace: &Path) -> Result<IShellLinkW> {
    let label = workspace
        .file_name()
        .unwrap_or(workspace.as_os_str())
        .to_string_lossy();
    unsafe {
        let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
        link.SetPath(&HSTRING::from(executable))?;
        link.SetArguments(&workspace_arguments(workspace))?;
        link.SetDescription(&HSTRING::from(super::workspace::display_path(workspace)))?;
        link.SetIconLocation(&HSTRING::from(executable), 0)?;
        let properties: IPropertyStore = link.cast()?;
        let mut title = PROPVARIANT::from(label.as_ref());
        PSCoerceToCanonicalValue(&PKEY_Title, &mut title)?;
        properties.SetValue(&PKEY_Title, &title)?;
        properties.Commit()?;
        Ok(link)
    }
}

fn visible_workspaces(
    recent: &[PathBuf],
    removed: &BTreeSet<String>,
    slots: usize,
) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    recent
        .iter()
        .filter(|path| {
            path.is_dir()
                && seen.insert((*path).clone())
                && !removed.contains(&workspace_arguments(path).to_string())
        })
        .take(slots.min(10))
        .cloned()
        .collect()
}

fn removed_arguments(items: &IObjectArray) -> Result<BTreeSet<String>> {
    let mut removed = BTreeSet::new();
    unsafe {
        for index in 0..items.GetCount()? {
            if let Ok(link) = items.GetAt::<IShellLinkW>(index) {
                let mut arguments = vec![0; 32768];
                link.GetArguments(&mut arguments)?;
                removed.insert(PCWSTR(arguments.as_ptr()).to_string()?);
            }
        }
    }
    Ok(removed)
}

fn reconcile_removals(
    removed: &mut BTreeSet<String>,
    pending: BTreeSet<String>,
    reopened: Option<&str>,
) -> bool {
    let defer_reopen = reopened.is_some_and(|path| pending.contains(path));
    removed.extend(pending);
    if !defer_reopen && let Some(path) = reopened {
        removed.remove(path);
    }
    defer_reopen
}

fn refresh_for(
    preferences_path: &Path,
    reopened: Option<&Path>,
    executable: &Path,
    app_id: &str,
) -> Result<usize> {
    fs::create_dir_all(
        preferences_path
            .parent()
            .ok_or_else(|| anyhow!("Invalid preferences path"))?,
    )?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(preferences_path.with_extension("jump-list.lock"))?;
    lock.lock().context("Cannot lock the shared Jump List")?;
    let mut recent = super::workspace::Preferences::load(preferences_path)?.recent_workspaces;
    if let Some(workspace) = reopened {
        recent.retain(|path| path != workspace);
        recent.insert(0, workspace.to_path_buf());
    }
    recent.truncate(10);
    let removed_path = preferences_path.with_extension("jump-list-removed.json");
    let mut removed: BTreeSet<String> = match fs::read(&removed_path) {
        Ok(bytes) => serde_json::from_slice(&bytes).context("Invalid Jump List removal history")?,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
        Err(error) => return Err(error.into()),
    };
    let tracked: BTreeSet<_> = recent
        .iter()
        .map(|path| workspace_arguments(path).to_string())
        .collect();
    removed.retain(|arguments| tracked.contains(arguments));
    let reopened = reopened.map(|path| workspace_arguments(path).to_string());
    let _apartment = ComApartment::enter()?;
    unsafe {
        let destinations: ICustomDestinationList =
            CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)?;
        destinations.SetAppID(&HSTRING::from(app_id))?;
        let mut count = 0;
        for _ in 0..2 {
            let mut slots = 0;
            let pending: IObjectArray = destinations.BeginList(&mut slots)?;
            let result = (|| -> Result<(usize, bool)> {
                let defer_reopen = reconcile_removals(
                    &mut removed,
                    removed_arguments(&pending)?,
                    reopened.as_deref(),
                );
                crate::helpers::write_file_atomically(
                    &removed_path,
                    &serde_json::to_vec(&removed)?,
                )?;
                let visible = visible_workspaces(&recent, &removed, slots as usize);
                let links: IObjectCollection =
                    CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)?;
                for workspace in &visible {
                    links.AddObject(&workspace_link(executable, workspace)?)?;
                }
                let mut count = visible.len();
                if count > 0 {
                    let links: IObjectArray = links.cast()?;
                    if let Err(error) =
                        destinations.AppendCategory(&HSTRING::from("Recent Workspaces"), &links)
                    {
                        if error.code() == E_ACCESSDENIED {
                            count = 0;
                        } else {
                            return Err(error.into());
                        }
                    }
                }
                destinations.CommitList()?;
                Ok((count, defer_reopen))
            })();
            match result {
                Ok((published, defer_reopen)) => {
                    count = published;
                    if !defer_reopen {
                        break;
                    }
                }
                Err(error) => {
                    let _ = destinations.AbortList();
                    return Err(error);
                }
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        APP_ID, ComApartment, IPropertyStore, Interface, PKEY_Title, TASKBAR_PROPERTIES,
        WindowTaskbar, application_id, reconcile_removals, refresh_for, removed_arguments,
        visible_workspaces, workspace_arguments, workspace_link,
    };
    use std::{collections::BTreeSet, os::windows::ffi::OsStrExt, path::Path};
    use windows::{
        Win32::{
            Foundation::{HLOCAL, LocalFree},
            UI::Shell::CommandLineToArgvW,
        },
        core::{HSTRING, PCWSTR},
    };

    #[test]
    fn taskbar_window_metadata_uses_running_executable_and_is_cleared_on_drop() {
        let _apartment = ComApartment::enter().unwrap();
        for path in [
            r"C:\Program Files\Regressify Desktop\regressify-desktop.exe",
            r"\\server\share\Regressify Desktop\regressify-desktop.exe",
            "C:\\Workspaces\\\u{4f8b}\\regressify-desktop.exe",
        ] {
            let properties: IPropertyStore = unsafe {
                let link: super::IShellLinkW =
                    super::CoCreateInstance(&super::ShellLink, None, super::CLSCTX_INPROC_SERVER)
                        .unwrap();
                link.cast().unwrap()
            };
            let taskbar = WindowTaskbar::new(properties.clone(), Path::new(path), APP_ID).unwrap();
            let expected = [
                format!("\"{path}\""),
                format!("{path},-1"),
                format!("@{path},-101"),
                APP_ID.into(),
            ];
            for (key, expected) in TASKBAR_PROPERTIES.iter().zip(expected) {
                assert_eq!(
                    unsafe { properties.GetValue(key).unwrap() }.to_string(),
                    expected
                );
            }
            drop(taskbar);
            for key in TASKBAR_PROPERTIES {
                assert!(
                    unsafe { properties.GetValue(&key).unwrap() }
                        .to_string()
                        .is_empty()
                );
            }
        }
    }

    #[test]
    fn taskbar_identity_separates_portable_copies_from_installed_shortcuts() {
        let installed = Path::new(r"C:\Custom Install\regressify-desktop.exe");
        let debug = Path::new(r"D:\git\regressify-cli\target\debug\regressify-desktop.exe");
        let release = Path::new(r"D:\git\regressify-cli\target\release\regressify-desktop.exe");
        assert_eq!(application_id(installed, Some(installed)), APP_ID);
        assert_eq!(
            application_id(
                Path::new(r"c:\CUSTOM INSTALL\regressify-desktop.exe"),
                Some(installed)
            ),
            APP_ID
        );
        let portable = application_id(debug, Some(installed));
        assert!(portable.starts_with("Regressify.Desktop.Portable."));
        assert!(portable.len() <= 128);
        assert_eq!(portable, application_id(debug, None));
        assert_ne!(portable, application_id(release, Some(installed)));
        assert_eq!(
            portable,
            application_id(
                Path::new(r"d:\GIT\REGRESSIFY-CLI\TARGET\DEBUG\REGRESSIFY-DESKTOP.EXE"),
                None
            )
        );
    }

    #[test]
    fn installer_shortcuts_use_matching_app_id() {
        let installer = include_str!("../../installer/regressify.iss");
        for location in ["{group}", "{autodesktop}"] {
            let shortcut = format!("Name: \"{location}\\Regressify Desktop\";");
            assert!(installer.lines().any(|line| {
                line.starts_with(&shortcut)
                    && line.contains(&format!("AppUserModelID: \"{APP_ID}\""))
                    && line.contains("IconFilename: \"{app}\\regressify-desktop.exe\"")
            }));
        }
    }

    #[test]
    fn workspace_arguments_preserve_spaces_unicode_network_and_drive_roots() {
        for path in [
            r"C:\Workspaces\with spaces",
            r"C:\Workspaces\with spaces\",
            r"C:\",
            r"\\server\share\",
            r"\\?\C:\Workspaces\sample",
            "C:\\Workspaces\\\u{4f8b}",
        ] {
            let workspace = Path::new(path);
            let command = HSTRING::from(format!(
                "regressify-desktop.exe {}",
                workspace_arguments(workspace)
            ));
            unsafe {
                let mut count = 0;
                let arguments = CommandLineToArgvW(&command, &mut count);
                assert!(!arguments.is_null());
                let actual: Vec<_> = std::slice::from_raw_parts(arguments, count as usize)
                    .iter()
                    .map(|argument| argument.as_wide().to_vec())
                    .collect();
                LocalFree(Some(HLOCAL(arguments.cast())));
                assert_eq!(actual.len(), 3);
                assert_eq!(actual[1], "--".encode_utf16().collect::<Vec<_>>());
                assert_eq!(
                    actual[2],
                    workspace.as_os_str().encode_wide().collect::<Vec<_>>()
                );
            }
        }
    }

    #[test]
    fn shell_link_opens_workspace_with_title_and_icon() {
        let _apartment = ComApartment::enter().unwrap();
        let executable = std::env::current_exe().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let workspace = directory.path().join("Workspace with spaces \u{4f8b}");
        std::fs::create_dir(&workspace).unwrap();
        let link = workspace_link(&executable, &workspace).unwrap();
        unsafe {
            let mut buffer = vec![0; 32768];
            link.GetPath(&mut buffer, std::ptr::null_mut(), 0).unwrap();
            assert_eq!(
                PCWSTR(buffer.as_ptr()).to_string().unwrap(),
                executable.to_string_lossy()
            );
            link.GetArguments(&mut buffer).unwrap();
            assert_eq!(
                PCWSTR(buffer.as_ptr()).to_string().unwrap(),
                workspace_arguments(&workspace).to_string()
            );
            let mut icon_index = -1;
            link.GetIconLocation(&mut buffer, &mut icon_index).unwrap();
            assert_eq!(icon_index, 0);
            assert_eq!(
                PCWSTR(buffer.as_ptr()).to_string().unwrap(),
                executable.to_string_lossy()
            );
            let properties: IPropertyStore = link.cast().unwrap();
            assert_eq!(
                properties.GetValue(&PKEY_Title).unwrap().to_string(),
                "Workspace with spaces \u{4f8b}"
            );
        }
    }

    #[test]
    fn recent_workspaces_preserve_order_and_filter_duplicates_missing_and_removed() {
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("First workspace");
        let second = directory.path().join("Second workspace");
        let missing = directory.path().join("Missing workspace");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let recent = vec![first.clone(), missing, first.clone(), second.clone()];
        assert_eq!(
            visible_workspaces(&recent, &BTreeSet::new(), 10),
            vec![first.clone(), second.clone()]
        );
        assert_eq!(
            visible_workspaces(&recent, &BTreeSet::new(), 1),
            vec![first.clone()]
        );
        let removed = BTreeSet::from([workspace_arguments(&first).to_string()]);
        assert_eq!(visible_workspaces(&recent, &removed, 10), vec![second]);
        assert!(visible_workspaces(&recent, &removed, 0).is_empty());
    }

    #[test]
    fn native_removal_notifications_are_acknowledged_before_explicit_reopen() {
        let _apartment = ComApartment::enter().unwrap();
        let executable = std::env::current_exe().unwrap();
        let directory = tempfile::tempdir().unwrap();
        let arguments = workspace_arguments(directory.path()).to_string();
        let mut removed = BTreeSet::new();
        unsafe {
            let pending: super::IObjectCollection = super::CoCreateInstance(
                &super::EnumerableObjectCollection,
                None,
                super::CLSCTX_INPROC_SERVER,
            )
            .unwrap();
            pending
                .AddObject(&workspace_link(&executable, directory.path()).unwrap())
                .unwrap();
            let pending = removed_arguments(&pending.cast().unwrap()).unwrap();
            assert_eq!(pending, BTreeSet::from([arguments.clone()]));
            assert!(reconcile_removals(&mut removed, pending, Some(&arguments)));
        }
        assert!(removed.contains(&arguments));
        assert!(!reconcile_removals(&mut removed, BTreeSet::new(), None));
        assert!(removed.contains(&arguments));
        assert!(!reconcile_removals(
            &mut removed,
            BTreeSet::new(),
            Some(&arguments)
        ));
        assert!(removed.is_empty());
    }

    #[test]
    #[ignore = "writes an isolated Windows Jump List; requires recent-item tracking"]
    fn native_recent_workspaces_category_commits_and_preserves_removals() {
        use windows::Win32::{
            System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance},
            UI::Shell::{DestinationList, ICustomDestinationList},
        };

        struct TestList {
            app_id: HSTRING,
            list: ICustomDestinationList,
        }

        impl Drop for TestList {
            fn drop(&mut self) {
                unsafe {
                    let _ = self.list.DeleteList(&self.app_id);
                }
            }
        }

        let _apartment = ComApartment::enter().unwrap();
        let app_id = format!("Regressify.Desktop.Test.{}", uuid::Uuid::new_v4());
        let directory = tempfile::tempdir().unwrap();
        let first = directory.path().join("Workspace with spaces \u{4f8b}");
        let second = directory.path().join("Another workspace");
        std::fs::create_dir(&first).unwrap();
        std::fs::create_dir(&second).unwrap();
        let preferences_path = directory.path().join("settings.json");
        let preferences = super::super::workspace::Preferences {
            recent_workspaces: vec![first.clone(), second.clone(), first.clone()],
            ..Default::default()
        };
        preferences.save(&preferences_path).unwrap();
        let original_preferences = std::fs::read(&preferences_path).unwrap();
        let executable = std::env::current_exe().unwrap();
        unsafe {
            super::SetCurrentProcessExplicitAppUserModelID(&HSTRING::from(&app_id)).unwrap();
            let _cleanup = TestList {
                app_id: HSTRING::from(&app_id),
                list: CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER).unwrap(),
            };
            assert_eq!(
                refresh_for(&preferences_path, None, &executable, &app_id).unwrap(),
                2
            );
            let removed_path = preferences_path.with_extension("jump-list-removed.json");
            let removed = BTreeSet::from([workspace_arguments(&first).to_string()]);
            std::fs::write(&removed_path, serde_json::to_vec(&removed).unwrap()).unwrap();
            assert_eq!(
                refresh_for(&preferences_path, None, &executable, &app_id).unwrap(),
                1
            );
            assert_eq!(
                refresh_for(&preferences_path, None, &executable, &app_id).unwrap(),
                1
            );
            assert_eq!(
                refresh_for(&preferences_path, Some(&first), &executable, &app_id).unwrap(),
                2
            );
            assert_eq!(
                std::fs::read(&preferences_path).unwrap(),
                original_preferences
            );
            let removed: BTreeSet<String> =
                serde_json::from_slice(&std::fs::read(removed_path).unwrap()).unwrap();
            assert!(removed.is_empty());
        }
    }
}
