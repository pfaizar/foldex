//! Scoped geometry for a synchronous paint before Win32 applies a live resize.

use std::{
    cell::{Cell, RefCell},
    collections::HashMap,
    marker::PhantomData,
    rc::Rc,
};

use winit::dpi::PhysicalSize;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Geometry {
    owner: isize,
    rect: [i32; 4],
    size: PhysicalSize<u32>,
}

impl Geometry {
    fn new(owner: isize, rect: [i32; 4]) -> Option<Self> {
        let width = rect[2].checked_sub(rect[0])?;
        let height = rect[3].checked_sub(rect[1])?;
        (owner != 0 && width > 0 && height > 0).then_some(Self {
            owner,
            rect,
            size: PhysicalSize::new(width as u32, height as u32),
        })
    }
}

#[derive(Clone, Copy)]
struct Hint {
    geometry: Geometry,
    claimed: bool,
    painted: bool,
}

thread_local! {
    static RENDER_DEPTH: Cell<usize> = const { Cell::new(0) };
    static HINT: Cell<Option<Hint>> = const { Cell::new(None) };
    static ACKNOWLEDGED: RefCell<HashMap<isize, Geometry>> = RefCell::new(HashMap::new());
}

struct HintGuard;

impl Drop for HintGuard {
    fn drop(&mut self) {
        // Also runs when the caller's synchronous paint unwinds.
        HINT.with(|hint| hint.set(None));
    }
}

/// Offer a physical client rectangle for a synchronous pre-resize paint.
///
/// Call only on the HWND's owning thread, after calculating its final proposed
/// client rectangle in WM_NCCALCSIZE. `paint` should send WM_PAINT synchronously
/// and return true only if a native presentation for this HWND and extent
/// succeeded. The hint never outlives that call and is consumed by its HWND only.
///
/// Returns true only when that presentation and the completed UI/painter extent
/// match the proposal. An active application/render callback declines the call;
/// the ordinary WM_SIZE paint remains the fallback.
#[doc(hidden)]
pub fn with_windows_early_resize(
    owner: isize,
    physical_client_rect: [i32; 4],
    paint: impl FnOnce() -> bool,
) -> bool {
    let Some(geometry) = Geometry::new(owner, physical_client_rect) else {
        return false;
    };
    ACKNOWLEDGED.with(|acks| acks.borrow_mut().remove(&owner));
    if RENDER_DEPTH.with(|depth| depth.get() != 0)
        || super::event_loop_context::with_current_event_loop(|_| ()).is_some()
        || HINT.with(|hint| hint.get().is_some())
    {
        return false;
    }

    HINT.with(|hint| {
        hint.set(Some(Hint {
            geometry,
            claimed: false,
            painted: false,
        }));
    });
    let _guard = HintGuard;
    let presented = paint();
    let completed = HINT
        .with(|hint| hint.take())
        .is_some_and(|hint| hint.geometry == geometry && hint.claimed && hint.painted && presented);
    if completed {
        ACKNOWLEDGED.with(|acks| acks.borrow_mut().insert(owner, geometry));
    }
    completed
}

/// Discard pending resize state before removing a native frame or destroying its HWND.
#[doc(hidden)]
pub fn forget_windows_early_resize(owner: isize) {
    ACKNOWLEDGED.with(|acks| acks.borrow_mut().remove(&owner));
    HINT.with(|hint| {
        if hint.get().is_some_and(|hint| hint.geometry.owner == owner) {
            hint.set(None);
        }
    });
}

pub(crate) struct RenderGuard {
    previous_depth: usize,
    _same_thread: PhantomData<Rc<()>>,
}

impl RenderGuard {
    pub(crate) fn new() -> Self {
        let previous_depth = RENDER_DEPTH.with(|depth| {
            let previous = depth.get();
            depth.set(previous + 1);
            previous
        });
        Self {
            previous_depth,
            _same_thread: PhantomData,
        }
    }
}

impl Drop for RenderGuard {
    fn drop(&mut self) {
        RENDER_DEPTH.with(|depth| depth.set(self.previous_depth));
    }
}

pub(crate) fn take_proposed_rect(owner: isize) -> Option<[i32; 4]> {
    HINT.with(|slot| {
        let mut hint = slot.get()?;
        if hint.geometry.owner != owner || hint.claimed {
            return None;
        }
        hint.claimed = true;
        slot.set(Some(hint));
        Some(hint.geometry.rect)
    })
}

pub(crate) fn frame_painted(
    owner: isize,
    completed_size: PhysicalSize<u32>,
    configured_size: Option<PhysicalSize<u32>>,
) {
    HINT.with(|slot| {
        if let Some(mut hint) = slot.get()
            && hint.geometry.owner == owner
            && hint.claimed
            && hint.geometry.size == completed_size
            && configured_size == Some(completed_size)
        {
            hint.painted = true;
            slot.set(Some(hint));
        }
    });
}

