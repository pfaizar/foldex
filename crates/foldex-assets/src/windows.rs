//! Raw COM ABI follows the installed Windows SDK's ShObjIdl_core.h:
//! IShellItemImageFactory inherits IUnknown and adds GetImage(SIZE, SIIGBF,
//! HBITMAP*). No interface pointer or GDI handle leaves this worker module.

use super::native_icon::{self, NativeIcon};
use super::{AssetKey, AssetOptions, Backend, MAX_IMAGE_EDGE, RawPixels};
use super::{KnownFolderIcon, StockIcon};
use std::{ffi::c_void, mem, os::windows::ffi::OsStrExt, path::Path, ptr};
use windows_sys::{
    Win32::{
        Foundation::SIZE,
        Graphics::Gdi::{
            BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, CreateDIBSection,
            DIB_RGB_COLORS, DeleteDC, DeleteObject, GdiFlush, GetDIBits, GetObjectW, HBITMAP, HDC,
            HGDIOBJ, SelectObject,
        },
        Storage::FileSystem::{FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_NORMAL},
        System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize},
        UI::{
            Shell::{
                SHCreateItemFromParsingName, SHFILEINFOW, SHGFI_ICON, SHGFI_LARGEICON,
                SHGFI_SMALLICON, SHGFI_USEFILEATTRIBUTES, SHGetFileInfoW, SIIGBF_ICONONLY,
                SIIGBF_INCACHEONLY, SIIGBF_THUMBNAILONLY,
            },
            WindowsAndMessaging::{
                DI_NORMAL, DestroyIcon, DrawIconEx, GetIconInfo, HICON, ICONINFO,
            },
        },
    },
    core::{GUID, HRESULT},
};

pub(super) struct Native {
    apartment: Result<Apartment, String>,
    options: AssetOptions,
}

impl Native {
    pub(super) fn new(options: AssetOptions) -> Self {
        Self {
            apartment: Apartment::new(),
            options,
        }
    }
}

impl Backend for Native {
    fn load(&mut self, key: &AssetKey) -> Result<RawPixels, String> {
        if let Err(error) = &self.apartment {
            return Err(error.clone());
        }
        if let Some(source) = native_icon::decode(&key.path) {
            return native_icon_pixels(source, key.size);
        }
        let location = classify(&key.path);
        if location.shell_allowed {
            let factory = Factory::for_path(&key.path);
            if let Ok(factory) = factory {
                if key.thumbnail {
                    let flags = SIIGBF_THUMBNAILONLY
                        | if self.options.extract_local_thumbnails && !location.cache_only {
                            0
                        } else {
                            SIIGBF_INCACHEONLY
                        };
                    if let Ok(image) = factory.image(key.size, flags, true) {
                        return Ok(image);
                    }
                }
                // Cached per-instance icons first; extraction is allowed only
                // on this local worker. Never use ShellExecute or load an app.
                if let Ok(image) =
                    factory.image(key.size, SIIGBF_ICONONLY | SIIGBF_INCACHEONLY, false)
                {
                    return Ok(image);
                }
                if !location.cache_only
                    && let Ok(image) = factory.image(key.size, SIIGBF_ICONONLY, false)
                {
                    return Ok(image);
                }
            }
        }
        let directory = location.directory.ok_or_else(|| {
            "native asset skipped for a remote, virtual or unavailable path; retain the caller icon"
                .to_owned()
        })?;
        generic_icon(key, directory)
    }
}

struct Apartment;

impl Apartment {
    fn new() -> Result<Self, String> {
        // The worker is newly created and owns its apartment throughout. S_FALSE
        // also requires a balancing CoUninitialize, so all nonnegative results
        // are represented by this guard.
        let result = unsafe { CoInitializeEx(ptr::null(), COINIT_APARTMENTTHREADED as u32) };
        if result < 0 {
            Err(hresult("COM initialization", result))
        } else {
            Ok(Self)
        }
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        unsafe { CoUninitialize() };
    }
}

#[repr(C)]
struct ImageFactory {
    vtable: *const ImageFactoryVtable,
}

#[repr(C)]
struct ImageFactoryVtable {
    query_interface:
        unsafe extern "system" fn(*mut ImageFactory, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut ImageFactory) -> u32,
    release: unsafe extern "system" fn(*mut ImageFactory) -> u32,
    get_image: unsafe extern "system" fn(*mut ImageFactory, SIZE, i32, *mut HBITMAP) -> HRESULT,
}

const IID_IMAGE_FACTORY: GUID = GUID {
    data1: 0xbcc18b79,
    data2: 0xba16,
    data3: 0x442f,
    data4: [0x80, 0xc4, 0x8a, 0x59, 0xc3, 0x0c, 0x46, 0x3b],
};

struct Factory(*mut ImageFactory);

impl Factory {
    fn from_result(pointer: *mut c_void, result: HRESULT) -> Result<Self, String> {
        if pointer.is_null() {
            return Err(hresult("shell image binding", result));
        }
        let factory = Self(pointer.cast());
        if result < 0 {
            Err(hresult("shell image binding", result))
        } else {
            Ok(factory)
        }
    }

