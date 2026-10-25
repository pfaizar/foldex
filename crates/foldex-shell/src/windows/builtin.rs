//! Only the Windows New handler and registered Windows compression commands
//! are instantiated here. All calls occur on the bounded system-menu STA.

use super::*;
use crate::{
    BuiltinItem, BuiltinKind, BuiltinSnapshot,
    builtin::{Backend, Target},
};
use std::{
    ffi::OsString, fs, os::windows::ffi::OsStringExt, path::PathBuf, sync::atomic::AtomicU32,
};
use windows_sys::Win32::{
    Foundation::{E_NOTIMPL, POINTL},
    System::{
        Com::{CLSCTX_INPROC_SERVER, CLSIDFromString, CoCreateInstance},
        LibraryLoader::{
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            GetModuleFileNameW, GetModuleHandleExW, GetModuleHandleW, GetProcAddress,
        },
        Ole::{OleInitialize, OleUninitialize},
        Registry::{
            HKEY, HKEY_CLASSES_ROOT, HKEY_LOCAL_MACHINE, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ,
            RegGetValueW,
        },
        SystemInformation::GetSystemDirectoryW,
    },
    UI::Shell::{
        CLSID_NewMenu, FOLDERID_SendTo, SHCNE_CREATE, SHCNF_PATHW, SHChangeNotify,
        SHCreateShellItemArrayFromIDLists, SHGetKnownFolderPath, SHLoadIndirectString,
    },
};

const IID_SHELL_EXT_INIT: GUID = GUID::from_u128(0x000214e8_0000_0000_c000_000000000046);
const IID_EXPLORER_COMMAND: GUID = GUID::from_u128(0xa08ce4d0_fa25_44ab_b57c_c7b1c323e0b9);
const IID_DROP_TARGET: GUID = GUID::from_u128(0x00000122_0000_0000_c000_000000000046);
const IID_UNKNOWN: GUID = GUID::from_u128(0x00000000_0000_0000_c000_000000000046);
const IID_OLE_WINDOW: GUID = GUID::from_u128(0x00000114_0000_0000_c000_000000000046);
const IID_OBJECT_WITH_SITE: GUID = GUID::from_u128(0xfc4801a3_2ba9_11cf_a229_00aa003d7352);
const IID_PERSIST_FILE: GUID = GUID::from_u128(0x0000010b_0000_0000_c000_000000000046);
const IID_PERSIST: GUID = GUID::from_u128(0x0000010c_0000_0000_c000_000000000046);
const IID_INITIALIZE_COMMAND: GUID = GUID::from_u128(0x85075acf_231f_40ea_9610_d26b7b58f638);
const IID_PROPERTY_BAG: GUID = GUID::from_u128(0x55272a00_42cb_11ce_8135_00aa004bb851);
const ZIP_SENDTO_CLASS: GUID = GUID::from_u128(0x888dca60_fc0a_11cf_8f0f_00c04fd7d062);
const ZIP_SENDTO_CLASS_KEY: &str = "CLSID\\{888DCA60-FC0A-11CF-8F0F-00C04FD7D062}";
const COMMAND_STORE: &str =
    "SOFTWARE\\Microsoft\\Windows\\CurrentVersion\\Explorer\\CommandStore\\shell";
const COMPRESSION_COMMANDS: [(&str, &str); 3] = [
    ("Windows.CompressTo.7Z", "7z archive"),
    ("Windows.CompressTo.Tar", "TAR archive"),
    ("Windows.CompressTo.Wizard", "Additional options…"),
];

#[repr(C)]
struct InitVtable {
    unknown: UnknownVtable,
    initialize:
        unsafe extern "system" fn(*mut c_void, *const ITEMIDLIST, *mut c_void, HKEY) -> HRESULT,
}
#[repr(C)]
struct ExplorerCommandVtable {
    unknown: UnknownVtable,
    title: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut *mut u16) -> HRESULT,
    icon: usize,
    tooltip: usize,
    canonical: unsafe extern "system" fn(*mut c_void, *mut GUID) -> HRESULT,
    state: unsafe extern "system" fn(*mut c_void, *mut c_void, i32, *mut u32) -> HRESULT,
    invoke: unsafe extern "system" fn(*mut c_void, *mut c_void, *mut c_void) -> HRESULT,
    flags: usize,
    children: usize,
}
#[repr(C)]
struct DropTargetVtable {
    unknown: UnknownVtable,
    enter: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, POINTL, *mut u32) -> HRESULT,
    over: usize,
    leave: unsafe extern "system" fn(*mut c_void) -> HRESULT,
    drop: unsafe extern "system" fn(*mut c_void, *mut c_void, u32, POINTL, *mut u32) -> HRESULT,
}
#[repr(C)]
struct ObjectWithSiteVtable {
    unknown: UnknownVtable,
    set_site: unsafe extern "system" fn(*mut c_void, *mut c_void) -> HRESULT,
    get_site: usize,
}

#[repr(C)]
struct PersistFileVtable {
    unknown: UnknownVtable,
    class_id: unsafe extern "system" fn(*mut c_void, *mut GUID) -> HRESULT,
    dirty: usize,
    load: unsafe extern "system" fn(*mut c_void, *const u16, u32) -> HRESULT,
    save: usize,
    saved: usize,
    current_file: usize,
}
#[repr(C)]
struct InitializeCommandVtable {
    unknown: UnknownVtable,
    initialize: unsafe extern "system" fn(*mut c_void, *const u16, *mut c_void) -> HRESULT,
}