pub(crate) fn consume_resize_ack(
    owner: isize,
    event_size: PhysicalSize<u32>,
    actual_client_rect: Option<[i32; 4]>,
) -> bool {
    ACKNOWLEDGED.with(|acks| {
        acks.borrow_mut().remove(&owner).is_some_and(|geometry| {
            geometry.size == event_size && Some(geometry.rect) == actual_client_rect
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const OWNER: isize = 17;
    const OTHER: isize = 29;
    const RECT: [i32; 4] = [120, 80, 920, 680];
    const SIZE: PhysicalSize<u32> = PhysicalSize::new(800, 600);

    #[test]
    fn only_the_owner_can_claim_and_acknowledge_one_matching_frame() {
        assert!(with_windows_early_resize(OWNER, RECT, || {
            assert_eq!(take_proposed_rect(OTHER), None);
            assert_eq!(take_proposed_rect(OWNER), Some(RECT));
            assert_eq!(take_proposed_rect(OWNER), None);
            frame_painted(OTHER, SIZE, Some(SIZE));
            frame_painted(OWNER, SIZE, Some(SIZE));
            true
        }));
        assert_eq!(take_proposed_rect(OWNER), None);
        assert!(!consume_resize_ack(OTHER, SIZE, Some(RECT)));
        assert!(consume_resize_ack(OWNER, SIZE, Some(RECT)));
        assert!(!consume_resize_ack(OWNER, SIZE, Some(RECT)));
    }

    #[test]
    fn render_depth_and_nested_scopes_decline_without_running_the_callback() {
        {
            let _parent = RenderGuard::new();
            {
                let _child = RenderGuard::new();
                assert!(!with_windows_early_resize(OWNER, RECT, || panic!(
                    "reentry"
                )));
            }
            assert!(!with_windows_early_resize(OWNER, RECT, || panic!(
                "parent active"
            )));
        }
        assert!(!with_windows_early_resize(OWNER, RECT, || {
            assert!(!with_windows_early_resize(OTHER, RECT, || panic!(
                "nested hint"
            )));
            false
        }));
        assert_eq!(take_proposed_rect(OWNER), None);
    }

    #[test]
    fn skipped_failed_or_zoom_mismatched_frames_keep_the_normal_resize_paint() {
        for (claim, completed, configured, presented) in [
            (false, SIZE, Some(SIZE), true),
            (true, SIZE, Some(SIZE), false),
            (true, PhysicalSize::new(799, 600), Some(SIZE), true),
            (true, SIZE, Some(PhysicalSize::new(799, 600)), true),
            (true, SIZE, None, true),
        ] {
            assert!(!with_windows_early_resize(OWNER, RECT, || {
                if claim {
                    assert_eq!(take_proposed_rect(OWNER), Some(RECT));
                }
                frame_painted(OWNER, completed, configured);
                presented
            }));
            assert!(!consume_resize_ack(OWNER, SIZE, Some(RECT)));
            assert_eq!(take_proposed_rect(OWNER), None);
        }
    }

    #[test]
    fn geometry_mismatch_and_window_destruction_consume_or_clear_stale_acknowledgements() {
        let paint = || {
            take_proposed_rect(OWNER);
            frame_painted(OWNER, SIZE, Some(SIZE));
            true
        };
        assert!(with_windows_early_resize(OWNER, RECT, paint));
        assert!(!consume_resize_ack(OWNER, SIZE, Some([121, 80, 921, 680])));
        assert!(!consume_resize_ack(OWNER, SIZE, Some(RECT)));
        assert!(with_windows_early_resize(OWNER, RECT, paint));
        forget_windows_early_resize(OWNER);
        assert!(!consume_resize_ack(OWNER, SIZE, Some(RECT)));
        assert!(!with_windows_early_resize(OWNER, RECT, || {
            take_proposed_rect(OWNER);
            frame_painted(OWNER, SIZE, Some(SIZE));
            forget_windows_early_resize(OWNER);
            true
        }));
    }

    #[test]
    fn unwinding_clears_the_hint_without_publishing_an_acknowledgement() {
        let result = std::panic::catch_unwind(|| {
            with_windows_early_resize(OWNER, RECT, || {
                take_proposed_rect(OWNER);
                frame_painted(OWNER, SIZE, Some(SIZE));
                panic!("paint failed");
            });
        });
        assert!(result.is_err());
        assert_eq!(take_proposed_rect(OWNER), None);
        assert!(!consume_resize_ack(OWNER, SIZE, Some(RECT)));
        assert!(!with_windows_early_resize(0, RECT, || panic!(
            "invalid owner"
        )));
        assert!(!with_windows_early_resize(OWNER, [0; 4], || panic!(
            "empty rect"
        )));
    }
}
