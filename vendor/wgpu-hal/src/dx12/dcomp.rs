use alloc::sync::Arc;
use core::{ffi, mem, ptr};

use once_cell::sync::Lazy;
use windows::{
    core::Interface as _,
    Win32::{
        Foundation::{HANDLE, HWND},
        Graphics::{Direct3D12, DirectComposition, Dxgi},
    },
};

use super::{borrow_interface_temporarily, Device, DynLib};

fn api<T>(
    result: windows::core::Result<T>,
    operation: &'static str,
) -> Result<T, crate::SurfaceError> {
    result.map_err(|error| {
        log::error!("{operation}: {error}");
        crate::SurfaceError::Other(operation)
    })
}

/// A solid pixel, presented only when its color changes. The visual scales the
/// solid backdrop once; the independent UI visual is never scaled or clipped.
struct Background {
    swap_chain: Dxgi::IDXGISwapChain3,
    device: Direct3D12::ID3D12Device,
    queue: Direct3D12::ID3D12CommandQueue,
    color: Option<u32>,
    _library: Arc<super::D3D12Lib>,
}

impl Background {
    unsafe fn new(
        device: &Device,
        factory: &super::DxgiFactory,
    ) -> Result<Self, crate::SurfaceError> {
        let desc = Dxgi::DXGI_SWAP_CHAIN_DESC1 {
            Width: 1,
            Height: 1,
            Format: Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: Dxgi::Common::DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            BufferUsage: Dxgi::DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            Scaling: Dxgi::DXGI_SCALING_STRETCH,
            SwapEffect: Dxgi::DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL,
            AlphaMode: Dxgi::Common::DXGI_ALPHA_MODE_IGNORE,
            ..Default::default()
        };
        let chain = api(
            unsafe { factory.CreateSwapChainForComposition(&device.present_queue, &desc, None) },
            "Create composition background swapchain",
        )?;
        Ok(Self {
            swap_chain: api(chain.cast(), "Query composition background swapchain")?,
            device: device.raw.clone(),
            queue: device.present_queue.clone(),
            color: None,
            _library: Arc::clone(&device.library),
        })
    }

    unsafe fn update(&mut self, color: u32) -> Result<(), crate::SurfaceError> {
        if self.color == Some(color) {
            return Ok(());
        }
        let allocator: Direct3D12::ID3D12CommandAllocator = api(
            unsafe {
                self.device
                    .CreateCommandAllocator(Direct3D12::D3D12_COMMAND_LIST_TYPE_DIRECT)
            },
            "Create composition background allocator",
        )?;
        let list: Direct3D12::ID3D12GraphicsCommandList = api(
            unsafe {
                self.device.CreateCommandList(
                    0,
                    Direct3D12::D3D12_COMMAND_LIST_TYPE_DIRECT,
                    &allocator,
                    None,
                )
            },
            "Create composition background command list",
        )?;
        let heap: Direct3D12::ID3D12DescriptorHeap = api(
            unsafe {
                self.device
                    .CreateDescriptorHeap(&Direct3D12::D3D12_DESCRIPTOR_HEAP_DESC {
                        Type: Direct3D12::D3D12_DESCRIPTOR_HEAP_TYPE_RTV,
                        NumDescriptors: 1,
                        ..Default::default()
                    })
            },
            "Create composition background RTV heap",
        )?;
        let buffer: Direct3D12::ID3D12Resource = api(
            unsafe {
                self.swap_chain
                    .GetBuffer(self.swap_chain.GetCurrentBackBufferIndex())
            },
            "Get composition background buffer",
        )?;
        let fence: Direct3D12::ID3D12Fence = api(
            unsafe {
                self.device
                    .CreateFence(0, Direct3D12::D3D12_FENCE_FLAG_NONE)
            },
            "Create composition background fence",
        )?;
        let rtv = unsafe { heap.GetCPUDescriptorHandleForHeapStart() };
        unsafe { self.device.CreateRenderTargetView(&buffer, None, rtv) };
        let transition = |before, after| Direct3D12::D3D12_RESOURCE_BARRIER {
            Type: Direct3D12::D3D12_RESOURCE_BARRIER_TYPE_TRANSITION,
            Flags: Direct3D12::D3D12_RESOURCE_BARRIER_FLAG_NONE,
            Anonymous: Direct3D12::D3D12_RESOURCE_BARRIER_0 {
                Transition: mem::ManuallyDrop::new(Direct3D12::D3D12_RESOURCE_TRANSITION_BARRIER {
                    pResource: unsafe { borrow_interface_temporarily(&buffer) },
                    Subresource: Direct3D12::D3D12_RESOURCE_BARRIER_ALL_SUBRESOURCES,
                    StateBefore: before,
                    StateAfter: after,
                }),
            },
        };
        let [r, g, b, _] = color.to_le_bytes();
        // This is an UNORM target, matching egui's gamma-valued clear color.
        let rgba = [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0];
        unsafe {
            list.ResourceBarrier(&[transition(
                Direct3D12::D3D12_RESOURCE_STATE_PRESENT,
                Direct3D12::D3D12_RESOURCE_STATE_RENDER_TARGET,
            )]);
            list.ClearRenderTargetView(rtv, &rgba, None);
            list.ResourceBarrier(&[transition(
                Direct3D12::D3D12_RESOURCE_STATE_RENDER_TARGET,
                Direct3D12::D3D12_RESOURCE_STATE_PRESENT,
            )]);
        }
        api(
            unsafe { list.Close() },
            "Close composition background command list",
        )?;
        let command: Direct3D12::ID3D12CommandList =
            api(list.cast(), "Query composition background command list")?;
        unsafe { self.queue.ExecuteCommandLists(&[Some(command)]) };

        // Complete the tiny upload before releasing its allocator/descriptors.
        // A null event makes SetEventOnCompletion a blocking GPU-fence wait;
        // this runs on creation/theme changes, never on ordinary resize frames.
        let completion = unsafe { self.queue.Signal(&fence, 1) }
            .and_then(|_| unsafe { fence.SetEventOnCompletion(1, HANDLE::default()) });
        if let Err(error) = completion {
            // On device loss work has stopped. If the live device could not
            // establish completion, quarantine these few objects instead of
            // freeing memory the GPU might still be reading; report the error.
            if unsafe { self.device.GetDeviceRemovedReason() }.is_ok() {
                mem::forget((
                    allocator,
                    list,
                    heap,
                    buffer,
                    fence,
                    self.queue.clone(),
                    Arc::clone(&self._library),
                ));
            } else {
                return Err(crate::SurfaceError::Device(crate::DeviceError::Lost));
            }
            return api(Err(error), "Wait for composition background upload");
        }
        api(
            unsafe { self.swap_chain.Present(0, Dxgi::DXGI_PRESENT::default()) }.ok(),
            "Present composition background",
        )?;
        self.color = Some(color);
        Ok(())
    }
}

