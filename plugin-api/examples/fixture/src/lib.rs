#![no_std]

extern crate alloc;

use alloc::string::String;

#[global_allocator]
static ALLOCATOR: dlmalloc::GlobalDlmalloc = dlmalloc::GlobalDlmalloc;

#[no_mangle]
pub unsafe extern "C" fn cabi_realloc(
    old_ptr: *mut u8,
    old_len: usize,
    align: usize,
    new_len: usize,
) -> *mut u8 {
    use alloc::alloc::{alloc, dealloc, realloc, Layout};

    if old_len == 0 {
        if new_len == 0 {
            return align as *mut u8;
        }
        let pointer = alloc(Layout::from_size_align_unchecked(new_len, align));
        if pointer.is_null() {
            core::arch::wasm32::unreachable();
        }
        return pointer;
    }

    let layout = Layout::from_size_align_unchecked(old_len, align);
    if new_len == 0 {
        dealloc(old_ptr, layout);
        return align as *mut u8;
    }
    let pointer = realloc(old_ptr, layout, new_len);
    if pointer.is_null() {
        core::arch::wasm32::unreachable();
    }
    pointer
}

wit_bindgen::generate!({
    path: "../../wit",
    world: "adapter",
});

struct Fixture;

impl Guest for Fixture {
    fn invoke(_request: String) -> Result<String, String> {
        Ok(String::from(r#"{"operation":"valid"}"#))
    }
}

export!(Fixture);

#[panic_handler]
fn panic(_info: &core::panic::PanicInfo<'_>) -> ! {
    loop {}
}
