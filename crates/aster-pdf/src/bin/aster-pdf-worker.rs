//! A PDF child process on its own, for the process tests: the same as
//! `aster-shell --pdf-worker`.

#[global_allocator]
static ALLOCATOR: aster_pdf::child::Budgeted = aster_pdf::child::Budgeted;

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.first().is_some_and(|a| a == "--sandbox-selftest") {
        std::process::exit(selftest(args.get(1), true));
    }
    // Control run: the same, but no font directory may be read.
    if args
        .first()
        .is_some_and(|a| a == "--sandbox-selftest-no-fonts")
    {
        std::process::exit(selftest(args.get(1), false));
    }
    let code = aster_pdf::child::run_if_child(&args).unwrap_or(2);
    std::process::exit(code);
}

/// Descriptors above 2 that are open now.
fn open_descriptors() -> Vec<i32> {
    #[cfg(unix)]
    {
        // SAFETY: F_GETFD only asks whether the number is open.
        (3..256)
            .filter(|fd| unsafe { libc::fcntl(*fd, libc::F_GETFD) } != -1)
            .collect()
    }
    #[cfg(not(unix))]
    {
        Vec::new()
    }
}

/// Hidden mode: close descriptors, read the file, enter the sandbox, render
/// page 0, then try forbidden things. One line per check.
fn selftest(path: Option<&std::ffi::OsString>, with_fonts: bool) -> i32 {
    use aster_pdf::sandbox;
    use std::io::Write;
    use std::net::{TcpStream, ToSocketAddrs};
    use std::time::Duration;

    let Some(path) = path else { return 2 };
    println!("fds before {:?}", open_descriptors());
    sandbox::close_inherited_descriptors();
    println!("fds after {:?}", open_descriptors());
    let Ok(bytes) = std::fs::read(path) else {
        println!("file unreadable");
        return 2;
    };
    let dirs = if with_fonts {
        sandbox::font_dirs()
    } else {
        Vec::new()
    };
    let dirs: Vec<&std::path::Path> = dirs.iter().map(|d| d.as_path()).collect();
    match sandbox::restrict(&dirs) {
        Ok(()) => println!("sandbox on"),
        Err(e) => println!("sandbox off: {e}"),
    }
    let _ = std::io::stdout().flush();

    match sandbox::render_first_page_for_selftest(bytes) {
        Ok((w, h, ink)) => println!("render ok {w}x{h} ink {ink}"),
        Err(e) => println!("render failed: {e}"),
    }
    let verdict = |ok: bool, allowed: &str, denied: &str| {
        println!("{}", if ok { allowed } else { denied });
    };
    verdict(
        std::fs::read("/etc/hosts").is_ok(),
        "read allowed",
        "read denied",
    );
    let file = std::env::temp_dir().join("aster-selftest-write");
    let wrote = std::fs::write(&file, b"x").is_ok();
    if wrote {
        let _ = std::fs::remove_file(&file);
    }
    verdict(wrote, "write allowed", "write denied");
    let connected =
        TcpStream::connect_timeout(&"127.0.0.1:9".parse().unwrap(), Duration::from_secs(2));
    // Without the sandbox, port 9 refuses; with it, the call fails by
    // permission. Tell them apart.
    let kind = connected.as_ref().err().map(|e| e.kind());
    verdict(
        connected.is_ok() || kind == Some(std::io::ErrorKind::ConnectionRefused),
        "connect allowed",
        "connect denied",
    );
    verdict(
        ("localhost", 80).to_socket_addrs().is_ok(),
        "resolve allowed",
        "resolve denied",
    );
    verdict(
        std::process::Command::new("/bin/echo")
            .arg("x")
            .output()
            .is_ok(),
        "spawn allowed",
        "spawn denied",
    );
    // What the profile used to allow and must not: the environment and the
    // process list through sysctl, metadata of paths outside the font
    // directories, readlink, the IOKit user clients.
    // SAFETY (all unsafe below): plain system calls with buffers we own.
    #[cfg(target_os = "macos")]
    {
        let parent = unsafe { libc::getppid() };
        let sysctl_size = |mib: &mut [libc::c_int]| -> bool {
            let mut size: libc::size_t = 1 << 16;
            let mut buf = vec![0u8; size];
            let result = unsafe {
                libc::sysctl(
                    mib.as_mut_ptr(),
                    mib.len() as libc::c_uint,
                    buf.as_mut_ptr().cast(),
                    &mut size,
                    std::ptr::null_mut(),
                    0,
                )
            };
            result == 0
        };
        verdict(
            sysctl_size(&mut [libc::CTL_KERN, libc::KERN_PROCARGS2, parent]),
            "procargs allowed",
            "procargs denied",
        );
        verdict(
            sysctl_size(&mut [libc::CTL_KERN, libc::KERN_PROCARGS2, unsafe {
                libc::getpid()
            }]),
            "own procargs allowed",
            "own procargs denied",
        );
        verdict(
            sysctl_size(&mut [libc::CTL_KERN, libc::KERN_PROC, libc::KERN_PROC_ALL, 0]),
            "proclist allowed",
            "proclist denied",
        );
        let home = std::env::var("HOME").unwrap_or_default();
        verdict(
            std::fs::metadata(format!("{home}/Library/Keychains/login.keychain-db")).is_ok(),
            "stat allowed",
            "stat denied",
        );
        verdict(
            std::fs::metadata("/etc").is_ok(),
            "stat etc allowed",
            "stat etc denied",
        );
        verdict(
            std::fs::read_link("/etc").is_ok(),
            "readlink allowed",
            "readlink denied",
        );
        verdict(
            std::fs::read_dir("/Library/Fonts/..").is_ok(),
            "list parent allowed",
            "list parent denied",
        );
        verdict(iokit_open(), "iokit allowed", "iokit denied");
    }
    0
}

/// Try to open IOKit user clients (the surface and the GPU); true if any
/// of them opens.
#[cfg(target_os = "macos")]
fn iokit_open() -> bool {
    use std::ffi::CString;
    use std::os::raw::{c_char, c_int, c_uint, c_void};

    #[link(name = "IOKit", kind = "framework")]
    unsafe extern "C" {
        fn IOServiceMatching(name: *const c_char) -> *mut c_void;
        fn IOServiceGetMatchingService(main: c_uint, matching: *mut c_void) -> c_uint;
        fn IOServiceOpen(service: c_uint, task: c_uint, kind: u32, conn: *mut c_uint) -> c_int;
    }
    unsafe extern "C" {
        static mach_task_self_: c_uint;
    }
    let mut opened = false;
    for class in ["IOSurfaceRoot", "AGXAccelerator", "IOHIDSystem"] {
        let name = CString::new(class).unwrap();
        // SAFETY: IOKit calls with valid arguments; a zero service is
        // rejected by IOServiceOpen.
        unsafe {
            let service = IOServiceGetMatchingService(0, IOServiceMatching(name.as_ptr()));
            let mut connection: c_uint = 0;
            if service != 0 && IOServiceOpen(service, mach_task_self_, 0, &mut connection) == 0 {
                opened = true;
            }
        }
    }
    opened
}