    fn for_pidl(bytes: &[u8]) -> Result<Self, String> {
        // Aligned storage for the SDK's ITEMIDLIST; bytes have already passed
        // strict item-length/terminator validation on the public constructor.
        let aligned: Vec<u16> = bytes
            .chunks(2)
            .map(|pair| u16::from_ne_bytes([pair[0], pair.get(1).copied().unwrap_or(0)]))
            .collect();
        let mut pointer = ptr::null_mut();
        let result = unsafe {
            windows_sys::Win32::UI::Shell::SHCreateItemFromIDList(
                aligned.as_ptr().cast(),
                &IID_IMAGE_FACTORY,
                &mut pointer,
            )
        };
        Self::from_result(pointer, result)
    }

    fn for_known_folder(folder: KnownFolderIcon) -> Result<Self, String> {
        use windows_sys::Win32::UI::Shell::*;
        // Home is a shell namespace item rather than a per-user physical folder.
        if folder == KnownFolderIcon::Home {
            return Self::for_path(Path::new("shell:Home"))
                .or_else(|_| Self::for_path(Path::new("::{f874310e-b6b7-47dc-bc84-b9e6b38f5903}")))
                .or_else(|_| {
                    Self::for_path(Path::new("::{679f85cb-0220-4080-b29b-5540cc05aab6}"))
                });
        }
        let id = match folder {
            KnownFolderIcon::Profile => &FOLDERID_Profile,
            KnownFolderIcon::Desktop => &FOLDERID_Desktop,
            KnownFolderIcon::Downloads => &FOLDERID_Downloads,
            KnownFolderIcon::Documents => &FOLDERID_Documents,
            KnownFolderIcon::Pictures => &FOLDERID_Pictures,
            KnownFolderIcon::Music => &FOLDERID_Music,
            KnownFolderIcon::Videos => &FOLDERID_Videos,
            KnownFolderIcon::Home => unreachable!(),
        };
        let mut pointer = ptr::null_mut();
        let result = unsafe {
            SHGetKnownFolderItem(id, 0, ptr::null_mut(), &IID_IMAGE_FACTORY, &mut pointer)
        };
        Self::from_result(pointer, result)
    }

    fn for_path(path: &Path) -> Result<Self, String> {
        let wide = shell_parsing_name(path);
        let mut pointer: *mut c_void = ptr::null_mut();
        let result = unsafe {
            SHCreateItemFromParsingName(
                wide.as_ptr(),
                ptr::null_mut(),
                &IID_IMAGE_FACTORY,
                &mut pointer,
            )
        };
        if pointer.is_null() {
            return Err(hresult("shell image binding", result));
        }
        let factory = Self(pointer.cast());
        if result < 0 {
            Err(hresult("shell image binding", result))
        } else {
            Ok(factory)
        }
    }

    fn image(&self, size: u16, flags: i32, thumbnail: bool) -> Result<RawPixels, String> {
        let mut handle: HBITMAP = ptr::null_mut();
        let result = unsafe {
            ((*(*self.0).vtable).get_image)(
                self.0,
                SIZE {
                    cx: i32::from(size),
                    cy: i32::from(size),
                },
                flags,
                &mut handle,
            )
        };
        // Even an inconsistent provider returning failure plus a bitmap does
        // not leak that returned GDI resource.
        let bitmap = Bitmap(handle);
        if result < 0 || bitmap.0.is_null() {
            return Err(hresult("shell image", result));
        }
        bitmap_pixels(&bitmap, usize::from(size), thumbnail)
    }
}

fn native_icon_pixels(source: NativeIcon, size: u16) -> Result<RawPixels, String> {
    let factory = match source {
        NativeIcon::Known(KnownFolderIcon::Home) => return home_icon(size),
        NativeIcon::Known(folder) => Factory::for_known_folder(folder),
        NativeIcon::ShellItem(bytes) => Factory::for_pidl(&bytes),
        NativeIcon::AppUserModelId(id) => {
            Factory::for_path(Path::new(&format!("shell:AppsFolder\\{id}")))
        }
        NativeIcon::Stock(icon) => return stock_icon(icon, size),
        NativeIcon::Window(window) => return window_icon(window, size),
        NativeIcon::FileType(extension) => {
            // The existing generic provider uses SHGFI_USEFILEATTRIBUTES and
            // only the extension. No original/backing path reaches classify,
            // IShellItemImageFactory, thumbnails or filesystem metadata.
            return generic_icon(
                &AssetKey {
                    path: format!("file.{extension}").into(),
                    size,
                    thumbnail: false,
                },
                false,
            );
        }
    }?;
    factory
        .image(size, SIIGBF_ICONONLY | SIIGBF_INCACHEONLY, false)
        .or_else(|_| factory.image(size, SIIGBF_ICONONLY, false))
}

const HOME_NAMES: [&str; 3] = [
    "shell:Home",
    "::{f874310e-b6b7-47dc-bc84-b9e6b38f5903}",
    "::{679f85cb-0220-4080-b29b-5540cc05aab6}",
];

