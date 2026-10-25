//! Shell drag images/descriptions and an input-transparent destination overlay.
//! The overlay paints independently of egui: SHDoDragDrop runs a modal loop
//! inside an egui frame, so requesting an egui repaint alone cannot highlight
//! a different row while dragging from this window.

use super::{Com, Destination, UnknownVtable, check, wide};
use std::{cell::RefCell, ffi::c_void, mem, path::Path, ptr};
use windows_sys::{
    Win32::{
        Foundation::{GlobalFree, HWND, POINT, POINTL, RECT, SIZE},
        Graphics::Gdi::*,
        System::{
            Com::{
                CLSCTX_INPROC_SERVER, CoCreateInstance, DVASPECT_CONTENT, FORMATETC, STGMEDIUM,
                STGMEDIUM_0, TYMED_HGLOBAL,
            },
            DataExchange::RegisterClipboardFormatW,
            Memory::{GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock},
            Ole::{DROPEFFECT_COPY, DROPEFFECT_LINK, DROPEFFECT_MOVE},
        },
        UI::{Shell::*, WindowsAndMessaging::*},
    },
    core::{GUID, PCWSTR, w},
};

const IID_SOURCE_HELPER: GUID = GUID::from_u128(0x83e07d0d_0c5f_4163_bf1a_60b274051e40);
const IID_TARGET_HELPER: GUID = GUID::from_u128(0x4657278b_411b_11d2_839a_00c04fd918d0);
const IID_IMAGE_FACTORY: GUID = GUID::from_u128(0xbcc18b79_ba16_442f_80c4_8a59c30c463b);

#[repr(C)]
struct SourceHelperVtable {
    unknown: UnknownVtable,
    bitmap: unsafe extern "system" fn(*mut c_void, *const SHDRAGIMAGE, *mut c_void) -> i32,
    window: usize,
    flags: unsafe extern "system" fn(*mut c_void, u32) -> i32,
}

#[repr(C)]
struct TargetHelperVtable {
    unknown: UnknownVtable,
    enter: unsafe extern "system" fn(*mut c_void, HWND, *mut c_void, *const POINT, u32) -> i32,
    leave: unsafe extern "system" fn(*mut c_void) -> i32,
    over: unsafe extern "system" fn(*mut c_void, *const POINT, u32) -> i32,
    drop: unsafe extern "system" fn(*mut c_void, *mut c_void, *const POINT, u32) -> i32,
    show: usize,
}

#[repr(C)]
struct ImageFactoryVtable {
    unknown: UnknownVtable,
    image: unsafe extern "system" fn(*mut c_void, SIZE, u32, *mut HBITMAP) -> i32,
}

#[repr(C)]
struct DataVtable {
    unknown: UnknownVtable,
    get_data: usize,
    get_data_here: usize,
    query_get_data: usize,
    canonical_format: usize,
    set_data:
        unsafe extern "system" fn(*mut c_void, *const FORMATETC, *const STGMEDIUM, i32) -> i32,
}

fn helper(iid: &GUID) -> Option<Com> {
    let mut output = ptr::null_mut();
    let result = unsafe {
        CoCreateInstance(
            &CLSID_DragDropHelper,
            ptr::null_mut(),
            CLSCTX_INPROC_SERVER,
            iid,
            &mut output,
        )
    };
    Com::received(result, output).ok()
}

// These are private formats on this drag's IDataObject, not writes to the
// user's clipboard. SetData(TRUE) owns the allocation only on success.
fn set_format<T: Copy>(data: &Com, name: PCWSTR, value: T) {
    let format = unsafe { RegisterClipboardFormatW(name) };
    if format == 0 {
        return;
    }
    let allocation = unsafe { GlobalAlloc(GMEM_MOVEABLE, mem::size_of::<T>()) };
    if allocation.is_null() {
        return;
    }
    let bytes = unsafe { GlobalLock(allocation) };
    if bytes.is_null() {
        unsafe { GlobalFree(allocation) };
        return;
    }
    unsafe {
        ptr::write_unaligned(bytes.cast::<T>(), value);
        GlobalUnlock(allocation);
    }
    let format = FORMATETC {
        cfFormat: format as u16,
        dwAspect: DVASPECT_CONTENT,
        lindex: -1,
        tymed: TYMED_HGLOBAL as u32,
        ..Default::default()
    };
    let medium = STGMEDIUM {
        tymed: TYMED_HGLOBAL as u32,
        u: STGMEDIUM_0 {
            hGlobal: allocation,
        },
        pUnkForRelease: ptr::null_mut(),
    };
    if unsafe { (data.vtable::<DataVtable>().set_data)(data.0, &format, &medium, 1) } < 0 {
        unsafe { GlobalFree(allocation) };
    }
}

