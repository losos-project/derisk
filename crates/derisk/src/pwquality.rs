//! homed's password quality check, asked while the password is typed.
//!
//! homed refuses a new account whose password libpwquality rejects, and
//! homectl then waits for another one that the setup cannot give it
//! ([`derisk::setup::ask_socket`]). systemd loads libpwquality at run time
//! and checks with its built-in defaults, `/etc/security/pwquality.conf`,
//! and the dictionary check off when the dictionary file is missing. This
//! loads the same library and asks the same way, so the account page says
//! what homed would say before "Set up" is pressed. Where the library cannot
//! be loaded the page judges nothing and homed's refusal still comes back
//! as a failure.

use std::ffi::{CStr, CString, c_char, c_int, c_void};

/// libpwquality's `PWQ_SETTING_DICT_PATH`.
const DICT_PATH: c_int = 10;
/// libpwquality's `PWQ_SETTING_DICT_CHECK`.
const DICT_CHECK: c_int = 15;
/// libpwquality's `PWQ_MAX_ERROR_MESSAGE_LEN`.
const MAX_ERROR_MESSAGE_LEN: usize = 256;

type DefaultSettings = unsafe extern "C" fn() -> *mut c_void;
type FreeSettings = unsafe extern "C" fn(*mut c_void);
type ReadConfig = unsafe extern "C" fn(*mut c_void, *const c_char, *mut *mut c_void) -> c_int;
type GetStr = unsafe extern "C" fn(*mut c_void, c_int, *mut *const c_char) -> c_int;
type SetInt = unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int;
type Check = unsafe extern "C" fn(
    *mut c_void,
    *const c_char,
    *const c_char,
    *const c_char,
    *mut *mut c_void,
) -> c_int;
type StrError = unsafe extern "C" fn(*mut c_char, usize, c_int, *mut c_void) -> *const c_char;

/// libpwquality, loaded, with the settings systemd checks with. Owns both:
/// dropping it frees the settings and closes the library.
pub struct Checker {
    library: *mut c_void,
    settings: *mut c_void,
    check: Check,
    strerror: StrError,
    free: FreeSettings,
}

impl Checker {
    /// Loads libpwquality, or `None` where it is not installed.
    pub fn load() -> Option<Self> {
        // SAFETY: a soname and a flag; a null handle is checked below.
        let library = unsafe { libc::dlopen(c"libpwquality.so.1".as_ptr(), libc::RTLD_NOW) };
        if library.is_null() {
            return None;
        }
        // SAFETY: each symbol is libpwquality's, with the signature its
        // header (pwquality.h) gives it.
        let symbols = unsafe {
            (
                symbol::<DefaultSettings>(library, c"pwquality_default_settings"),
                symbol::<FreeSettings>(library, c"pwquality_free_settings"),
                symbol::<ReadConfig>(library, c"pwquality_read_config"),
                symbol::<GetStr>(library, c"pwquality_get_str_value"),
                symbol::<SetInt>(library, c"pwquality_set_int_value"),
                symbol::<Check>(library, c"pwquality_check"),
                symbol::<StrError>(library, c"pwquality_strerror"),
            )
        };
        let (
            Some(default_settings),
            Some(free),
            Some(read_config),
            Some(get_str),
            Some(set_int),
            Some(check),
            Some(strerror),
        ) = symbols
        else {
            // SAFETY: opened above, and nothing from it is kept.
            unsafe { libc::dlclose(library) };
            return None;
        };
        // SAFETY: the library's own allocator; a null is checked below.
        let settings = unsafe { default_settings() };
        if settings.is_null() {
            // SAFETY: as above.
            unsafe { libc::dlclose(library) };
            return None;
        }
        let checker = Self {
            library,
            settings,
            check,
            strerror,
            free,
        };
        // SAFETY: `settings` is live; a null file name means the default
        // configuration, and the auxiliary error goes back to strerror,
        // which frees it. A configuration that fails to read leaves the
        // defaults, as systemd does.
        unsafe {
            let mut aux = std::ptr::null_mut();
            let read = read_config(settings, std::ptr::null(), &mut aux);
            if read != 0 {
                checker.message(read, aux);
            }
            let mut path = std::ptr::null();
            if get_str(settings, DICT_PATH, &mut path) == 0
                && !path.is_null()
                && let Ok(path) = CStr::from_ptr(path).to_str()
                && !path.is_empty()
                && !std::path::Path::new(path).exists()
            {
                set_int(settings, DICT_CHECK, 0);
            }
        }
        Some(checker)
    }

    /// Why homed would refuse `password` for `user`, in libpwquality's
    /// words ("The password is shorter than 8 characters"), or `None` when
    /// it would take it.
    pub fn weakness(&self, password: &str, user: &str) -> Option<String> {
        let password = CString::new(password).ok()?;
        let user = CString::new(user).ok()?;
        let mut aux = std::ptr::null_mut();
        // SAFETY: live settings and NUL-terminated strings; no old
        // password, as for a new account.
        let score = unsafe {
            (self.check)(
                self.settings,
                password.as_ptr(),
                std::ptr::null(),
                user.as_ptr(),
                &mut aux,
            )
        };
        // A negative score is an error code; zero and up are accepted.
        (score < 0).then(|| self.message(score, aux))
    }

    /// The text for `code`, freeing `aux` as libpwquality requires.
    fn message(&self, code: c_int, aux: *mut c_void) -> String {
        let mut buf = [0 as c_char; MAX_ERROR_MESSAGE_LEN];
        // SAFETY: a buffer of the length passed; the result is either that
        // buffer or a static string, both NUL-terminated.
        unsafe {
            let text = (self.strerror)(buf.as_mut_ptr(), buf.len(), code, aux);
            if text.is_null() {
                return String::new();
            }
            CStr::from_ptr(text).to_string_lossy().into_owned()
        }
    }
}

impl Drop for Checker {
    fn drop(&mut self) {
        // SAFETY: both were made in `load` and are freed only here.
        unsafe {
            (self.free)(self.settings);
            libc::dlclose(self.library);
        }
    }
}

/// `name` from `library` as a function of type `F`, or `None` when the
/// library lacks it.
///
/// # Safety
///
/// `F` must be the symbol's real signature, and `library` a live handle.
unsafe fn symbol<F: Copy>(library: *mut c_void, name: &CStr) -> Option<F> {
    // SAFETY: the caller's.
    let address = unsafe { libc::dlsym(library, name.as_ptr()) };
    // SAFETY: a non-null function address, typed by the caller.
    (!address.is_null()).then(|| unsafe { std::mem::transmute_copy::<*mut c_void, F>(&address) })
}