/// Some virtual Home providers do not implement IShellItemImageFactory, or
/// bind successfully but reject GetImage. Ask the same namespace's icon handler
/// through its absolute PIDL before trying the next Windows Home alias.
fn home_icon(size: u16) -> Result<RawPixels, String> {
    let mut failures = Vec::with_capacity(HOME_NAMES.len());
    for name in HOME_NAMES {
        let factory = Factory::for_path(Path::new(name)).and_then(|factory| {
            factory
                .image(size, SIIGBF_ICONONLY | SIIGBF_INCACHEONLY, false)
                .or_else(|_| factory.image(size, SIIGBF_ICONONLY, false))
        });
        if let Ok(pixels) = factory {
            return Ok(pixels);
        }
        match namespace_icon(name, size) {
            Ok(pixels) => return Ok(pixels),
            Err(error) => failures.push(format!("{name}: {error}")),
        }
    }
    Err(format!(
        "Windows Home icon unavailable; {}",
        failures.join("; ")
    ))
}

struct ItemIdList(*mut windows_sys::Win32::UI::Shell::Common::ITEMIDLIST);
impl Drop for ItemIdList {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe {
                windows_sys::Win32::System::Com::CoTaskMemFree(self.0.cast());
            }
        }
    }
}

fn namespace_icon(name: &str, size: u16) -> Result<RawPixels, String> {
    use windows_sys::Win32::UI::Shell::{SHGFI_PIDL, SHParseDisplayName};
    let name = wide(Path::new(name));
    let mut raw = ptr::null_mut();
    // This runs only on the existing asset worker's COM apartment. No Shell
    // verb is invoked, and no resource DLL/index is hard-coded.
    let parsed =
        unsafe { SHParseDisplayName(name.as_ptr(), ptr::null_mut(), &mut raw, 0, ptr::null_mut()) };
    let pidl = ItemIdList(raw);
    if parsed < 0 || pidl.0.is_null() {
        return Err(hresult("Home namespace PIDL", parsed));
    }
    let mut information = SHFILEINFOW::default();
    let flags = SHGFI_PIDL
        | SHGFI_ICON
        | if size <= 24 {
            SHGFI_SMALLICON
        } else {
            SHGFI_LARGEICON
        };
    // With SHGFI_PIDL the nominal PCWSTR parameter is an absolute ITEMIDLIST.
    // USEFILEATTRIBUTES must not be combined with this flag.
    let found = unsafe {
        SHGetFileInfoW(
            pidl.0.cast(),
            0,
            &mut information,
            mem::size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    let icon = Icon(information.hIcon);
    if found == 0 || icon.0.is_null() {
        return Err(last_error("Home namespace icon"));
    }
    render_icon(&icon, usize::from(size))
}

fn stock_icon(source: StockIcon, size: u16) -> Result<RawPixels, String> {
    use windows_sys::Win32::UI::Shell::*;
    let id = match source {
        StockIcon::File => SIID_DOCNOASSOC,
        StockIcon::Folder => SIID_FOLDER,
        StockIcon::Drive => SIID_DRIVEFIXED,
        StockIcon::Application => SIID_APPLICATION,
        StockIcon::Network => SIID_MYNETWORK,
        StockIcon::NetworkDrive => SIID_DRIVENET,
        StockIcon::OfflineNetworkDrive => SIID_DRIVENETDISABLED,
        StockIcon::RecycleBin => SIID_RECYCLER,
        StockIcon::Computer => SIID_DESKTOPPC,
    };
    let mut info = SHSTOCKICONINFO {
        cbSize: mem::size_of::<SHSTOCKICONINFO>() as u32,
        ..Default::default()
    };
    let result = unsafe {
        SHGetStockIconInfo(
            id,
            SHGSI_ICON
                | SHGSI_SYSICONINDEX
                | if size <= 24 {
                    SHGSI_SMALLICON
                } else {
                    SHGSI_LARGEICON
                },
            &mut info,
        )
    };
    let icon = Icon(info.hIcon);
    if result < 0 || icon.0.is_null() {
        return Err(hresult("stock icon", result));
    }
    system_image_icon(info.iSysImageIndex, size)
        .or_else(|_| render_icon_at_native_extent(&icon, size))
}

// Windows SDK 10.0.26100.0 commoncontrols.h: IImageList inherits IUnknown;
// Add, ReplaceIcon, SetOverlayImage, Replace, AddMasked, Draw and Remove precede
// GetIcon (vtable slot 10). No image list or interface pointer leaves this worker.
#[repr(C)]
struct NativeImageList {
    vtable: *const NativeImageListVtable,
}
#[repr(C)]
struct NativeImageListVtable {
    query_interface:
        unsafe extern "system" fn(*mut NativeImageList, *const GUID, *mut *mut c_void) -> HRESULT,
    add_ref: unsafe extern "system" fn(*mut NativeImageList) -> u32,
    release: unsafe extern "system" fn(*mut NativeImageList) -> u32,
    before_get_icon: [usize; 7],
    get_icon: unsafe extern "system" fn(*mut NativeImageList, i32, u32, *mut HICON) -> HRESULT,
}
const IID_IMAGE_LIST: GUID = GUID {
    data1: 0x46eb5926,
    data2: 0x582e,
    data3: 0x4017,
    data4: [0x9f, 0xdf, 0xe8, 0x99, 0x8d, 0xaa, 0x09, 0x50],
};
struct ImageList(*mut NativeImageList);
impl Drop for ImageList {
    fn drop(&mut self) {
        unsafe { ((*(*self.0).vtable).release)(self.0) };
    }
}
fn image_list_icon(index: i32, list: u32) -> Result<Icon, String> {
    use windows_sys::Win32::UI::Shell::SHGetImageList;
    let mut pointer = ptr::null_mut();
    let result = unsafe { SHGetImageList(list as i32, &IID_IMAGE_LIST, &mut pointer) };
    if pointer.is_null() {
        return Err(hresult("system image list", result));
    }
    let list = ImageList(pointer.cast());
    if result < 0 {
        return Err(hresult("system image list", result));
    }
    let mut icon = ptr::null_mut();
    let result = unsafe { ((*(*list.0).vtable).get_icon)(list.0, index, 0, &mut icon) };
    let icon = Icon(icon);
    if result < 0 || icon.0.is_null() {
        return Err(hresult("system image list icon", result));
    }
    Ok(icon)
}
fn system_image_icon(index: i32, size: u16) -> Result<RawPixels, String> {
    use windows_sys::Win32::UI::Shell::{SHIL_EXTRALARGE, SHIL_JUMBO, SHIL_LARGE, SHIL_SMALL};
    let lists: &[u32] = if size > 48 {
        &[SHIL_JUMBO, SHIL_EXTRALARGE, SHIL_LARGE]
    } else if size > 32 {
        &[SHIL_EXTRALARGE, SHIL_LARGE]
    } else if size > 16 {
        &[SHIL_LARGE, SHIL_SMALL]
    } else {
        &[SHIL_SMALL]
    };
    for &list in lists {
        if let Ok(icon) = image_list_icon(index, list)
            && let Ok(pixels) = render_icon_at_native_extent(&icon, size)
        {
            return Ok(pixels);
        }
    }
    Err("The system image lists did not supply this icon".into())
}

fn icon_extent(icon: &Icon) -> Result<[usize; 2], String> {
    let mut info = ICONINFO::default();
    if unsafe { GetIconInfo(icon.0, &mut info) } == 0 {
        return Err(last_error("read native icon extent"));
    }
    let color = Bitmap(info.hbmColor);
    let mask = Bitmap(info.hbmMask);
    let monochrome = color.0.is_null();
    let bitmap = if monochrome { mask.0 } else { color.0 };
    let mut object = BITMAP::default();
    if unsafe {
        GetObjectW(
            bitmap,
            mem::size_of::<BITMAP>() as i32,
            (&mut object as *mut BITMAP).cast(),
        )
    } == 0
    {
        return Err(last_error("read native icon bitmap"));
    }
    let width = object.bmWidth.unsigned_abs() as usize;
    let height = object.bmHeight.unsigned_abs() as usize / if monochrome { 2 } else { 1 };
    if width == 0 || height == 0 {
        return Err("The native icon bitmap is empty".into());
    }
    Ok([width, height])
}
fn render_icon_at_native_extent(icon: &Icon, requested: u16) -> Result<RawPixels, String> {
    let extent = icon_extent(icon)?;
    // Missing jumbo artwork keeps its real pixel extent. Never manufacture a
    // 256px texture by stretching the small HICON returned by a fallback list.
    let size = usize::from(requested)
        .min(extent[0])
        .min(extent[1])
        .min(MAX_IMAGE_EDGE)
        .max(1);
    render_icon(icon, size)
}

fn window_icon(window: isize, size: u16) -> Result<RawPixels, String> {
    use windows_sys::Win32::UI::WindowsAndMessaging::*;
    let window = window as windows_sys::Win32::Foundation::HWND;
    if unsafe { IsWindow(window) } == 0 {
        return Err("window is no longer available".into());
    }
    let mut handle = 0usize;
    // A hung foreign app must not stall the only asset worker indefinitely.
    let received = unsafe {
        SendMessageTimeoutW(
            window,
            WM_GETICON,
            ICON_SMALL2 as usize,
            0,
            SMTO_ABORTIFHUNG | SMTO_BLOCK,
            30,
            &mut handle,
        )
    };
    if received == 0 || handle == 0 {
        handle = unsafe { GetClassLongPtrW(window, GCLP_HICONSM) } as usize;
    }
    if handle == 0 {
        handle = unsafe { GetClassLongPtrW(window, GCLP_HICON) } as usize;
    }
    if handle == 0 {
        return stock_icon(StockIcon::Application, size);
    }
    // WM_GETICON/class handles belong to the target. Only our CopyIcon is freed.
    let icon = Icon(unsafe { CopyIcon(handle as HICON) });
    if icon.0.is_null() {
        return Err(last_error("copy window icon"));
    }
    render_icon(&icon, usize::from(size))
}

impl Drop for Factory {
    fn drop(&mut self) {
        unsafe { ((*(*self.0).vtable).release)(self.0) };
    }
}

struct Bitmap(HBITMAP);

impl Drop for Bitmap {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { DeleteObject(self.0) };
        }
    }
}