fn text(value: &str) -> [u16; 260] {
    let mut output = [0; 260];
    let mut length = 0;
    for character in value.chars() {
        let mut units = [0; 2];
        let units = character.encode_utf16(&mut units);
        if length + units.len() >= output.len() {
            break;
        }
        output[length..length + units.len()].copy_from_slice(units);
        length += units.len();
    }
    output
}

pub(super) fn describe(data: &Com, path: Option<&Path>, effect: u32) {
    let (kind, message) = if path.is_none() || effect == 0 {
        (DROPIMAGE_NONE, "Can't drop here")
    } else if effect & DROPEFFECT_MOVE != 0 {
        (DROPIMAGE_MOVE, "Move to %1")
    } else if effect & DROPEFFECT_COPY != 0 {
        (DROPIMAGE_COPY, "Copy to %1")
    } else if effect & DROPEFFECT_LINK != 0 {
        (DROPIMAGE_LINK, "Create link in %1")
    } else {
        (DROPIMAGE_NONE, "Can't drop here")
    };
    let name = path.map(|path| {
        path.file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
    });
    set_format(
        data,
        CFSTR_DROPDESCRIPTION,
        DROPDESCRIPTION {
            r#type: kind,
            szMessage: text(message),
            szInsert: text(name.as_deref().unwrap_or_default()),
        },
    );
    set_format(data, w!("IsShowingText"), 1_i32);
}

pub(super) fn clear_description(data: &Com) {
    set_format(
        data,
        CFSTR_DROPDESCRIPTION,
        DROPDESCRIPTION {
            r#type: DROPIMAGE_INVALID,
            ..Default::default()
        },
    );
}

pub(super) struct Feedback {
    owner: HWND,
    helper: RefCell<Option<Com>>,
    highlight: RefCell<Highlight>,
}

impl Feedback {
    pub(super) fn new(owner: HWND) -> Self {
        Self {
            owner,
            helper: RefCell::new(
                (!owner.is_null())
                    .then(|| helper(&IID_TARGET_HELPER))
                    .flatten(),
            ),
            highlight: RefCell::new(Highlight::default()),
        }
    }

    pub(super) fn update(
        &self,
        data: &Com,
        point: POINTL,
        effect: u32,
        enter: bool,
        destination: Option<&Destination>,
        scale: f32,
    ) {
        let point = POINT {
            x: point.x,
            y: point.y,
        };
        // COM methods may pump messages. Do not keep a RefCell borrow alive
        // across them, including when a delegated Shell target changes.
        let helper = self.helper.take();
        if let Some(helper) = &helper {
            let vtable = helper.vtable::<TargetHelperVtable>();
            unsafe {
                if enter {
                    (vtable.enter)(helper.0, self.owner, data.0, &point, effect);
                } else {
                    (vtable.over)(helper.0, &point, effect);
                }
            }
        }
        self.helper.replace(helper);
        let mut highlight = self.highlight.take();
        highlight.update(self.owner, destination, scale);
        self.highlight.replace(highlight);
    }

    pub(super) fn finish(&self, data: *mut c_void, point: POINTL, effect: u32) {
        self.hide_highlight();
        let helper = self.helper.take();
        if let Some(helper) = &helper {
            let point = POINT {
                x: point.x,
                y: point.y,
            };
            unsafe { (helper.vtable::<TargetHelperVtable>().drop)(helper.0, data, &point, effect) };
        }
        self.helper.replace(helper);
    }

    pub(super) fn leave(&self) {
        self.hide_highlight();
        let helper = self.helper.take();
        if let Some(helper) = &helper {
            unsafe { (helper.vtable::<TargetHelperVtable>().leave)(helper.0) };
        }
        self.helper.replace(helper);
    }

