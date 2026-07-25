//! C FFI interface for abyssflower.
//!
//! Usage from C/C++:
//!   const char* result = abyssflower_decompile(bytes, len);
//!   // use result...
//!   abyssflower_free(result);
//!
//! Usage from Python (ctypes):
//!   lib = ctypes.CDLL("abyssflower_lib.dll")
//!   lib.abyssflower_decompile.restype = ctypes.c_char_p
//!   lib.abyssflower_decompile.argtypes = [ctypes.c_char_p, ctypes.c_size_t]
//!   result = lib.abyssflower_decompile(data, len(data))
//!   # result is bytes, decode with .decode('utf-8')
//!   lib.abyssflower_free(result)

use std::ffi::CString;
use std::os::raw::c_char;
use std::ptr;

use crate::classfile::ClassFile;
use crate::codegen::class_writer::render_class;
use crate::kotlin::writer::{is_kotlin_class, render_kotlin_class};

/// Decompile a .class file from raw bytes.
///
/// Returns a null-terminated UTF-8 string with the decompiled source,
/// or NULL on error. The caller must free the result with `abyssflower_free`.
///
/// # Safety
/// `data` must point to `len` valid bytes.
#[no_mangle]
pub unsafe extern "C" fn abyssflower_decompile(data: *const u8, len: usize) -> *mut c_char {
    if data.is_null() || len == 0 {
        return ptr::null_mut();
    }

    let bytes = std::slice::from_raw_parts(data, len);
    let cf = match ClassFile::parse(bytes) {
        Ok(cf) => cf,
        Err(_) => return ptr::null_mut(),
    };

    // Try Kotlin first, fall back to Java
    let source = if is_kotlin_class(&cf) {
        render_kotlin_class(&cf)
    } else {
        render_class(&cf)
    };

    match CString::new(source) {
        Ok(cs) => cs.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

/// Decompile a .class file, forcing Java output (no Kotlin metadata).
///
/// # Safety
/// `data` must point to `len` valid bytes.
#[no_mangle]
pub unsafe extern "C" fn abyssflower_decompile_java(data: *const u8, len: usize) -> *mut c_char {
    if data.is_null() || len == 0 {
        return ptr::null_mut();
    }

    let bytes = std::slice::from_raw_parts(data, len);
    let cf = match ClassFile::parse(bytes) {
        Ok(cf) => cf,
        Err(_) => return ptr::null_mut(),
    };

    let source = render_class(&cf);

    match CString::new(source) {
        Ok(cs) => cs.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

/// Free a string returned by `abyssflower_decompile`.
///
/// # Safety
/// `ptr` must be a pointer returned by `abyssflower_decompile` or NULL.
#[no_mangle]
pub unsafe extern "C" fn abyssflower_free(ptr: *mut c_char) {
    if !ptr.is_null() {
        drop(CString::from_raw(ptr));
    }
}

/// Get the library version string.
/// The returned pointer is static and must NOT be freed.
#[no_mangle]
pub extern "C" fn abyssflower_version() -> *const c_char {
    static VERSION: &[u8] = b"0.1.0\0";
    VERSION.as_ptr() as *const c_char
}