struct OleApartment;
impl OleApartment {
    fn new() -> Result<Self, String> {
        // OLE drag/drop requires OleInitialize, beyond CoInitializeEx(STA).
        let hr = unsafe { OleInitialize(ptr::null()) };
        if hr < 0 {
            Err(failure("system menu OLE initialization", hr))
        } else {
            Ok(Self)
        }
    }
}
impl Drop for OleApartment {
    fn drop(&mut self) {
        unsafe {
            OleUninitialize();
        }
    }
}

pub(crate) struct Native {
    // Shell objects and their window site must release before the apartment.
    menu: Option<Session>,
    command: Option<Com>,
    data: Option<Com>,
    site: Option<Com>,
    apartment: Result<OleApartment, String>,
}
impl Native {
    pub(crate) fn new() -> Self {
        Self {
            menu: None,
            command: None,
            data: None,
            site: None,
            apartment: OleApartment::new(),
        }
    }
    fn clear(&mut self) {
        self.menu = None;
        self.command = None;
        self.data = None;
        self.site = None;
    }
    fn new_command(&mut self, target: &Target, key: &str) -> Result<(), String> {
        let directory = directory(target)?;
        // An exact .txt fallback remains available when no application has
        // registered .txt\ShellNew. It never overwrites an existing name.
        let mut menu = match new_menu(target.owner, &directory) {
            Ok(menu) => menu,
            Err(_) if key == "new:text" => return create_text(&directory),
            Err(error) => return Err(error),
        };
        let definitions = new_definitions(&mut menu)?;
        let mut matches = definitions
            .iter()
            .filter(|definition| definition.item.key == key);
        let found = matches
            .next()
            .ok_or("The registered New template is no longer available")?;
        if matches.next().is_some() {
            return Err("The registered New template is ambiguous".into());
        }
        let result = match found.native_id {
            Some(id) => menu.invoke(id),
            None if key == "new:text" => create_text(&directory),
            None => Err("The registered New template is no longer available".into()),
        };
        self.menu = Some(menu);
        result
    }
    fn compress_command(&mut self, target: &Target, key: &str) -> Result<(), String> {
        if matches!(
            key,
            "compress:legacy-zip" | "compress:Windows.CompressTo.Zip"
        ) {
            // Never instantiate Windows.CompressTo.Zip's generic CommandStore
            // Action Handler. Without its Explorer registry/site initialization
            // that dispatcher can activate the default verb of the selection.
            // ZIP is bound directly to the verified native ZIP SendTo class.
            return self.legacy_zip(target);
        }
        let name = key
            .strip_prefix("compress:")
            .filter(|name| COMPRESSION_COMMANDS.iter().any(|(known, _)| known == name))
            .ok_or("Unknown Windows compression command")?;
        let command = compression_command(name)
            .ok_or("This Windows compression format is no longer available")?;
        let selection = shell_selection(&target.paths)?;
        let site = window_site(target.owner);
        if let Some(site) = &site {
            set_site(&command, site);
        }
        let mut state = 0;
        let hr = unsafe {
            (command.vtable::<ExplorerCommandVtable>().state)(command.0, selection.0, 1, &mut state)
        };
        compression_state(hr, state)?;
        let hr = unsafe {
            (command.vtable::<ExplorerCommandVtable>().invoke)(
                command.0,
                selection.0,
                ptr::null_mut(),
            )
        };
        self.command = Some(command);
        self.data = Some(selection);
        self.site = site;
        if hr < 0 {
            Err(failure("native compression", hr))
        } else {
            Ok(())
        }
    }
    fn legacy_zip(&mut self, target: &Target) -> Result<(), String> {
        // Resolve no association-dependent UI object for the SendTo file.
        // Instantiate the registered Windows ZIP CLSID itself, validate its
        // IPersistFile class identity, and load only that known SendTo file.
        let (drop_target, _) = verified_zip_drop_target()?;
        let pidls = target
            .paths
            .iter()
            .map(|path| Pidl::for_path(path))
            .collect::<Result<Vec<_>, _>>()?;
        // Supply exactly the captured filesystem paths/CIDA. This never asks
        // a selected file's default application for a data object or verb.
        let data = absolute_selection_data(&pidls, &target.paths)?;
        let site = window_site(target.owner);
        if let Some(site) = &site {
            set_site(&drop_target, site);
        }
        let mut effect = 1; // DROPEFFECT_COPY: archive the selection without moving it.
        let point = POINTL::default();
        let hr = unsafe {
            (drop_target.vtable::<DropTargetVtable>().enter)(
                drop_target.0,
                data.0,
                1,
                point,
                &mut effect,
            )
        };
        if hr != 0 {
            return Err(failure("prepare native ZIP compression", hr));
        }
        if effect & 1 == 0 {
            unsafe {
                (drop_target.vtable::<DropTargetVtable>().leave)(drop_target.0);
            }
            return Err("Windows cannot ZIP this selection".into());
        }
        let hr = unsafe {
            (drop_target.vtable::<DropTargetVtable>().drop)(
                drop_target.0,
                data.0,
                1,
                point,
                &mut effect,
            )
        };
        self.command = Some(drop_target);
        self.data = Some(data);
        self.site = site;
        accepted_zip_drop(hr, effect)
    }
}
impl Backend for Native {
    fn discover(&mut self, kind: BuiltinKind, target: &Target) -> Result<BuiltinSnapshot, String> {
        self.apartment.as_ref().map_err(Clone::clone)?;
        validate_owner(target.owner)?;
        match kind {
            BuiltinKind::New => {
                let mut menu = new_menu(target.owner, &directory(target)?)?;
                let definitions = new_definitions(&mut menu)?;
                Ok(BuiltinSnapshot {
                    items: definitions
                        .into_iter()
                        .map(|definition| definition.item)
                        .collect::<Vec<_>>()
                        .into(),
                    truncated: menu.identities.len() >= MAX_ITEMS,
                })
            }
            BuiltinKind::Compress => {
                let selection = shell_selection(&target.paths)?;
                let mut items = Vec::new();
                if diagnostics_enabled() {
                    diagnose_zip_wrapper(&selection);
                }
                let verified_zip = verified_zip_drop_target();
                if let Ok((drop_target, path)) = &verified_zip {
                    if diagnostics_enabled() {
                        eprintln!(
                            "foldex-native-menu ZIP: class={} target={} drop_module={} external_activation=false",
                            guid_text(&ZIP_SENDTO_CLASS),
                            path.display(),
                            interface_module(
                                drop_target.vtable::<DropTargetVtable>().drop as usize
                            )
                            .unwrap_or_default()
                        );
                    }
                    items.push(BuiltinItem {
                        key: "compress:legacy-zip".into(),
                        label: "ZIP archive".into(),
                    });
                } else if diagnostics_enabled() {
                    if let Err(error) = verified_zip {
                        eprintln!(
                            "foldex-native-menu ZIP: verification failed: {error}; generic CommandStore wrapper is never invoked"
                        );
                    }
                }
                for (name, fallback) in COMPRESSION_COMMANDS {
                    let Some(command) = compression_command(name) else {
                        if diagnostics_enabled() {
                            eprintln!(
                                "foldex-native-menu {name}: dedicated handler registration unavailable or unverified"
                            );
                        }
                        continue;
                    };
                    let label =
                        command_title(&command, &selection).unwrap_or_else(|| fallback.into());
                    if diagnostics_enabled() {
                        diagnose_compression_command(name, &command, &selection, &label);
                    }
                    items.push(BuiltinItem {
                        key: format!("compress:{name}"),
                        label,
                    });
                }
                Ok(BuiltinSnapshot {
                    items: items.into(),
                    truncated: false,
                })
            }
        }
    }
    fn invoke(&mut self, kind: BuiltinKind, target: &Target, key: &str) -> Result<(), String> {
        self.apartment.as_ref().map_err(Clone::clone)?;
        validate_owner(target.owner)?;
        self.clear();
        // Native wizards/progress windows can outlive Invoke. Keep their COM
        // thread reference and pump messages until Windows releases the lane.
        let mut lifetime = PropertiesThread::new()?;
        let result = match kind {
            BuiltinKind::New => self.new_command(target, key),
            BuiltinKind::Compress if !target.background => self.compress_command(target, key),
            BuiltinKind::Compress => Err("Compression requires selected items".into()),
        };
        lifetime.activated = kind == BuiltinKind::Compress && result.is_ok();
        // Release our own command/data references before waiting. An object
        // can itself hold the Shell thread reference; retaining it here would
        // keep the lane alive even after its native operation has completed.
        self.clear();
        drop(lifetime);
        result
    }
    fn pump(&mut self) {
        let mut message = MSG::default();
        for _ in 0..32 {
            if unsafe { PeekMessageW(&mut message, ptr::null_mut(), 0, 0, PM_REMOVE) } == 0 {
                break;
            }
            unsafe {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }
}

fn validate_owner(owner: isize) -> Result<(), String> {
    if owner != 0 && unsafe { IsWindow(owner as HWND) } == 0 {
        Err("system menu owner window no longer exists".into())
    } else {
        Ok(())
    }
}

fn compression_state(hr: HRESULT, state: u32) -> Result<(), String> {
    // A verified, dedicated compression class can omit GetState. Its output is valid
    // only on success (the documented GetState contract), so E_NOTIMPL cannot
    // be interpreted as DISABLED/HIDDEN. The explicit Invoke remains the
    // authority on whether this registered native action can actually run.
    // https://learn.microsoft.com/windows/win32/api/shobjidl_core/nf-shobjidl_core-iexplorercommand-getstate
    if hr == E_NOTIMPL {
        return Ok(());
    }
    if hr < 0 {
        return Err(failure("check native compression command", hr));
    }
    // ECS_ENABLED is zero; a real DISABLED/HIDDEN result must never execute.
    if state != 0 {
        return Err("Windows cannot compress this selection using that format".into());
    }
    Ok(())
}

fn accepted_zip_drop(hr: HRESULT, effect: u32) -> Result<(), String> {
    if hr != 0 {
        return Err(failure("native ZIP drop was not accepted", hr));
    }
    if effect != 1 {
        return Err("Windows ZIP cancelled the drop or accepted no items".into());
    }
    Ok(())
}
fn directory(target: &Target) -> Result<PathBuf, String> {
    let first = target.paths.first().ok_or("New needs an open folder")?;
    if target.background {
        return Ok(first.clone());
    }
    let parent = first.parent().ok_or("New needs an open folder")?;
    if target
        .paths
        .iter()
        .any(|path| path.parent() != Some(parent))
    {
        return Err("Open a folder to create an item in this selection".into());
    }
    Ok(parent.to_path_buf())
}

fn create_instance(class: &GUID, iid: &GUID, operation: &str) -> Result<Com, String> {
    let mut pointer = ptr::null_mut();
    let hr = unsafe {
        CoCreateInstance(
            class,
            ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            iid,
            &mut pointer,
        )
    };
    Com::received(hr, pointer, operation)
}
fn new_menu(owner: isize, directory: &Path) -> Result<Session, String> {
    let pidl = Pidl::for_path(directory)?;
    let context = create_instance(&CLSID_NewMenu, &IID_CONTEXT, "Windows New menu")?;
    let init = context
        .query(&IID_SHELL_EXT_INIT)
        .ok_or("Windows New menu cannot initialize this folder")?;
    let hr = unsafe {
        (init.vtable::<InitVtable>().initialize)(init.0, pidl.0, ptr::null_mut(), ptr::null_mut())
    };
    if hr < 0 {
        return Err(failure("initialize Windows New menu", hr));
    }
    let context3 = context.query(&IID_CONTEXT3);
    let context2 = if context3.is_none() {
        context.query(&IID_CONTEXT2)
    } else {
        None
    };
    let menu = Menu::new()?;
    let hr = unsafe {
        (context.vtable::<ContextVtable>().query_menu)(
            context.0,
            menu.0,
            0,
            COMMAND_FIRST,
            COMMAND_LAST,
            0,
        )
    };
    if hr < 0 {
        return Err(failure("enumerate Windows New menu", hr));
    }
    let mut desktop = ptr::null_mut();
    let desktop_hr = unsafe { SHGetDesktopFolder(&mut desktop) };
    Ok(Session {
        menu,
        context3,
        context2,
        context,
        _folder: Com::received(desktop_hr, desktop, "New desktop folder")?,
        _pidls: vec![pidl],
        owner: owner as HWND,
        directory: parsing_name(directory),
        command_end: COMMAND_FIRST
            .saturating_add((hr as u32) & 0xffff)
            .min(COMMAND_LAST + 1),
        identities: HashMap::new(),
        next_id: 1,
        nodes: HashMap::new(),
        expanded: HashSet::new(),
    })
}
struct Definition {
    item: BuiltinItem,
    native_id: Option<u32>,
}
fn new_definitions(menu: &mut Session) -> Result<Vec<Definition>, String> {
    let mut snapshot = menu.snapshot()?;
    // CLSID_NewMenu contributes one New submenu. Its first two commands are the
    // system Folder and Shortcut entries; remaining rows are ShellNew types.
    if let Some(root) = snapshot.items.iter().find(|item| item.has_submenu) {
        snapshot = menu.expand(root.id)?;
    }
    let items = if let Some(root) = snapshot.items.iter().find(|item| item.has_submenu) {
        &root.children
    } else {
        &snapshot.items
    };
    let leaves: Vec<_> = items
        .iter()
        .filter(|item| !item.separator && !item.has_submenu && item.enabled)
        .collect();
    if leaves.len() < 2 {
        return Err("Windows New menu did not supply Folder and Shortcut".into());
    }
    let text_label = text_type_label();
    let mut text = false;
    let mut definitions = Vec::new();
    for (index, item) in leaves.into_iter().enumerate() {
        let key = match index {
            0 => "new:folder".to_owned(),
            1 => "new:shortcut".to_owned(),
            _ if text_label
                .as_ref()
                .is_some_and(|label| item.label.eq_ignore_ascii_case(label)) =>
            {
                text = true;
                "new:text".to_owned()
            }
            _ => format!(
                "new:template:{}:{}",
                item.canonical_verb.as_deref().unwrap_or(""),
                item.label
            ),
        };
        definitions.push(Definition {
            item: BuiltinItem {
                key,
                label: item.label.clone(),
            },
            native_id: Some(item.id),
        });
    }
    if !text {
        definitions.insert(
            2,
            Definition {
                item: BuiltinItem {
                    key: "new:text".into(),
                    label: "Text document (.txt)".into(),
                },
                native_id: None,
            },
        );
    }
    Ok(definitions)
}

fn text_type_label() -> Option<String> {
    let progid = registry_string(HKEY_CLASSES_ROOT, ".txt", "")?;
    let raw = registry_string(HKEY_CLASSES_ROOT, &progid, "FriendlyTypeName")
        .or_else(|| registry_string(HKEY_CLASSES_ROOT, &progid, ""))?;
    indirect_string(&raw).or(Some(raw))
}
fn create_text(parent: &Path) -> Result<(), String> {
    for index in 1..=1000 {
        let name = if index == 1 {
            "New Text Document.txt".into()
        } else {
            format!("New Text Document ({index}).txt")
        };
        let path = parent.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(file) => {
                drop(file);
                let name = parsing_name(&path);
                unsafe {
                    SHChangeNotify(
                        SHCNE_CREATE as i32,
                        SHCNF_PATHW,
                        name.as_ptr().cast(),
                        ptr::null(),
                    );
                }
                return Ok(());
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("Cannot create text document: {error}")),
        }
    }
    Err("Cannot choose an unused text document name".into())
}

fn registry_string(root: HKEY, key: &str, value: &str) -> Option<String> {
    let key: Vec<_> = key.encode_utf16().chain(Some(0)).collect();
    let value: Vec<_> = value.encode_utf16().chain(Some(0)).collect();
    let mut units = [0u16; 2048];
    let mut bytes = mem::size_of_val(&units) as u32;
    let status = unsafe {
        RegGetValueW(
            root,
            key.as_ptr(),
            value.as_ptr(),
            RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ,
            ptr::null_mut(),
            units.as_mut_ptr().cast(),
            &mut bytes,
        )
    };
    if status != 0 {
        return None;
    }
    let end = units.iter().position(|unit| *unit == 0)?;
    (end > 0).then(|| String::from_utf16_lossy(&units[..end]))
}
fn indirect_string(raw: &str) -> Option<String> {
    if !raw.starts_with('@') {
        return Some(raw.into());
    }
    let source: Vec<_> = raw.encode_utf16().chain(Some(0)).collect();
    let mut buffer = [0u16; 512];
    let hr = unsafe {
        SHLoadIndirectString(
            source.as_ptr(),
            buffer.as_mut_ptr(),
            buffer.len() as u32,
            ptr::null(),
        )
    };
    if hr < 0 {
        return None;
    }
    let end = buffer.iter().position(|unit| *unit == 0)?;
    (end > 0).then(|| String::from_utf16_lossy(&buffer[..end]))
}
fn compression_command(name: &str) -> Option<Com> {
    // The ZIP CommandStore entry points to a generic Action Handler that needs
    // Explorer-specific initialization. Never CoCreate/Invoke that dispatcher.
    // These three classes are the dedicated Windows zipfldr format commands.
    let expected = dedicated_compression_class(name)?;
    let raw = registry_string(
        HKEY_LOCAL_MACHINE,
        &format!("{COMMAND_STORE}\\{name}"),
        "ExplorerCommandHandler",
    )?;
    let class = parse_guid(&raw)?;
    if !same_guid(&class, &expected)
        || system_zip_server(&format!("CLSID\\{}", guid_text(&class))).is_err()
    {
        return None;
    }
    let command =
        create_instance(&class, &IID_EXPLORER_COMMAND, "Windows compression command").ok()?;
    // Verify the code behind the actual invoked slot, beyond registry text.
    if !is_system_zip_dll(&interface_module(
        command.vtable::<ExplorerCommandVtable>().invoke as usize,
    )?) {
        return None;
    }
    // If the handler exposes IInitializeCommand, it must receive the exact
    // registered verb name and a read-only bag for that verb's registry key.
    // A failure here rejects the definition and never reaches Invoke.
    initialize_registered_command(&command, name).ok()?;
    Some(command)
}

fn initialize_registered_command(command: &Com, name: &str) -> Result<bool, String> {
    let key = format!("{COMMAND_STORE}\\{name}");
    let generic = registry_string(HKEY_LOCAL_MACHINE, &key, "ActionId").is_some()
        || registry_string(HKEY_LOCAL_MACHINE, &key, "CommandStore").is_some();
    let Some(initializer) = command.query(&IID_INITIALIZE_COMMAND) else {
        if generic {
            return Err(
                "Generic CommandStore handler cannot be initialized; invocation is disabled".into(),
            );
        }
        return Ok(false);
    };
    let bag = registry_property_bag(&key)?;
    let verb: Vec<_> = name.encode_utf16().chain(Some(0)).collect();
    let hr = unsafe {
        (initializer.vtable::<InitializeCommandVtable>().initialize)(
            initializer.0,
            verb.as_ptr(),
            bag.0,
        )
    };
    if hr < 0 {
        return Err(failure("initialize registered compression command", hr));
    }
    Ok(true)
}

fn registry_property_bag(key: &str) -> Result<Com, String> {
    type CreateBag =
        unsafe extern "system" fn(HKEY, *const u16, u32, *const GUID, *mut *mut c_void) -> HRESULT;
    let module_name: Vec<_> = "shlwapi.dll".encode_utf16().chain(Some(0)).collect();
    let module = unsafe { GetModuleHandleW(module_name.as_ptr()) };
    if module.is_null() {
        return Err("Shell registry property-bag helper is unavailable".into());
    }
    // SHCreatePropertyBagOnRegKey is an optional Shell export (SHLWAPI.471),
    // absent from the public SDK bindings. Resolve it at runtime; absence is a
    // hard failure, never permission to Invoke a generic uninitialized object.
    let function =
        unsafe { GetProcAddress(module, c"SHCreatePropertyBagOnRegKey".as_ptr().cast()) }
            .or_else(|| unsafe { GetProcAddress(module, 471_usize as *const u8) })
            .ok_or("Shell registry property-bag helper is unavailable")?;
    let create: CreateBag = unsafe { mem::transmute(function) };
    let path: Vec<_> = key.encode_utf16().chain(Some(0)).collect();
    let mut pointer = ptr::null_mut();
    let hr = unsafe {
        create(
            HKEY_LOCAL_MACHINE,
            path.as_ptr(),
            0,
            &IID_PROPERTY_BAG,
            &mut pointer,
        )
    }; // STGM_READ only.
    Com::received(hr, pointer, "read-only registry command property bag")
}

fn dedicated_compression_class(name: &str) -> Option<GUID> {
    match name {
        "Windows.CompressTo.7Z" => Some(GUID::from_u128(0x21def7e5_832f_44d1_9109_e1161753b062)),
        "Windows.CompressTo.Tar" => Some(GUID::from_u128(0x3da1a190_b430_49f5_86bf_765d29f7b9f0)),
        "Windows.CompressTo.Wizard" => {
            Some(GUID::from_u128(0x812ee37b_5065_4c14_b0d1_a2266454b0c1))
        }
        _ => None,
    }
}

fn parse_guid(raw: &str) -> Option<GUID> {
    let value: Vec<_> = raw.encode_utf16().chain(Some(0)).collect();
    let mut class = GUID::from_u128(0);
    (unsafe { CLSIDFromString(value.as_ptr(), &mut class) } >= 0).then_some(class)
}
fn same_guid(left: &GUID, right: &GUID) -> bool {
    left.data1 == right.data1
        && left.data2 == right.data2
        && left.data3 == right.data3
        && left.data4 == right.data4
}
fn guid_text(value: &GUID) -> String {
    format!(
        "{{{:08X}-{:04X}-{:04X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}}}",
        value.data1,
        value.data2,
        value.data3,
        value.data4[0],
        value.data4[1],
        value.data4[2],
        value.data4[3],
        value.data4[4],
        value.data4[5],
        value.data4[6],
        value.data4[7]
    )
}
fn is_system_zip_dll(path: &str) -> bool {
    let mut buffer = [0u16; 512];
    let length = unsafe { GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    if length == 0 || length >= buffer.len() {
        return false;
    }
    let expected = format!(
        "{}\\zipfldr.dll",
        String::from_utf16_lossy(&buffer[..length])
    );
    path.trim_matches('"')
        .replace('/', "\\")
        .eq_ignore_ascii_case(&expected)
}
fn system_zip_server(class_key: &str) -> Result<String, String> {
    if registry_string(HKEY_CLASSES_ROOT, &format!("{class_key}\\TreatAs"), "").is_some() {
        return Err("Windows compression class has a redirected registration".into());
    }
    let server = registry_string(
        HKEY_CLASSES_ROOT,
        &format!("{class_key}\\InProcServer32"),
        "",
    )
    .filter(|server| is_system_zip_dll(server))
    .ok_or("Windows compression handler is not the system zipfldr.dll")?;
    Ok(server)
}
fn interface_module(address: usize) -> Option<String> {
    let mut module = ptr::null_mut();
    if unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            address as *const u16,
            &mut module,
        )
    } == 0
    {
        return None;
    }
    let mut buffer = [0u16; 2048];
    let length =
        unsafe { GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
    (length > 0 && length < buffer.len()).then(|| String::from_utf16_lossy(&buffer[..length]))
}
fn verified_zip_drop_target() -> Result<(Com, PathBuf), String> {
    let path = legacy_zip_target().ok_or("Windows ZIP SendTo target is unavailable")?;
    let association = registry_string(HKEY_CLASSES_ROOT, ".ZFSendToTarget", "")
        .filter(|value| value.eq_ignore_ascii_case(ZIP_SENDTO_CLASS_KEY))
        .ok_or("The ZIP SendTo association is not the Windows ZIP target")?;
    let drop_class = registry_string(
        HKEY_CLASSES_ROOT,
        &format!("{association}\\shellex\\DropHandler"),
        "",
    )
    .and_then(|raw| parse_guid(&raw))
    .filter(|class| same_guid(class, &ZIP_SENDTO_CLASS))
    .ok_or("The ZIP SendTo drop handler is not the Windows ZIP class")?;
    system_zip_server(ZIP_SENDTO_CLASS_KEY)?;
    let target = create_instance(
        &drop_class,
        &IID_DROP_TARGET,
        "verified Windows ZIP drop handler",
    )?;
    let module = interface_module(target.vtable::<DropTargetVtable>().drop as usize)
        .filter(|module| is_system_zip_dll(module))
        .ok_or("The ZIP Drop implementation is not in system zipfldr.dll")?;
    if let Some(persist) = target.query(&IID_PERSIST) {
        let mut class = GUID::from_u128(0);
        let hr = unsafe { (persist.vtable::<PersistFileVtable>().class_id)(persist.0, &mut class) };
        // IPersist describes the persistence/delegated object's class. It is
        // not an additional activation-identity contract for this SendTo
        // adapter. The requested registered CLSID and the actual Drop slot's
        // system zipfldr.dll module were independently verified above.
        if diagnostics_enabled() {
            let reported = if hr >= 0 {
                guid_text(&class)
            } else {
                "<unavailable>".into()
            };
            eprintln!(
                "foldex-native-menu ZIP IPersist: GetClassID_hr=0x{:08X} reported_class={reported} activation_class={} actual_Drop_module={module} persistence_identity_is_diagnostic_only=true Invoke_called=false",
                hr as u32,
                guid_text(&drop_class)
            );
        }
    } else if diagnostics_enabled() {
        eprintln!(
            "foldex-native-menu ZIP IPersist: unsupported; activation_class={} actual_Drop_module={module} Invoke_called=false",
            guid_text(&drop_class)
        );
    }
    let persist_file = target.query(&IID_PERSIST_FILE);
    if let Some(persist) = &persist_file {
        let name = parsing_name(&path);
        let hr =
            unsafe { (persist.vtable::<PersistFileVtable>().load)(persist.0, name.as_ptr(), 0) }; // STGM_READ
        if diagnostics_enabled() {
            eprintln!(
                "foldex-native-menu ZIP IPersistFile: Load_hr=0x{:08X} mode=STGM_READ target={} Invoke_called=false",
                hr as u32,
                path.display()
            );
        }
        if hr < 0 {
            return Err(failure("initialize Windows ZIP SendTo target", hr));
        }
    }
    if diagnostics_enabled() {
        eprintln!(
            "foldex-native-menu ZIP verified: registry_class={} Drop_module={module} IPersistFile={} default_open_resolution=false",
            guid_text(&drop_class),
            persist_file.is_some()
        );
    }
    Ok((target, path))
}
fn diagnostics_enabled() -> bool {
    std::env::var_os("FOLDEX_NATIVE_MENU_DIAGNOSTICS").is_some_and(|value| value == "1")
}
fn diagnose_compression_command(name: &str, command: &Com, selection: &Com, title: &str) {
    let mut canonical = GUID::from_u128(0);
    let hr =
        unsafe { (command.vtable::<ExplorerCommandVtable>().canonical)(command.0, &mut canonical) };
    let initializer = command.query(&IID_INITIALIZE_COMMAND).is_some();
    let mut state = u32::MAX;
    let state_hr = unsafe {
        (command.vtable::<ExplorerCommandVtable>().state)(command.0, selection.0, 0, &mut state)
    };
    eprintln!(
        "foldex-native-menu {name}: title={title:?} canonical_hr=0x{:08X} canonical={} GetState_hr=0x{:08X} state={state} IInitializeCommand={initializer} initialization_succeeded_or_not_required=true Invoke_module={} Invoke_called=false",
        hr as u32,
        guid_text(&canonical),
        state_hr as u32,
        interface_module(command.vtable::<ExplorerCommandVtable>().invoke as usize)
            .unwrap_or_default()
    );
}

fn diagnose_zip_wrapper(selection: &Com) {
    let name = "Windows.CompressTo.Zip";
    let raw = registry_string(
        HKEY_LOCAL_MACHINE,
        &format!("{COMMAND_STORE}\\{name}"),
        "ExplorerCommandHandler",
    );
    eprintln!(
        "foldex-native-menu ZIP generic wrapper registration={raw:?}; production_route=verified ZIP SendTo; Invoke_permitted=false"
    );
    let Some(class) = raw.as_deref().and_then(parse_guid) else {
        return;
    };
    let expected = GUID::from_u128(0x9c07355e_c50a_45d2_b4a3_0a8235f8047f);
    if !same_guid(&class, &expected) {
        return;
    }
    let Ok(command) = create_instance(
        &class,
        &IID_EXPLORER_COMMAND,
        "ZIP read-only wrapper diagnostic",
    ) else {
        return;
    };
    let initialized = initialize_registered_command(&command, name);
    eprintln!("foldex-native-menu ZIP generic Initialize={initialized:?}; Invoke_called=false");
    if initialized.is_err() {
        return;
    }
    let title = command_title(&command, selection).unwrap_or_else(|| "<no title>".into());
    diagnose_compression_command(name, &command, selection, &title);
}
fn shell_selection(paths: &[PathBuf]) -> Result<Com, String> {
    let pidls = paths
        .iter()
        .map(|path| Pidl::for_path(path))
        .collect::<Result<Vec<_>, _>>()?;
    let pointers: Vec<_> = pidls
        .iter()
        .map(|pidl| pidl.0 as *const ITEMIDLIST)
        .collect();
    let mut pointer = ptr::null_mut();
    let hr = unsafe {
        SHCreateShellItemArrayFromIDLists(pointers.len() as u32, pointers.as_ptr(), &mut pointer)
    };
    Com::received(hr, pointer, "native compression selection")
}
fn command_title(command: &Com, selection: &Com) -> Option<String> {
    let mut pointer = ptr::null_mut();
    let hr = unsafe {
        (command.vtable::<ExplorerCommandVtable>().title)(command.0, selection.0, &mut pointer)
    };
    if pointer.is_null() {
        return None;
    }
    let text = if hr >= 0 {
        let mut length = 0;
        while length < MAX_STRING_UNITS && unsafe { *pointer.add(length) } != 0 {
            length += 1;
        }
        let raw = unsafe { std::slice::from_raw_parts(pointer, length) };
        let label = split_label(&String::from_utf16_lossy(raw)).0;
        (!label.is_empty()).then_some(label)
    } else {
        None
    };
    unsafe {
        CoTaskMemFree(pointer.cast());
    }
    text
}
fn legacy_zip_target() -> Option<PathBuf> {
    let mut pointer = ptr::null_mut();
    let hr = unsafe { SHGetKnownFolderPath(&FOLDERID_SendTo, 0, ptr::null_mut(), &mut pointer) };
    if pointer.is_null() {
        return None;
    }
    let mut length = 0;
    while length < 32_768 && unsafe { *pointer.add(length) } != 0 {
        length += 1;
    }
    let folder = if hr >= 0 && length < 32_768 {
        Some(PathBuf::from(OsString::from_wide(unsafe {
            std::slice::from_raw_parts(pointer, length)
        })))
    } else {
        None
    };
    unsafe {
        CoTaskMemFree(pointer.cast());
    }
    // A real registered SendTo target is required on older Windows versions.
    // No ZIP implementation/extension is guessed or launched via a shell string.
    fs::read_dir(folder?)
        .ok()?
        .take(256)
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("ZFSendToTarget"))
        })
}