    pub(super) fn hide_highlight(&self) {
        let mut highlight = self.highlight.take();
        highlight.hide();
        self.highlight.replace(highlight);
    }

    pub(super) fn shutdown(&self) {
        self.leave();
        self.helper.take();
        self.highlight.take();
    }
}

pub(super) fn source_image(data: &Com, paths: &[std::path::PathBuf], scale: f32) -> Option<Com> {
    let helper = helper(&IID_SOURCE_HELPER)?;
    let vtable = helper.vtable::<SourceHelperVtable>();
    check(unsafe { (vtable.flags)(helper.0, DSH_ALLOWDROPDESCRIPTIONTEXT as u32) }).ok()?;
    let scale = scale.clamp(0.75, 4.0);
    let mut bitmap = preview(paths, scale)?;
    let image = SHDRAGIMAGE {
        sizeDragImage: SIZE {
            cx: bitmap.width,
            cy: bitmap.height,
        },
        ptOffset: POINT {
            x: (bitmap.width as f32 * 0.65) as i32,
            y: bitmap.height - (16.0 * scale) as i32,
        },
        hbmpDragImage: bitmap.handle,
        crColorKey: u32::MAX,
    };
    set_format(data, CFSTR_INDRAGLOOP, 1_i32);
    check(unsafe { (vtable.bitmap)(helper.0, &image, data.0) }).ok()?;
    // InitializeFromBitmap takes ownership on success. Its pixels use straight
    // alpha; the Shell performs premultiplication itself.
    bitmap.handle = ptr::null_mut();
    set_format(data, w!("IsShowingText"), 1_i32);
    set_format(data, w!("UsingDefaultDragImage"), 0_i32);
    Some(helper)
}

struct Bitmap {
    handle: HBITMAP,
    bits: *mut u8,
    width: i32,
    height: i32,
}

impl Bitmap {
    fn new(width: i32, height: i32) -> Option<Self> {
        if width <= 0 || height <= 0 || i64::from(width) * i64::from(height) > 16_777_216 {
            return None;
        }
        let info = bitmap_info(width, height);
        let mut bits = ptr::null_mut();
        let handle = unsafe {
            CreateDIBSection(
                ptr::null_mut(),
                &info,
                DIB_RGB_COLORS,
                &mut bits,
                ptr::null_mut(),
                0,
            )
        };
        if handle.is_null() || bits.is_null() {
            if !handle.is_null() {
                unsafe { DeleteObject(handle) };
            }
            return None;
        }
        let bitmap = Self {
            handle,
            bits: bits.cast(),
            width,
            height,
        };
        unsafe { ptr::write_bytes(bitmap.bits, 0, width as usize * height as usize * 4) };
        Some(bitmap)
    }

    fn pixels(&mut self) -> &mut [u8] {
        unsafe {
            std::slice::from_raw_parts_mut(
                self.bits,
                self.width as usize * self.height as usize * 4,
            )
        }
    }

    fn blend(&mut self, x: i32, y: i32, rgba: [u8; 4]) {
        if x < 0 || y < 0 || x >= self.width || y >= self.height || rgba[3] == 0 {
            return;
        }
        let offset = (y as usize * self.width as usize + x as usize) * 4;
        let pixel = &mut self.pixels()[offset..offset + 4];
        let alpha = u32::from(rgba[3]);
        let old_alpha = u32::from(pixel[3]);
        let result_alpha = alpha + old_alpha * (255 - alpha) / 255;
        for (channel, color) in pixel[..3].iter_mut().zip([rgba[2], rgba[1], rgba[0]]) {
            *channel = ((u32::from(color) * alpha
                + u32::from(*channel) * old_alpha * (255 - alpha) / 255)
                / result_alpha) as u8;
        }
        pixel[3] = result_alpha as u8;
    }

    fn rounded(&mut self, bounds: [f32; 4], radius: f32, color: [u8; 4]) {
        let [left, top, right, bottom] = bounds;
        for y in top.floor().max(0.0) as i32..bottom.ceil().min(self.height as f32) as i32 {
            for x in left.floor().max(0.0) as i32..right.ceil().min(self.width as f32) as i32 {
                let center_x = (x as f32 + 0.5).clamp(left + radius, right - radius);
                let center_y = (y as f32 + 0.5).clamp(top + radius, bottom - radius);
                let distance = (x as f32 + 0.5 - center_x).hypot(y as f32 + 0.5 - center_y);
                let coverage = (radius + 0.5 - distance).clamp(0.0, 1.0);
                self.blend(
                    x,
                    y,
                    [
                        color[0],
                        color[1],
                        color[2],
                        (color[3] as f32 * coverage) as u8,
                    ],
                );
            }
        }
    }
}

