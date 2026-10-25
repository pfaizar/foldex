//! No-window GPU/context isolation using the app's exact renderer configuration.
use std::{
    error::Error,
    ffi::OsString,
    future::Future,
    sync::Arc,
    task::{Context, Poll, Wake, Waker},
    time::Instant,
};

struct ThreadWake(std::thread::Thread);
impl Wake for ThreadWake {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}
fn block_on<T>(future: impl Future<Output = T>) -> T {
    let waker = Waker::from(Arc::new(ThreadWake(std::thread::current())));
    let mut context = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut context) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::park(),
        }
    }
}

#[cfg(windows)]
fn memory() -> serde_json::Value {
    use windows_sys::Win32::System::{
        ProcessStatus::{
            GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX2,
        },
        Threading::GetCurrentProcess,
    };
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX2>() as u32;
    let mut counters = PROCESS_MEMORY_COUNTERS_EX2 {
        cb: size,
        ..Default::default()
    };
    // SAFETY: the pseudo-handle is the current process and the correctly sized
    // output buffer is valid for this synchronous, read-only accounting call.
    if unsafe {
        GetProcessMemoryInfo(
            GetCurrentProcess(),
            (&mut counters as *mut PROCESS_MEMORY_COUNTERS_EX2).cast::<PROCESS_MEMORY_COUNTERS>(),
            size,
        )
    } == 0
    {
        return serde_json::json!({"available":false});
    }
    serde_json::json!({"private_commit_bytes":counters.PrivateUsage,"private_working_set_bytes":counters.PrivateWorkingSetSize,"working_set_bytes":counters.WorkingSetSize,"shared_commit_bytes":counters.SharedCommitUsage})
}
#[cfg(not(windows))]
fn memory() -> serde_json::Value {
    serde_json::json!({"available":false})
}

pub(super) fn run(args: &[OsString], launched: Instant) -> Result<(), Box<dyn Error>> {
    let mut samples = Vec::new();
    let sample = |samples: &mut Vec<serde_json::Value>, label: &str| {
        samples.push(serde_json::json!({"stage":label,"elapsed_ms":launched.elapsed().as_secs_f64()*1000.0,"memory":memory()}));
    };
    sample(&mut samples, "entry");
    let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) =
        eframe::egui_wgpu::WgpuSetup::without_display_handle()
    else {
        unreachable!()
    };
    let descriptor = setup.device_descriptor.clone();
    let preference = setup.power_preference;
    #[cfg(windows)]
    if args.iter().any(|arg| arg == "--benchmark-single-adapter") {
        wgpu::hal::dx12::set_single_adapter_preference(Some(preference));
    }
    let instance = wgpu::Instance::new(setup.instance_descriptor);
    sample(&mut samples, "instance");
    let adapter = block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: preference,
        compatible_surface: None,
        force_fallback_adapter: false,
        ..Default::default()
    }))?;
    let info = adapter.get_info();
    #[cfg(windows)]
    // SAFETY: the guard retains the adapter; GetDesc2 only reads its DXGI
    // identity. No raw device/queue mutation or destruction occurs here.
    let adapter_luid = unsafe { adapter.as_hal::<wgpu::hal::api::Dx12>() }
        .and_then(|adapter| unsafe { adapter.raw_adapter().GetDesc2() }.ok())
        .map(|desc| {
            format!(
                "{:08x}{:08x}",
                desc.AdapterLuid.HighPart as u32, desc.AdapterLuid.LowPart
            )
        });
    #[cfg(not(windows))]
    let adapter_luid: Option<String> = None;
    sample(&mut samples, "adapter");
    let mut descriptor = descriptor(&adapter);
    descriptor.required_limits.max_non_sampler_bindings = 4096;
    descriptor.memory_hints = wgpu::MemoryHints::MemoryUsage;
    let (device, queue) = block_on(adapter.request_device(&descriptor))?;
    sample(&mut samples, "device");
    let renderer = eframe::egui_wgpu::Renderer::new(
        &device,
        wgpu::TextureFormat::Bgra8Unorm,
        eframe::egui_wgpu::RendererOptions::default(),
    );
    sample(&mut samples, "renderer");
    drop(renderer);
    drop(queue);
    drop(device);
    drop(adapter);
    drop(instance);
    sample(&mut samples, "released");
    let report = serde_json::json!({"schema":1,"workload":"gpu-context-isolation","backend":format!("{:?}",info.backend),"adapter":info.name,"adapter_luid":adapter_luid,"preferred_adapter_requested":std::env::var("FOLDEX_DX12_PREFERRED_ADAPTER_LUID").ok(),"device_type":format!("{:?}",info.device_type),"window_created":false,"samples":samples});
    if let Some(path) = super::value(args, "--benchmark-output") {
        std::fs::write(path, serde_json::to_vec_pretty(&report)?)?;
    }
    println!("{report}");
    Ok(())
}