// Lazy-loaded DirectComposition library
#[derive(Debug)]
pub(crate) struct DCompLib {
    lib: Lazy<Result<DynLib, crate::SurfaceError>>,
}

impl DCompLib {
    pub(crate) fn new() -> Self {
        Self {
            lib: Lazy::new(|| unsafe {
                DynLib::new("dcomp.dll").map_err(|err| {
                    log::error!("Error loading dcomp.dll: {err}");
                    crate::SurfaceError::Other("Error loading dcomp.dll")
                })
            }),
        }
    }

    fn get_lib(&self) -> Result<&DynLib, crate::SurfaceError> {
        match self.lib.as_ref() {
            Ok(lib) => Ok(lib),
            Err(err) => Err(err.clone()),
        }
    }

    pub(crate) fn create_device(
        &self,
    ) -> Result<DirectComposition::IDCompositionDevice, crate::SurfaceError> {
        let lib = self.get_lib()?;

        // Calls windows::Win32::Graphics::DirectComposition::DCompositionCreateDevice2 on dcomp.dll
        type Fun = extern "system" fn(
            pdxdevice: *mut ffi::c_void,
            riid: *const windows_core::GUID,
            ppdcompdevice: *mut *mut ffi::c_void,
        ) -> windows_core::HRESULT;
        let func: libloading::Symbol<Fun> =
            unsafe { lib.get(c"DCompositionCreateDevice2".to_bytes()) }?;

        let mut res: Option<DirectComposition::IDCompositionDevice> = None;

        (func)(
            ptr::null_mut(),
            &DirectComposition::IDCompositionDevice::IID,
            <*mut _>::cast(&mut res),
        )
        .map(|| res.unwrap())
        .map_err(|err| {
            log::error!("DirectComposition::DCompositionCreateDevice2 failed: {err}");
            crate::SurfaceError::Other("DirectComposition::DCompositionCreateDevice2")
        })
    }
}

#[derive(Default)]
pub struct DCompState {
    inner: Option<InnerState>,
}

impl DCompState {
    pub unsafe fn publish(&mut self, color: u32) -> Result<(), crate::SurfaceError> {
        if let Some(inner) = &mut self.inner {
            unsafe { inner.publish(color) }?;
        }
        Ok(())
    }

    /// This will create a DirectComposition device and a target for the window handle if not already initialized.
    /// If the device is already initialized, it will return the existing state.
    pub unsafe fn get_or_init(
        &mut self,
        lib: &Arc<DCompLib>,
        hwnd: &HWND,
    ) -> Result<&mut InnerState, crate::SurfaceError> {
        if self.inner.is_none() {
            self.inner = Some(unsafe { InnerState::init(lib, hwnd) }?);
        }
        Ok(self.inner.as_mut().unwrap())
    }
}

pub struct InnerState {
    pub visual: DirectComposition::IDCompositionVisual,
    pub device: DirectComposition::IDCompositionDevice,
    // Must be kept alive but is otherwise unused after initialization.
    pub _target: DirectComposition::IDCompositionTarget,
    _root: DirectComposition::IDCompositionVisual,
    background_visual: DirectComposition::IDCompositionVisual,
    background: Option<Background>,
    background_bound: bool,
    bound_content: Option<Dxgi::IDXGISwapChain3>,
    pending_content: Option<Dxgi::IDXGISwapChain3>,
    dirty: bool,
}