impl Drop for Bitmap {
    fn drop(&mut self) {
        if !self.handle.is_null() {
            unsafe { DeleteObject(self.handle) };
        }
    }
}

fn bitmap_info(width: i32, height: i32) -> BITMAPINFO {
    BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn preview(paths: &[std::path::PathBuf], scale: f32) -> Option<Bitmap> {
    let side = (112.0 * scale).ceil() as i32;
    let mut output = Bitmap::new(side, side)?;
    for index in (0..paths.len().min(3)).rev() {
        let offset = index as f32 * 4.0 * scale;
        let bounds = [
            12.0 * scale + offset,
            8.0 * scale + offset,
            96.0 * scale + offset,
            92.0 * scale + offset,
        ];
        for spread in (1..=7).rev() {
            let spread = spread as f32 * scale;
            output.rounded(
                [
                    bounds[0] - spread,
                    bounds[1] - spread + 2.0 * scale,
                    bounds[2] + spread,
                    bounds[3] + spread + 2.0 * scale,
                ],
                4.0 * scale + spread,
                [0, 0, 0, 8],
            );
        }
        output.rounded(bounds, 3.0 * scale, [168, 174, 182, 235]);
        output.rounded(
            [
                bounds[0] + scale,
                bounds[1] + scale,
                bounds[2] - scale,
                bounds[3] - scale,
            ],
            2.0 * scale,
            [250, 250, 250, 245],
        );
        if let Some((width, height, pixels)) = shell_image(&paths[index], (68.0 * scale) as i32) {
            let size = 68.0 * scale;
            let ratio = (size / width as f32).min(size / height as f32).min(1.0);
            let drawn_width = (width as f32 * ratio).round().max(1.0) as i32;
            let drawn_height = (height as f32 * ratio).round().max(1.0) as i32;
            let left = ((bounds[0] + bounds[2] - drawn_width as f32) * 0.5) as i32;
            let top = ((bounds[1] + bounds[3] - drawn_height as f32) * 0.5) as i32;
            for y in 0..drawn_height {
                for x in 0..drawn_width {
                    let source_x = (x as f32 / ratio) as usize;
                    let source_y = (y as f32 / ratio) as usize;
                    let offset = (source_y.min(height as usize - 1) * width as usize
                        + source_x.min(width as usize - 1))
                        * 4;
                    let pixel = &pixels[offset..offset + 4];
                    let alpha = u32::from(pixel[3]);
                    // Shell cache bitmaps contain premultiplied BGRA pixels.
                    let channel = |value: u8| {
                        if alpha == 0 {
                            0
                        } else {
                            (u32::from(value) * 255 / alpha).min(255) as u8
                        }
                    };
                    output.blend(
                        left + x,
                        top + y,
                        [
                            channel(pixel[2]),
                            channel(pixel[1]),
                            channel(pixel[0]),
                            pixel[3],
                        ],
                    );
                }
            }
        } else {
            // A cold Shell cache still gets a recognizable file preview without
            // starting a thumbnail extractor or reading the dragged file.
            let x = (bounds[0] + bounds[2]) * 0.5;
            output.rounded(
                [
                    x - 20.0 * scale,
                    bounds[1] + 14.0 * scale,
                    x + 20.0 * scale,
                    bounds[3] - 14.0 * scale,
                ],
                2.0 * scale,
                [218, 228, 239, 255],
            );
            for row in 0..3 {
                let y = bounds[1] + (36.0 + row as f32 * 8.0) * scale;
                output.rounded(
                    [x - 12.0 * scale, y, x + 12.0 * scale, y + 2.0 * scale],
                    scale,
                    [119, 144, 169, 255],
                );
            }
        }
    }
    if paths.len() > 1 {
        count_badge(&mut output, paths.len(), scale);
    }
    Some(output)
}

fn shell_image(path: &Path, side: i32) -> Option<(i32, i32, Vec<u8>)> {
    let name = wide(path).ok()?;
    let mut pointer = ptr::null_mut();
    let result = unsafe {
        SHCreateItemFromParsingName(
            name.as_ptr(),
            ptr::null_mut(),
            &IID_IMAGE_FACTORY,
            &mut pointer,
        )
    };
    let factory = Com::received(result, pointer).ok()?;
    let mut handle = ptr::null_mut();
    let result = unsafe {
        (factory.vtable::<ImageFactoryVtable>().image)(
            factory.0,
            SIZE { cx: side, cy: side },
            SIIGBF_MEMORYONLY as u32,
            &mut handle,
        )
    };
    if handle.is_null() {
        return None;
    }
    // Shell image handles always belong to the caller, including error paths.
    let bitmap = Bitmap {
        handle,
        bits: ptr::null_mut(),
        width: 0,
        height: 0,
    };
    check(result).ok()?;
    let mut shape = BITMAP::default();
    if unsafe {
        GetObjectW(
            bitmap.handle,
            mem::size_of::<BITMAP>() as i32,
            (&mut shape as *mut BITMAP).cast(),
        )
    } == 0
        || shape.bmWidth <= 0
        || shape.bmHeight <= 0
        || shape.bmWidth > 512
        || shape.bmHeight > 512
    {
        return None;
    }
    let mut info = bitmap_info(shape.bmWidth, shape.bmHeight);
    let mut pixels = vec![0; shape.bmWidth as usize * shape.bmHeight as usize * 4];
    let dc = unsafe { CreateCompatibleDC(ptr::null_mut()) };
    if dc.is_null() {
        return None;
    }
    let lines = unsafe {
        GetDIBits(
            dc,
            bitmap.handle,
            0,
            shape.bmHeight as u32,
            pixels.as_mut_ptr().cast(),
            &mut info,
            DIB_RGB_COLORS,
        )
    };
    unsafe { DeleteDC(dc) };
    if lines != shape.bmHeight {
        return None;
    }
    if pixels.chunks_exact(4).all(|pixel| pixel[3] == 0) {
        for pixel in pixels.chunks_exact_mut(4) {
            pixel[3] = 255;
        }
    }
    Some((shape.bmWidth, shape.bmHeight, pixels))
}

fn count_badge(bitmap: &mut Bitmap, count: usize, scale: f32) {
    let label = if count > 999 {
        "999+".to_owned()
    } else {
        count.to_string()
    };
    let width = ((label.len() as f32 * 8.0 + 12.0) * scale).round() as i32;
    let height = (22.0 * scale).round() as i32;
    let left = bitmap.width - width - (4.0 * scale) as i32;
    let top = bitmap.height - height - (7.0 * scale) as i32;
    bitmap.rounded(
        [
            left as f32,
            top as f32,
            (left + width) as f32,
            (top + height) as f32,
        ],
        4.0 * scale,
        [0, 103, 192, 255],
    );
    let dc = unsafe { CreateCompatibleDC(ptr::null_mut()) };
    if dc.is_null() {
        return;
    }
    unsafe {
        let old_bitmap = SelectObject(dc, bitmap.handle);
        let font = CreateFontW(
            -(14.0 * scale) as i32,
            0,
            0,
            0,
            FW_SEMIBOLD as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            0,
            0,
            NONANTIALIASED_QUALITY as u32,
            0,
            w!("Segoe UI"),
        );
        let old_font = if font.is_null() {
            ptr::null_mut()
        } else {
            SelectObject(dc, font)
        };
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, 0x00ffffff);
        let label: Vec<_> = label.encode_utf16().collect();
        let mut rect = RECT {
            left,
            top,
            right: left + width,
            bottom: top + height,
        };
        DrawTextW(
            dc,
            label.as_ptr(),
            label.len() as i32,
            &mut rect,
            DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_NOPREFIX,
        );
        GdiFlush();
        if !old_font.is_null() {
            SelectObject(dc, old_font);
        }
        if !font.is_null() {
            DeleteObject(font);
        }
        SelectObject(dc, old_bitmap);
        DeleteDC(dc);
    }
    // GDI text clears alpha. Restore it only inside the badge's solid center,
    // leaving its transparent rounded corners untouched.
    for y in top..top + height {
        for x in left..left + width {
            let radius = 4.0 * scale;
            let center_x =
                (x as f32 + 0.5).clamp(left as f32 + radius, (left + width) as f32 - radius);
            let center_y =
                (y as f32 + 0.5).clamp(top as f32 + radius, (top + height) as f32 - radius);
            if (x as f32 + 0.5 - center_x).hypot(y as f32 + 0.5 - center_y) <= radius {
                let offset = (y as usize * bitmap.width as usize + x as usize) * 4;
                bitmap.pixels()[offset + 3] = 255;
            }
        }
    }
}

