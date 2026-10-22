fn copyright() -> String {
    let year = time::OffsetDateTime::now_utc().year();
    format!("Copyright © 2023-{year} Tuyen Pham")
}

fn generate_pe_header(copyright: &str) {
    let mut res = winres::WindowsResource::new();
    // needed for MinGW cross-compiling
    if cfg!(unix) {
        res.set_windres_path("x86_64-w64-mingw32-windres");
    }
    res.set("LegalCopyright", copyright);
    if std::env::var_os("CARGO_FEATURE_GUI").is_some() {
        println!("cargo:rerun-if-changed=assets/icon.ico");
        res.set_icon("assets/icon.ico");
        res.append_rc_content("STRINGTABLE\nBEGIN\n    101 \"Regressify Desktop\"\nEND\n");
        let (version, parts) = desktop_version();
        let packed = parts
            .iter()
            .fold(0u64, |value, part| (value << 16) | u64::from(*part));
        res.set("OriginalFilename", "regressify-desktop.exe");
        res.set("ProductName", "Regressify Desktop");
        res.set("FileDescription", "Regressify Desktop");
        res.set("FileVersion", &version);
        res.set("ProductVersion", &version);
        res.set_version_info(winres::VersionInfo::FILEVERSION, packed);
        res.set_version_info(winres::VersionInfo::PRODUCTVERSION, packed);
    }
    res.compile().expect("Failed to compile Windows resources!");
}

fn desktop_version() -> (String, [u16; 4]) {
    let value = std::env::var("REGRESSIFY_DESKTOP_VERSION")
        .ok()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| std::env::var("CARGO_PKG_VERSION").expect("Cargo package version"));
    let mut parts = [0; 4];
    for (index, component) in value.split('.').enumerate() {
        assert!(
            index < 4
                && !component.is_empty()
                && component.bytes().all(|byte| byte.is_ascii_digit()),
            "Desktop version must have one to four numeric components"
        );
        let number: u16 = component
            .parse()
            .expect("Invalid desktop version component");
        assert!(number <= 9999, "Desktop version components must be 0-9999");
        parts[index] = number;
    }
    (
        format!("{}.{}.{}.{}", parts[0], parts[1], parts[2], parts[3]),
        parts,
    )
}

fn emit_desktop_release() {
    println!("cargo:rerun-if-env-changed=REGRESSIFY_DESKTOP_VERSION");
    println!("cargo:rerun-if-env-changed=REGRESSIFY_UPDATE_ORIGIN");
    let (version, _) = desktop_version();
    // Official release builds name the website origin whose /downloads/ hosts the
    // update feed; other builds leave it empty, which disables update checks.
    let origin = std::env::var("REGRESSIFY_UPDATE_ORIGIN").unwrap_or_default();
    assert!(
        origin.is_empty()
            || origin.strip_prefix("https://").is_some_and(|host| {
                !host.is_empty()
                    && host.len() <= 253
                    && host.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || byte == b'-'
                            || byte == b'.'
                    })
            }),
        "REGRESSIFY_UPDATE_ORIGIN must be an https://host origin without a path"
    );
    println!("cargo:rustc-env=REGRESSIFY_DESKTOP_VERSION={version}");
    println!("cargo:rustc-env=REGRESSIFY_UPDATE_ORIGIN={origin}");
}

fn emit_git_hash() {
    // Prefer an explicitly provided hash (set by CI, e.g. ci/make-release.sh)
    // so release builds embed the exact source commit even when building from a
    // tarball without a .git directory. Fall back to `git rev-parse HEAD`, then
    // to "unknown".
    let hash = std::env::var("BINARY_GIT_HASH")
        .ok()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("GIT_HASH")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| {
            std::env::var("GITHUB_SHA")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .or_else(|| {
            std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=BINARY_GIT_HASH={hash}");

    // Rebuild when the embedded hash source changes.
    println!("cargo:rerun-if-env-changed=BINARY_GIT_HASH");
    println!("cargo:rerun-if-env-changed=GIT_HASH");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    for head in [".git/HEAD", ".git/refs/heads"] {
        if std::path::Path::new(head).exists() {
            println!("cargo:rerun-if-changed={head}");
        }
    }
}

fn main() {
    // Always embed the git hash so it is available to every build profile.
    emit_git_hash();
    emit_desktop_release();
    let copyright = copyright();
    println!("cargo:rustc-env=REGRESSIFY_COPYRIGHT={copyright}");

    // GPUI's debug render trees exceed Windows' default 1 MiB main-thread stack.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows")
        && std::env::var_os("CARGO_FEATURE_GUI").is_some()
    {
        let argument = if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
            "/STACK:8388608"
        } else {
            "-Wl,--stack,8388608"
        };
        println!("cargo:rustc-link-arg-bin=regressify-desktop={argument}");
    }

    if std::env::var("CARGO_CFG_TARGET_OS").unwrap() != "windows"
        && std::env::var("PROFILE").unwrap() != "release"
    {
        return;
    }
    if cfg!(windows) {
        generate_pe_header(&copyright);
    }
}
