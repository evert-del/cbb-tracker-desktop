//! Windows: the taskbar icon's right-click menu (jump list) gets the app's
//! everyday shortcuts: the quick panel, a new tracker window and Saved for
//! offline. Each runs the app with a `tracker://` link, which the running
//! app receives through the single-instance plugin (route_deep_link). The
//! clock taps are left out on purpose: a link any page could open must
//! never call someone in or wrap them.

use std::path::Path;
use windows::core::{Interface, HSTRING, PWSTR};
use windows::Win32::Storage::EnhancedStorage::PKEY_Title;
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_APARTMENTTHREADED};
use windows::Win32::System::Variant::VT_LPWSTR;
use windows::Win32::UI::Shell::Common::{IObjectArray, IObjectCollection};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;
use windows::Win32::UI::Shell::{DestinationList, EnumerableObjectCollection, ICustomDestinationList, IShellLinkW, ShellLink};

pub(crate) const TASKS: [(&str, &str); 3] = [
    ("Quick panel", "tracker://panel"),
    ("New window", "tracker://new-window"),
    ("Saved for offline", "tracker://offline"),
];

pub(crate) fn set(exe: &Path) -> windows::core::Result<()> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let list: ICustomDestinationList = CoCreateInstance(&DestinationList, None, CLSCTX_INPROC_SERVER)?;
        let mut slots = 0u32;
        let _removed: IObjectArray = list.BeginList(&mut slots)?;
        let tasks: IObjectCollection = CoCreateInstance(&EnumerableObjectCollection, None, CLSCTX_INPROC_SERVER)?;
        let exe = HSTRING::from(exe.as_os_str());
        for (title, argument) in TASKS {
            let link: IShellLinkW = CoCreateInstance(&ShellLink, None, CLSCTX_INPROC_SERVER)?;
            link.SetPath(&exe)?;
            link.SetArguments(&HSTRING::from(argument))?;
            link.SetIconLocation(&exe, 0)?;
            let title = HSTRING::from(title);
            let mut value = PROPVARIANT::default();
            let inner = &mut *value.Anonymous.Anonymous;
            inner.vt = VT_LPWSTR;
            inner.Anonymous.pwszVal = PWSTR(title.as_ptr() as *mut u16);
            let store: IPropertyStore = link.cast()?;
            store.SetValue(&PKEY_Title, &value)?;
            store.Commit()?;
            tasks.AddObject(&link)?;
        }
        let array: IObjectArray = tasks.cast()?;
        list.AddUserTasks(&array)?;
        list.CommitList()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_task_is_a_tracker_link_the_app_routes() {
        for (_, link) in TASKS {
            let url: url::Url = link.parse().expect("a valid link");
            assert_eq!(url.scheme(), "tracker");
            assert!(crate::is_app_action(&url), "{link}");
        }
    }
}
