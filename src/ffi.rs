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

use crate::{DecompileLanguage, DecompileOptions, Decompiler};

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
    let output = match Decompiler::default().decompile_bytes(bytes) {
        Ok(output) => output,
        Err(_) => return ptr::null_mut(),
    };
    match CString::new(output.source) {
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
    let decompiler = Decompiler::new(DecompileOptions {
        language: DecompileLanguage::Java,
        ..DecompileOptions::default()
    });
    let output = match decompiler.decompile_bytes(bytes) {
        Ok(output) => output,
        Err(_) => return ptr::null_mut(),
    };
    match CString::new(output.source) {
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
    concat!(env!("CARGO_PKG_VERSION"), "\0").as_ptr() as *const c_char
}

/// Decompile a .class file by file path.
///
/// `path` must be a null-terminated UTF-8 string pointing to a .class file.
/// Returns decompiled source or NULL on error. Free with `abyssflower_free`.
///
/// # Safety
/// `path` must be a valid null-terminated C string.
#[no_mangle]
pub unsafe extern "C" fn abyssflower_decompile_file(path: *const c_char) -> *mut c_char {
    if path.is_null() {
        return ptr::null_mut();
    }

    let c_str = std::ffi::CStr::from_ptr(path);
    let path_str = match c_str.to_str() {
        Ok(s) => s,
        Err(_) => return ptr::null_mut(),
    };

    let output = match Decompiler::default().decompile_file(path_str) {
        Ok(output) => output,
        Err(_) => return ptr::null_mut(),
    };
    match CString::new(output.source) {
        Ok(cs) => cs.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}

/// Decompile a .class entry from a JAR (zip) file.
///
/// `jar_path` — null-terminated path to the .jar file.
/// `class_path` — null-terminated path of the .class entry inside the JAR
///                (e.g. "com/example/Main.class").
///
/// Returns decompiled source or NULL on error. Free with `abyssflower_free`.
///
/// # Safety
/// Both `jar_path` and `class_path` must be valid null-terminated C strings.
#[no_mangle]
pub unsafe extern "C" fn abyssflower_decompile_jar_entry(
    jar_path: *const c_char,
    class_path: *const c_char,
) -> *mut c_char {
    if jar_path.is_null() || class_path.is_null() {
        return ptr::null_mut();
    }

    let jar_str = match std::ffi::CStr::from_ptr(jar_path).to_str() {
        Ok(s) => s,
        Err(_) => return ptr::null_mut(),
    };
    let class_str = match std::ffi::CStr::from_ptr(class_path).to_str() {
        Ok(s) => s,
        Err(_) => return ptr::null_mut(),
    };

    let output = match Decompiler::default().decompile_jar_entry(jar_str, class_str) {
        Ok(output) => output,
        Err(_) => return ptr::null_mut(),
    };
    match CString::new(output.source) {
        Ok(cs) => cs.into_raw(),
        Err(_) => ptr::null_mut(),
    }
}