impl InnerState {
    /// Creates a DirectComposition device and a target for the given window handle.
    pub unsafe fn init(lib: &Arc<DCompLib>, hwnd: &HWND) -> Result<Self, crate::SurfaceError> {
        profiling::scope!("DCompState::init");
        let dcomp_device = lib.create_device()?;

        let target = unsafe { dcomp_device.CreateTargetForHwnd(*hwnd, false) }.map_err(|err| {
            log::error!("IDCompositionDevice::CreateTargetForHwnd failed: {err}");
            crate::SurfaceError::Other("IDCompositionDevice::CreateTargetForHwnd")
        })?;

        let visual = unsafe { dcomp_device.CreateVisual() }.map_err(|err| {
            log::error!("IDCompositionDevice::CreateVisual failed: {err}");
            crate::SurfaceError::Other("IDCompositionDevice::CreateVisual")
        })?;

        let root = api(
            unsafe { dcomp_device.CreateVisual() },
            "Create composition container",
        )?;
        let background_visual = api(
            unsafe { dcomp_device.CreateVisual() },
            "Create composition background visual",
        )?;
        let scale = api(
            unsafe { dcomp_device.CreateScaleTransform() },
            "Create composition background scale",
        )?;
        // Cover the entire Win32 WM_SIZE physical-pixel range. The HWND clips
        // this constant solid backdrop; no resize-dependent clip/scale commits.
        api(
            unsafe { scale.SetScaleX2(65536.0) },
            "Set composition background width",
        )?;
        api(
            unsafe { scale.SetScaleY2(65536.0) },
            "Set composition background height",
        )?;
        api(
            unsafe { background_visual.SetTransform(&scale) },
            "Set composition background transform",
        )?;
        api(
            unsafe {
                background_visual.SetBitmapInterpolationMode(
                    DirectComposition::DCOMPOSITION_BITMAP_INTERPOLATION_MODE_NEAREST_NEIGHBOR,
                )
            },
            "Set composition background point sampling",
        )?;
        api(
            unsafe {
                background_visual.SetBorderMode(DirectComposition::DCOMPOSITION_BORDER_MODE_HARD)
            },
            "Set composition background hard edges",
        )?;
        api(
            unsafe { root.AddVisual(&background_visual, false, None) },
            "Attach composition background",
        )?;
        api(
            unsafe { root.AddVisual(&visual, true, Some(&background_visual)) },
            "Attach identity composition content",
        )?;
        unsafe { target.SetRoot(&root) }.map_err(|err| {
            log::error!("IDCompositionTarget::SetRoot failed: {err}");
            crate::SurfaceError::Other("IDCompositionTarget::SetRoot")
        })?;

        Ok(InnerState {
            visual,
            device: dcomp_device,
            _target: target,
            _root: root,
            background_visual,
            background: None,
            background_bound: false,
            bound_content: None,
            pending_content: None,
            dirty: true,
        })
    }

    pub unsafe fn prepare(
        &mut self,
        gpu: &Device,
        factory: &super::DxgiFactory,
        content: &Dxgi::IDXGISwapChain3,
        opaque_background: bool,
    ) -> Result<(), crate::SurfaceError> {
        if opaque_background && self.background.is_none() {
            self.background = Some(unsafe { Background::new(gpu, factory) }?);
        } else if !opaque_background && self.background.is_some() {
            api(
                unsafe { self.background_visual.SetContent(None) },
                "Detach composition background",
            )?;
            self.background = None;
            self.background_bound = false;
            self.dirty = true;
        }
        if self
            .bound_content
            .as_ref()
            .is_none_or(|bound| bound.as_raw() != content.as_raw())
        {
            self.pending_content = Some(content.clone());
        }
        Ok(())
    }

    /// Publish only after the UI swapchain has presented a complete first frame.
    /// With no clip/transform changes this does not pretend to synchronize HWND
    /// geometry with DXGI Present; the persistent backdrop covers that interval.
    pub unsafe fn publish(&mut self, color: u32) -> Result<(), crate::SurfaceError> {
        if let Some(background) = &mut self.background {
            unsafe { background.update(color) }?;
            if !self.background_bound {
                api(
                    unsafe { self.background_visual.SetContent(&background.swap_chain) },
                    "Bind composition background",
                )?;
                self.background_bound = true;
                self.dirty = true;
            }
        }
        if let Some(content) = &self.pending_content {
            api(
                unsafe { self.visual.SetContent(content) },
                "Bind composition content",
            )?;
            self.dirty = true;
        }
        if self.dirty {
            api(unsafe { self.device.Commit() }, "Commit composition window")?;
            if let Some(content) = self.pending_content.take() {
                self.bound_content = Some(content);
            }
            self.dirty = false;
        }
        Ok(())
    }
}
