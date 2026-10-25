mod app_icon;
mod epi_integration;
mod event_loop_context;
pub mod run;

#[cfg(target_os = "macos")]
pub(crate) mod macos;

/// File storage which can be used by native backends.
#[cfg(feature = "persistence")]
pub mod file_storage;

pub(crate) mod winit_integration;

#[cfg(feature = "glow")]
mod glow_integration;

#[cfg(feature = "wgpu_no_default_features")]
mod wgpu_integration;

#[cfg(all(feature = "wgpu_no_default_features", any(target_os = "windows", test)))]
pub(crate) mod windows_resize;