// A minimal IOleWindow site supplies the actual Foldex owner to native Shell
// commands. Site references use COM ref-counting and may outlive Invoke.
#[repr(C)]
struct WindowSite {
    vtable: *const WindowVtable,
    references: AtomicU32,
    owner: isize,
}
#[repr(C)]
struct WindowVtable {
    unknown: UnknownVtable,
    window: unsafe extern "system" fn(*mut c_void, *mut HWND) -> HRESULT,
    help: unsafe extern "system" fn(*mut c_void, i32) -> HRESULT,
}
unsafe extern "system" fn site_query(
    this: *mut c_void,
    iid: *const GUID,
    output: *mut *mut c_void,
) -> HRESULT {
    if output.is_null() || iid.is_null() {
        return 0x80004003_u32 as i32;
    }
    unsafe {
        *output = ptr::null_mut();
    }
    let requested = unsafe { &*iid };
    let equal = |known: &GUID| {
        requested.data1 == known.data1
            && requested.data2 == known.data2
            && requested.data3 == known.data3
            && requested.data4 == known.data4
    };
    if equal(&IID_UNKNOWN) || equal(&IID_OLE_WINDOW) {
        unsafe {
            *output = this;
            site_add(this);
        }
        0
    } else {
        0x80004002_u32 as i32
    }
}
unsafe extern "system" fn site_add(this: *mut c_void) -> u32 {
    unsafe {
        (*this.cast::<WindowSite>())
            .references
            .fetch_add(1, Ordering::Relaxed)
            + 1
    }
}
unsafe extern "system" fn site_release(this: *mut c_void) -> u32 {
    let remaining = unsafe {
        (*this.cast::<WindowSite>())
            .references
            .fetch_sub(1, Ordering::AcqRel)
            - 1
    };
    if remaining == 0 {
        unsafe {
            drop(Box::from_raw(this.cast::<WindowSite>()));
        }
    }
    remaining
}
unsafe extern "system" fn site_window(this: *mut c_void, output: *mut HWND) -> HRESULT {
    if output.is_null() {
        return 0x80004003_u32 as i32;
    }
    unsafe {
        *output = (*this.cast::<WindowSite>()).owner as HWND;
    }
    0
}
unsafe extern "system" fn site_help(_: *mut c_void, _: i32) -> HRESULT {
    0x80004001_u32 as i32
}
static WINDOW_VTABLE: WindowVtable = WindowVtable {
    unknown: UnknownVtable {
        query: site_query,
        add_ref: site_add,
        release: site_release,
    },
    window: site_window,
    help: site_help,
};
fn window_site(owner: isize) -> Option<Com> {
    (owner != 0).then(|| {
        Com(Box::into_raw(Box::new(WindowSite {
            vtable: &WINDOW_VTABLE,
            references: AtomicU32::new(1),
            owner,
        }))
        .cast())
    })
}
fn set_site(command: &Com, site: &Com) {
    if let Some(object) = command.query(&IID_OBJECT_WITH_SITE) {
        unsafe {
            (object.vtable::<ObjectWithSiteVtable>().set_site)(object.0, site.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn omitted_native_command_state_does_not_block_explicit_compression() {
        // A failed GetState has no valid output, even if the provider touched
        // it. Actual enabled/disabled/hidden states still control invocation.
        assert!(compression_state(E_NOTIMPL, u32::MAX).is_ok());
        assert!(compression_state(0, 0).is_ok());
        assert!(compression_state(0, 1).is_err());
        assert!(compression_state(0, 2).is_err());
        assert!(compression_state(0x80070005_u32 as i32, 0).is_err());
    }

    #[test]
    fn generic_dispatchers_are_excluded_and_zip_drop_requires_copy_acceptance() {
        assert!(dedicated_compression_class("Windows.CompressTo.Zip").is_none());
        assert!(dedicated_compression_class("open").is_none());
        assert!(dedicated_compression_class("Windows.CompressTo.7Z").is_some());
        assert!(accepted_zip_drop(0, 1).is_ok());
        assert!(accepted_zip_drop(0, 0).is_err());
        assert!(accepted_zip_drop(0, 2).is_err());
        assert!(accepted_zip_drop(0, 3).is_err());
        // DRAGDROP_S_CANCEL is positive; FAILED(hr) alone is insufficient.
        assert!(accepted_zip_drop(0x00040101, 1).is_err());
        assert!(accepted_zip_drop(0x800704c7_u32 as i32, 1).is_err());
    }

    #[test]
    fn new_selection_uses_containing_folder_and_rejects_mixed_parents() {
        let target = Target {
            owner: 0,
            paths: vec![
                PathBuf::from(r"C:\fixture\one.txt"),
                PathBuf::from(r"C:\fixture\subfolder"),
            ],
            background: false,
        };
        assert_eq!(directory(&target).unwrap(), PathBuf::from(r"C:\fixture"));
        let mut mixed = target;
        mixed.paths.push(PathBuf::from(r"C:\elsewhere\two.txt"));
        assert!(directory(&mixed).is_err());
    }

    #[test]
    fn native_system_definitions_are_read_only_and_only_offer_installed_formats() {
        let _guard = NATIVE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let root =
            std::env::temp_dir().join(format!("foldex-new-catalogue-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let target = Target {
            owner: 0,
            paths: vec![root.clone()],
            background: true,
        };
        let mut native = Native::new();
        let snapshot = native.discover(BuiltinKind::New, &target).unwrap();
        for key in ["new:folder", "new:shortcut", "new:text"] {
            assert!(
                snapshot.items.iter().any(|item| item.key == key),
                "missing {key}"
            );
        }
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        let selection = Target {
            background: false,
            ..target
        };
        let formats = native.discover(BuiltinKind::Compress, &selection).unwrap();
        for item in formats.items.iter() {
            if item.key == "compress:legacy-zip" {
                assert!(legacy_zip_target().is_some());
            } else {
                assert!(compression_command(item.key.strip_prefix("compress:").unwrap()).is_some());
            }
        }
        assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
        fs::remove_dir(root).unwrap();
    }

    #[test]
    fn text_fallback_preserves_existing_file_and_has_exact_txt_extension() {
        let root = std::env::temp_dir().join(format!("foldex-new-text-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let first = root.join("New Text Document.txt");
        fs::write(&first, b"keep").unwrap();
        create_text(&root).unwrap();
        assert_eq!(fs::read(&first).unwrap(), b"keep");
        assert_eq!(
            fs::read(root.join("New Text Document (2).txt")).unwrap(),
            b""
        );
        fs::remove_file(first).unwrap();
        fs::remove_file(root.join("New Text Document (2).txt")).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