#[derive(Default)]
struct Highlight {
    window: HWND,
    current: Option<HighlightKey>,
}

#[derive(PartialEq, Eq)]
struct HighlightKey {
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    stroke: i32,
    accent: [u8; 3],
    pane: bool,
}

impl Highlight {
    fn hide(&mut self) {
        self.current = None;
        if !self.window.is_null() {
            unsafe { ShowWindow(self.window, SW_HIDE) };
        }
    }

    fn update(&mut self, owner: HWND, destination: Option<&Destination>, scale: f32) {
        let Some(destination) = destination else {
            self.hide();
            return;
        };
        if owner.is_null() || !scale.is_finite() || scale <= 0.0 {
            self.hide();
            return;
        }
        let rect = destination.rect;
        let mut origin = POINT {
            x: (rect.left() * scale).floor() as i32,
            y: (rect.top() * scale).floor() as i32,
        };
        let width = (rect.width() * scale).ceil() as i32;
        let height = (rect.height() * scale).ceil() as i32;
        if unsafe { ClientToScreen(owner, &mut origin) } == 0 {
            self.hide();
            return;
        }
        let key = HighlightKey {
            left: origin.x,
            top: origin.y,
            width,
            height,
            stroke: scale.round().max(1.0) as i32,
            accent: destination.accent,
            pane: destination.pane,
        };
        if self.current.as_ref() == Some(&key) {
            return;
        }
        let Some(mut bitmap) = Bitmap::new(width, height) else {
            self.hide();
            return;
        };
        for y in 0..height {
            for x in 0..width {
                let border = x < key.stroke
                    || y < key.stroke
                    || x >= width - key.stroke
                    || y >= height - key.stroke;
                let alpha = if border {
                    230_u32
                } else if key.pane {
                    0
                } else {
                    54
                };
                let offset = (y as usize * width as usize + x as usize) * 4;
                bitmap.pixels()[offset..offset + 4].copy_from_slice(&[
                    (u32::from(key.accent[2]) * alpha / 255) as u8,
                    (u32::from(key.accent[1]) * alpha / 255) as u8,
                    (u32::from(key.accent[0]) * alpha / 255) as u8,
                    alpha as u8,
                ]);
            }
        }
        if self.window.is_null() {
            // Owned rather than topmost: this stays above Foldex, never other
            // apps. Layered + transparent makes mouse and OLE hit tests pass
            // through to the real file destination; it cannot take focus.
            self.window = unsafe {
                CreateWindowExW(
                    WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW,
                    w!("STATIC"),
                    ptr::null(),
                    WS_POPUP,
                    0,
                    0,
                    0,
                    0,
                    owner,
                    ptr::null_mut(),
                    ptr::null_mut(),
                    ptr::null(),
                )
            };
        }
        if self.window.is_null() {
            return;
        }
        let dc = unsafe { CreateCompatibleDC(ptr::null_mut()) };
        if dc.is_null() {
            self.hide();
            return;
        }
        let success = unsafe {
            let previous = SelectObject(dc, bitmap.handle);
            let size = SIZE {
                cx: width,
                cy: height,
            };
            let source = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let result = UpdateLayeredWindow(
                self.window,
                ptr::null_mut(),
                &origin,
                &size,
                dc,
                &source,
                0,
                &blend,
                ULW_ALPHA,
            );
            SelectObject(dc, previous);
            DeleteDC(dc);
            result != 0
        };
        if success {
            unsafe { ShowWindow(self.window, SW_SHOWNOACTIVATE) };
            self.current = Some(key);
        } else {
            self.hide();
        }
    }
}

impl Drop for Highlight {
    fn drop(&mut self) {
        if !self.window.is_null() {
            unsafe { DestroyWindow(self.window) };
        }
    }
}