struct Dc(HDC);

impl Dc {
    fn new() -> Result<Self, String> {
        let handle = unsafe { CreateCompatibleDC(ptr::null_mut()) };
        if handle.is_null() {
            Err(last_error("create memory DC"))
        } else {
            Ok(Self(handle))
        }
    }
}

impl Drop for Dc {
    fn drop(&mut self) {
        unsafe { DeleteDC(self.0) };
    }
}

struct Icon(HICON);

impl Drop for Icon {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { DestroyIcon(self.0) };
        }
    }
}

fn bitmap_info(width: usize, height: usize) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32),
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn bitmap_pixels(bitmap: &Bitmap, requested: usize, thumbnail: bool) -> Result<RawPixels, String> {
    let mut object = BITMAP::default();
    let result = unsafe {
        GetObjectW(
            bitmap.0,
            mem::size_of::<BITMAP>() as i32,
            (&mut object as *mut BITMAP).cast(),
        )
    };
    if result != mem::size_of::<BITMAP>() as i32 {
        return Err(last_error("read shell bitmap layout"));
    }
    let width = usize::try_from(object.bmWidth).unwrap_or(0);
    let height = object.bmHeight.unsigned_abs() as usize;
    if width == 0
        || height == 0
        || width > requested
        || height > requested
        || width > MAX_IMAGE_EDGE
        || height > MAX_IMAGE_EDGE
    {
        return Err("shell bitmap exceeded requested bounds".into());
    }
    let dc = Dc::new()?;
    let mut info = bitmap_info(width, height);
    let mut pixels = vec![0u8; width * height * 4];
    // Factory bitmaps are not selected into a DC. Top-down output eliminates a
    // second full raster copy and keeps row order consistent with egui.
    let lines = unsafe {
        GetDIBits(
            dc.0,
            bitmap.0,
            0,
            height as u32,
            pixels.as_mut_ptr().cast(),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    if lines != height as i32 {
        return Err(last_error("read shell bitmap pixels"));
    }
    let any_alpha = pixels.chunks_exact(4).any(|pixel| pixel[3] != 0);
    if !any_alpha && !thumbnail {
        // Legacy RGB icon bitmaps do not carry their mask. Use the HICON path
        // below, which reconstructs transparency instead of making black opaque.
        return Err("legacy shell icon requires mask rendering".into());
    }
    bgra_to_rgba(&mut pixels, !any_alpha);
    Ok(RawPixels {
        size: [width, height],
        rgba: pixels,
    })
}

/// Factory DIBs are RGB24/RGB32/PARGB32 (Windows SDK IDL). RGB thumbnails with
/// an unused zero alpha channel are opaque; PARGB channels are unpremultiplied.
fn bgra_to_rgba(pixels: &mut [u8], opaque: bool) {
    for pixel in pixels.chunks_exact_mut(4) {
        pixel.swap(0, 2);
        if opaque {
            pixel[3] = 255;
        } else if pixel[3] == 0 {
            pixel[..3].fill(0);
        } else if pixel[3] < 255 {
            let alpha = u32::from(pixel[3]);
            for channel in &mut pixel[..3] {
                *channel = ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
    }
}

struct Location {
    shell_allowed: bool,
    cache_only: bool,
    directory: Option<bool>,
}

fn classify(path: &Path) -> Location {
    let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
    if local_disk_suffix(&raw).is_none() {
        return Location {
            shell_allowed: false,
            cache_only: true,
            directory: None,
        };
    }
    match foldex_fs::capabilities::background_path(path) {
        Ok(caps) => Location {
            shell_allowed: true,
            cache_only: caps.recall(),
            directory: Some(caps.is_dir()),
        },
        Err(_) => Location {
            shell_allowed: false,
            cache_only: true,
            directory: None,
        },
    }
}

fn local_disk_suffix(raw: &[u16]) -> Option<&[u16]> {
    let local = if raw.starts_with(&[92, 92, 63, 92]) {
        &raw[4..]
    } else {
        raw
    };
    if local.len() >= 3
        && local[0] <= 127
        && (local[0] as u8).is_ascii_alphabetic()
        && local[1] == 58
        && matches!(local[2], 47 | 92)
    {
        Some(local)
    } else {
        None
    }
}

fn generic_icon(key: &AssetKey, directory: bool) -> Result<RawPixels, String> {
    // USEFILEATTRIBUTES deliberately passes a synthetic name: no network path,
    // user executable or placeholder is opened for the generic fallback.
    let name = if directory {
        "folder".into()
    } else if let Some(extension) = key.path.extension() {
        let mut name = std::ffi::OsString::from("file.");
        name.push(extension);
        name
    } else {
        "file".into()
    };
    let name = wide(Path::new(&name));
    let attributes = if directory {
        FILE_ATTRIBUTE_DIRECTORY
    } else {
        FILE_ATTRIBUTE_NORMAL
    };
    let mut information = SHFILEINFOW::default();
    let flags = SHGFI_ICON
        | windows_sys::Win32::UI::Shell::SHGFI_SYSICONINDEX
        | SHGFI_USEFILEATTRIBUTES
        | if key.size <= 24 {
            SHGFI_SMALLICON
        } else {
            SHGFI_LARGEICON
        };
    let result = unsafe {
        SHGetFileInfoW(
            name.as_ptr(),
            attributes,
            &mut information,
            mem::size_of::<SHFILEINFOW>() as u32,
            flags,
        )
    };
    let icon = Icon(information.hIcon);
    if result == 0 || icon.0.is_null() {
        return Err(last_error("read generic shell icon"));
    }
    system_image_icon(information.iIcon, key.size)
        .or_else(|_| render_icon_at_native_extent(&icon, key.size))
}

struct Canvas {
    dc: Dc,
    _bitmap: Bitmap,
    previous: HGDIOBJ,
    bits: *mut u8,
    bytes: usize,
}

impl Canvas {
    fn new(size: usize, white: bool) -> Result<Self, String> {
        let dc = Dc::new()?;
        let info = bitmap_info(size, size);
        let mut bits = ptr::null_mut();
        let bitmap = Bitmap(unsafe {
            CreateDIBSection(dc.0, &info, DIB_RGB_COLORS, &mut bits, ptr::null_mut(), 0)
        });
        if bitmap.0.is_null() || bits.is_null() {
            return Err(last_error("create icon canvas"));
        }
        let previous = unsafe { SelectObject(dc.0, bitmap.0) };
        if previous.is_null() || previous as isize == -1 {
            return Err(last_error("select icon canvas"));
        }
        let bytes = size * size * 4;
        let mut result = Self {
            dc,
            _bitmap: bitmap,
            previous,
            bits: bits.cast(),
            bytes,
        };
        for pixel in result.pixels_mut().chunks_exact_mut(4) {
            pixel[..3].fill(if white { 255 } else { 0 });
            pixel[3] = 0;
        }
        Ok(result)
    }

    fn pixels(&self) -> &[u8] {
        // DIB owns exactly bytes bytes and lives until the Canvas guard drops.
        unsafe { std::slice::from_raw_parts(self.bits, self.bytes) }
    }

    fn pixels_mut(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.bits, self.bytes) }
    }
}

impl Drop for Canvas {
    fn drop(&mut self) {
        // A selected bitmap cannot be deleted. Restore before either field's
        // destructor releases its DC/bitmap, including all early-return paths.
        unsafe { SelectObject(self.dc.0, self.previous) };
    }
}

fn render_icon(icon: &Icon, size: usize) -> Result<RawPixels, String> {
    let black = Canvas::new(size, false)?;
    let white = Canvas::new(size, true)?;
    for canvas in [&black, &white] {
        if unsafe {
            DrawIconEx(
                canvas.dc.0,
                0,
                0,
                icon.0,
                size as i32,
                size as i32,
                0,
                ptr::null_mut(),
                DI_NORMAL,
            )
        } == 0
        {
            return Err(last_error("draw shell icon"));
        }
    }
    // GDI may batch drawing operations; finish them before CPU access to the
    // DIB's shared memory (CreateDIBSection's synchronization contract).
    if unsafe { GdiFlush() } == 0 {
        return Err(last_error("finish shell icon draw"));
    }
    // Rendering against black and white recovers both legacy AND masks and
    // modern alpha without leaking a black fringe into a light UI theme.
    let mut rgba = Vec::with_capacity(size * size * 4);
    for (black, white) in black
        .pixels()
        .chunks_exact(4)
        .zip(white.pixels().chunks_exact(4))
    {
        let background = (0..3)
            .map(|channel| white[channel].saturating_sub(black[channel]))
            .max()
            .unwrap_or(255);
        let alpha = 255 - background;
        for channel in [2, 1, 0] {
            rgba.push(if alpha == 0 {
                0
            } else {
                ((u32::from(black[channel]) * 255 + u32::from(alpha) / 2) / u32::from(alpha))
                    .min(255) as u8
            });
        }
        rgba.push(alpha);
    }
    Ok(RawPixels {
        size: [size, size],
        rgba,
    })
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

fn shell_parsing_name(path: &Path) -> Vec<u16> {
    let raw: Vec<u16> = path.as_os_str().encode_wide().collect();
    // std::fs::canonicalize returns verbatim disk paths. The shell factory on
    // this Windows installation rejects that filesystem prefix with E_INVALIDARG.
    // Normalize only an already-classified local drive prefix for the Shell;
    // raw request/cache identities and filesystem attribute paths stay exact.
    let name = if raw.starts_with(&[92, 92, 63, 92]) && local_disk_suffix(&raw).is_some() {
        &raw[4..]
    } else {
        raw.as_slice()
    };
    name.iter().copied().chain(Some(0)).collect()
}

fn hresult(operation: &str, result: HRESULT) -> String {
    format!("{operation} failed (HRESULT 0x{:08x})", result as u32)
}

fn last_error(operation: &str) -> String {
    format!("{operation}: {}", std::io::Error::last_os_error())
}

#[cfg(test)]
pub(super) static NATIVE_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
pub(super) fn direct_thumbnail_for_test(path: &Path, size: u16) -> Result<RawPixels, String> {
    let _apartment = Apartment::new()?;
    let location = classify(path);
    if !location.shell_allowed || location.cache_only {
        return Err(format!(
            "generated local fixture was not shell-eligible: {path:?}"
        ));
    }
    Factory::for_path(path)?.image(size, SIIGBF_THUMBNAILONLY, true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn system_image_list_vtable_matches_the_installed_sdk_get_icon_slot() {
        assert_eq!(
            std::mem::offset_of!(NativeImageListVtable, get_icon),
            10 * mem::size_of::<usize>()
        );
    }

    #[test]
    fn jumbo_shell_icons_have_native_256px_source_bitmaps_and_release_gdi_handles() {
        use windows_sys::Win32::{
            System::Threading::{GR_GDIOBJECTS, GetCurrentProcess, GetGuiResources},
            UI::Shell::{
                SHGSI_SYSICONINDEX, SHGetStockIconInfo, SHIL_EXTRALARGE, SHIL_JUMBO,
                SHSTOCKICONINFO, SIID_DOCNOASSOC, SIID_FOLDER,
            },
        };
        let _test_guard = NATIVE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _apartment = Apartment::new().expect("COM");
        let mut indices = Vec::new();
        for stock in [SIID_FOLDER, SIID_DOCNOASSOC] {
            let mut info = SHSTOCKICONINFO {
                cbSize: mem::size_of::<SHSTOCKICONINFO>() as u32,
                ..Default::default()
            };
            let result = unsafe { SHGetStockIconInfo(stock, SHGSI_SYSICONINDEX, &mut info) };
            assert!(result >= 0, "read-only stock image index: {result:#x}");
            let icon =
                image_list_icon(info.iSysImageIndex, SHIL_JUMBO).expect("jumbo system image list");
            // Check the HICON source before DrawIconEx. A stretched 32px icon
            // would fail even if its destination canvas happened to be 256px.
            let extent = icon_extent(&icon).expect("native jumbo source bitmap");
            assert!(
                extent[0] >= 256 && extent[1] >= 256,
                "native source extent: {extent:?}"
            );
            let pixels = render_icon_at_native_extent(&icon, 256).expect("render jumbo source");
            assert_eq!(pixels.size, [256, 256]);
            assert!(pixels.rgba.chunks_exact(4).any(|pixel| pixel[3] > 0));
            let smaller = image_list_icon(info.iSysImageIndex, SHIL_EXTRALARGE)
                .expect("extra large image list");
            let smaller_extent = icon_extent(&smaller).unwrap();
            assert!(smaller_extent[0] >= 48 && smaller_extent[1] >= 48);
            drop(render_icon_at_native_extent(&smaller, 48).unwrap());
            indices.push(info.iSysImageIndex);
        }
        // Warm both fallback providers before taking the per-process baseline.
        drop(stock_icon(StockIcon::Folder, 256).unwrap());
        let key = AssetKey::file_type_icon("txt", 256).unwrap();
        drop(native_icon_pixels(native_icon::decode(&key.path).unwrap(), 256).unwrap());
        let baseline = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        for _ in 0..16 {
            for &index in &indices {
                drop(system_image_icon(index, 256).unwrap());
            }
            drop(stock_icon(StockIcon::Folder, 256).unwrap());
            drop(native_icon_pixels(native_icon::decode(&key.path).unwrap(), 256).unwrap());
        }
        let after = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        assert!(
            after <= baseline + 4,
            "native image-list/HICON/GDI handles baseline={baseline}, after={after}"
        );
    }

    #[test]
    fn bgra_preserves_color_and_unpremultiplies_transparency() {
        let mut pixels = [25, 50, 100, 128, 9, 8, 7, 0, 1, 2, 3, 255];
        bgra_to_rgba(&mut pixels, false);
        assert_eq!(pixels, [199, 100, 50, 128, 0, 0, 0, 0, 3, 2, 1, 255]);
        let mut rgb = [9, 20, 80, 0];
        bgra_to_rgba(&mut rgb, true);
        assert_eq!(rgb, [80, 20, 9, 255]);
    }

    #[test]
    fn unc_and_device_names_never_become_local_factory_paths() {
        for text in [
            r"\\server\share\image.jpg",
            r"\\?\UNC\server\share\image.jpg",
            r"\\.\C:\image.jpg",
            "shell:Downloads",
            r"C:relative.jpg",
        ] {
            let raw: Vec<_> = text.encode_utf16().collect();
            assert!(local_disk_suffix(&raw).is_none(), "{text}");
        }
        for text in [r"C:\image.jpg", r"\\?\C:\image.jpg", "d:/image.jpg"] {
            let raw: Vec<_> = text.encode_utf16().collect();
            assert!(local_disk_suffix(&raw).is_some(), "{text}");
        }
    }

    #[test]
    fn shell_binding_normalizes_verbatim_drive_prefix_without_lossy_filename_conversion() {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt};
        let mut raw: Vec<u16> = r"\\?\D:\Generated ".encode_utf16().collect();
        raw.extend([0xd800, 46, 98, 109, 112]);
        let path = Path::new(&OsString::from_wide(&raw)).to_path_buf();
        let shell_name = shell_parsing_name(&path);
        assert_eq!(&shell_name[..shell_name.len() - 1], &raw[4..]);
        assert_eq!(shell_name.last(), Some(&0));
        assert_eq!(path.as_os_str().encode_wide().collect::<Vec<_>>(), raw);
    }

    #[test]
    fn generic_icon_handles_return_to_baseline() {
        use windows_sys::Win32::System::Threading::{
            GR_GDIOBJECTS, GetCurrentProcess, GetGuiResources,
        };
        let _test_guard = NATIVE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _apartment = Apartment::new().expect("COM");
        let key = AssetKey {
            path: Path::new(r"C:\not-accessed.txt").to_path_buf(),
            size: 42,
            thumbnail: false,
        };
        drop(generic_icon(&key, false).expect("warm OS type cache"));
        let baseline = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        for _ in 0..64 {
            let image = generic_icon(&key, false).expect("generic icon");
            assert_eq!(image.size, [42, 42]);
            assert!(image.rgba.chunks_exact(4).any(|pixel| pixel[3] == 0));
            assert!(image.rgba.chunks_exact(4).any(|pixel| pixel[3] != 0));
        }
        let after = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        assert!(
            after <= baseline + 2,
            "GDI handles baseline={baseline}, after={after}"
        );
    }

    #[test]
    fn stock_icon_pixels_are_bounded_and_owned_gdi_handles_return_to_baseline() {
        use windows_sys::Win32::System::Threading::{
            GR_GDIOBJECTS, GetCurrentProcess, GetGuiResources,
        };
        let _test_guard = NATIVE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _apartment = Apartment::new().expect("COM");
        drop(stock_icon(StockIcon::Folder, 24).expect("warm stock icon"));
        let before = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        for icon in [
            StockIcon::File,
            StockIcon::Folder,
            StockIcon::Drive,
            StockIcon::Application,
        ] {
            for _ in 0..8 {
                let pixels = stock_icon(icon, 24).expect("stock icon");
                assert_eq!(pixels.size, [24, 24]);
                assert_eq!(pixels.rgba.len(), 24 * 24 * 4);
                assert!(pixels.rgba.chunks_exact(4).any(|pixel| pixel[3] != 0));
            }
        }
        let after = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        assert!(
            after <= before + 2,
            "stock icon handles before={before} after={after}"
        );
    }

    #[test]
    #[ignore = "opt-in FOLDEX_NATIVE_HOME_ICON_PROBE=1 plus existing FOLDEX_NATIVE_HOME_ICON_OUTPUT directory beneath results; exports actual RGBA"]
    fn native_home_icon_exports_shell_pixels() {
        use std::io::Write;
        use windows_sys::Win32::System::Threading::{
            GR_GDIOBJECTS, GetCurrentProcess, GetGuiResources,
        };
        assert_eq!(
            std::env::var("FOLDEX_NATIVE_HOME_ICON_PROBE").as_deref(),
            Ok("1")
        );
        let directory = std::path::PathBuf::from(
            std::env::var_os("FOLDEX_NATIVE_HOME_ICON_OUTPUT")
                .expect("explicit native icon output directory"),
        )
        .canonicalize()
        .expect("output directory already exists");
        let results = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .canonicalize()
            .unwrap();
        assert!(
            directory.starts_with(&results),
            "native pixel export must stay under workspace results"
        );
        let _test_guard = NATIVE_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let _apartment = Apartment::new().expect("COM");
        let home = home_icon(32).expect("actual native Home icon");
        // Exercise the fallback directly even on Windows versions that support
        // the image factory; both captures represent the Shell's own pixels.
        let (name, pidl) = HOME_NAMES
            .into_iter()
            .find_map(|name| namespace_icon(name, 32).ok().map(|pixels| (name, pixels)))
            .expect("absolute-PIDL Home icon handler");
        for image in [&home, &pidl] {
            assert!(
                image
                    .size
                    .iter()
                    .all(|edge| *edge > 0 && *edge <= MAX_IMAGE_EDGE)
            );
            assert_eq!(image.rgba.len(), image.size[0] * image.size[1] * 4);
            assert!(
                image.rgba.chunks_exact(4).any(|pixel| pixel[3] > 0),
                "nonempty native pixels"
            );
        }
        let before = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        for _ in 0..16 {
            drop(namespace_icon(name, 32).expect("repeat owned PIDL/HICON extraction"));
        }
        let after = unsafe { GetGuiResources(GetCurrentProcess(), GR_GDIOBJECTS) };
        assert!(
            after <= before + 4,
            "native Home handles before={before} after={after}"
        );
        let create = |file: &str, bytes: &[u8]| {
            std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(directory.join(file))
                .unwrap()
                .write_all(bytes)
                .unwrap();
        };
        create("home-icon.rgba", &home.rgba);
        create("home-pidl-icon.rgba", &pidl.rgba);
        let metadata = format!(
            "{{\"format\":\"straight sRGB RGBA8\",\"home\":{{\"file\":\"home-icon.rgba\",\"width\":{},\"height\":{}}},\"pidl_fallback\":{{\"file\":\"home-pidl-icon.rgba\",\"width\":{},\"height\":{},\"namespace\":\"{}\"}},\"gdi_before\":{},\"gdi_after\":{},\"input_sent\":false,\"passed\":true}}\n",
            home.size[0], home.size[1], pidl.size[0], pidl.size[1], name, before, after
        );
        create("home-icon.json", metadata.as_bytes());
        eprintln!("FOLDEX_NATIVE_HOME_ICON_PROBE {metadata}");
    }
}
