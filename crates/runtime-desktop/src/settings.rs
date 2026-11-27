//! Injects a native GPUI **Sunbird** tab directly into Desktop Phoenix's User
//! Settings sidebar and renders native Mana GPUI settings controls when
//! selected.
//!
//! All patches are build-independent anchor lookups run from [`crate::prepare`]
//! inside `DllMain` before Discord initializes:
//! 1. Relabel settings page `24` (`Other -> Phoenix`, right above `Log Out`) to
//!    `Sunbird` by replacing the unique 22-byte `.rdata` string
//!    `PhoenixDESKTOP_PHOENIX` in-place with `SunbirdDESKTOP_SUNBIRD`.
//! 2. Unconditionally show page `24` in `is_page_visible` by tightening the
//!    `cmp r8d, 0xf` range guard (`pages 9..=24`) to `0x0e` (`pages 9..=23`),
//!    so page `24` falls through to `return true`.
//! 3. Resolve Discord's native GPUI/Mana widget builders ([`crate::gpui`]) and
//!    inline-hook `host::section` (located via the unique `account-loading`
//!    string xref and `.pdata` function bounds) so when active page `== 24`, it
//!    builds and returns a native GPUI `AnyElement` directly into Discord's
//!    User Settings scroll viewport.

use std::{
   ffi::c_void,
   sync::{
      OnceLock,
      atomic::{
         AtomicUsize,
         Ordering,
      },
   },
};

use phoenix_hook::image::Image;
use phoenix_sigs::desktop;

use crate::gpui::{
   self,
   RawAnyElement,
};

const SUNBIRD_PAGE: u8 = 24;

type SectionFn = unsafe extern "C" fn(
   sret: *mut RawAnyElement,
   host_state: *mut c_void,
   host_data: *mut c_void,
   host_vtable: *const c_void,
   window: *mut c_void,
   cx: *mut c_void,
) -> *mut RawAnyElement;

static ORIG_SECTION: OnceLock<SectionFn> = OnceLock::new();
static PAGE_OFFSET: AtomicUsize = AtomicUsize::new(0);

/// Installs the Desktop User Settings sidebar label, visibility patch, native
/// GPUI builder bindings, and `host::section` render hook.
pub fn install(image: &Image<'static>) {
   patch_sidebar_label(image);
   patch_page_visibility(image);
   if gpui::locate(image) {
      hook_section_render(image);
   }
}

fn patch_sidebar_label(image: &Image<'static>) {
   const NEW_NAME: &[u8] = b"Sunbird";
   const NEW_ID: &[u8] = b"SUNBIRD";
   const _: () = assert!(NEW_NAME.len() == b"Phoenix".len());
   const _: () = assert!(NEW_ID.len() == b"PHOENIX".len());

   let label = match desktop::settings_label_parts(image) {
      Ok(label) => label,
      Err(err) => {
         log::warn!("desktop settings: {err}, leaving sidebar label");
         return;
      },
   };
   // SAFETY: exact-length `.rodata`/`.rdata` string patches executed from
   // `prepare` before Discord threads run.
   if let Err(err) = unsafe { image.write_code(label.name, NEW_NAME) } {
      log::warn!("desktop settings: relabel name failed: {err}");
      return;
   }
   match unsafe { image.write_code(label.id, NEW_ID) } {
      Ok(()) => log::info!("desktop settings: relabeled page 24 to Sunbird"),
      Err(err) => log::warn!("desktop settings: relabel id failed: {err}"),
   }
}

fn patch_page_visibility(image: &Image<'static>) {
   let limit = match desktop::settings_visibility(image) {
      Ok(limit) => limit,
      Err(err) => {
         log::warn!("desktop settings: {err}, leaving it");
         return;
      },
   };
   // Tightening `cmp r8d, 0xf` -> `0x0e` excludes page 24 (`24 - 9 = 15`) from
   // the gated switch so `is_page_visible(24)` returns `true` unconditionally.
   // SAFETY: single-byte immediate patch in `.text` executed from `DllMain`.
   match unsafe { image.write_code(limit, &[0x0E]) } {
      Ok(()) => log::info!("desktop settings: page 24 visibility unlocked"),
      Err(err) => log::warn!("desktop settings: visibility patch failed: {err}"),
   }
}

fn hook_section_render(image: &Image<'static>) {
   let (fn_start, offset) = match desktop::settings_section(image) {
      Ok(section) => (section.func, section.page_offset),
      Err(err) => {
         log::warn!("desktop settings: {err}, skipping section hook");
         return;
      },
   };
   PAGE_OFFSET.store(offset, Ordering::Release);

   let pro = desktop::Prologues::of(phoenix_hook::x86::Abi::of(image)).frame;
   // SAFETY: runs from `prepare` before Discord threads execute;
   // `section_detour` matches `host::section`'s exact 6-argument sret ABI.
   let trampoline = match unsafe {
      image.hook_entry(
         fn_start,
         pro.pattern,
         pro.len,
         section_detour as *const () as usize,
      )
   } {
      Ok(trampoline) => trampoline,
      Err(err) => {
         log::warn!("desktop settings: section hook failed: {err}");
         return;
      },
   };
   // SAFETY: `trampoline` replays `fn_start`'s prologue and jumps to `fn_start
   // + 19`.
   let orig = unsafe { std::mem::transmute::<usize, SectionFn>(trampoline) };
   let _ = ORIG_SECTION.set(orig);
   log::info!("desktop settings: hooked section render @ {fn_start:#x} (page offset {offset:#x})");
}

unsafe extern "C" fn section_detour(
   sret: *mut RawAnyElement,
   host_state: *mut c_void,
   host_data: *mut c_void,
   host_vtable: *const c_void,
   window: *mut c_void,
   cx: *mut c_void,
) -> *mut RawAnyElement {
   if !host_state.is_null() && !sret.is_null() {
      let offset = PAGE_OFFSET.load(Ordering::Acquire);
      // SAFETY: `host_state` is Discord's live `&mut HostState` for this render
      // call.
      let page = unsafe { *host_state.cast::<u8>().add(offset) };
      if page == SUNBIRD_PAGE {
         let rendered =
            std::panic::catch_unwind(|| gpui::render_sunbird_page(sret)).unwrap_or(false);
         if rendered {
            return sret;
         }
      }
   }
   let Some(orig) = ORIG_SECTION.get().copied() else {
      return sret;
   };
   // SAFETY: forwards all arguments verbatim to the original `host::section`.
   unsafe { orig(sret, host_state, host_data, host_vtable, window, cx) }
}
