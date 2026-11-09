//! Compiles OpenTUI 0.8.1's Yoga 3.2.1 as `packages/native/build.zig` (`addYogaDependencies`) compiles it for the
//! shipped library (`zig build -Doptimize=ReleaseFast`): the same 19 Yoga sources and `src/yoga-bridge.cpp`, the same
//! include root, `YOGA_CXX_FLAGS` (`-std=c++20 -fexceptions -frtti`), the `-DFLT_MAX=__FLT_MAX__` macOS define, and
//! what Zig's ReleaseFast adds to C++ (`-DNDEBUG -O2`) whatever the cargo profile. Zig compiles C++ with clang, so
//! clang++ is preferred when `CXX` is unset and it is on the path. `src/logger.cpp` (this crate's) adds the
//! `YGLogger` trampoline that formats Yoga's messages for Rust, as yoga.zig's `internalLogger` does with
//! `otYogaFormatLog`.
//!
//! The sources come from `port/.vendor/opentui` (fetched by `port/scripts/vendor-opentui.sh`, gitignored);
//! `OPENCODE_OPENTUI_NATIVE` names another `packages/native` directory.
use std::path::{Path, PathBuf};

const YOGA_CXX_SOURCES: [&str; 19] = [
    "yoga/YGConfig.cpp",
    "yoga/YGEnums.cpp",
    "yoga/YGNode.cpp",
    "yoga/YGNodeLayout.cpp",
    "yoga/YGNodeStyle.cpp",
    "yoga/YGPixelGrid.cpp",
    "yoga/YGValue.cpp",
    "yoga/algorithm/AbsoluteLayout.cpp",
    "yoga/algorithm/Baseline.cpp",
    "yoga/algorithm/Cache.cpp",
    "yoga/algorithm/CalculateLayout.cpp",
    "yoga/algorithm/FlexLine.cpp",
    "yoga/algorithm/PixelGrid.cpp",
    "yoga/config/Config.cpp",
    "yoga/debug/AssertFatal.cpp",
    "yoga/debug/Log.cpp",
    "yoga/event/event.cpp",
    "yoga/node/LayoutResults.cpp",
    "yoga/node/Node.cpp",
];

fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

fn main() {
    let manifest = PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").unwrap());
    println!("cargo:rerun-if-env-changed=OPENCODE_OPENTUI_NATIVE");
    println!("cargo:rerun-if-env-changed=CXX");
    let native = match std::env::var_os("OPENCODE_OPENTUI_NATIVE") {
        Some(dir) => PathBuf::from(dir),
        None => manifest.join("../../../.vendor/opentui/packages/native"),
    };
    let yoga = native.join("zig-deps/yoga");
    if !yoga.join("yoga/Yoga.h").is_file() {
        panic!(
            "Yoga's sources are missing from {}: run port/scripts/vendor-opentui.sh (or set OPENCODE_OPENTUI_NATIVE)",
            yoga.display()
        );
    }
    let bridge = native.join("src/yoga-bridge.cpp");
    let logger = manifest.join("src/logger.cpp");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap();

    let mut build = cc::Build::new();
    build.cpp(true);
    if std::env::var_os("CXX").is_none() && target_os != "macos" && on_path("clang++") {
        build.compiler("clang++");
    }
    build
        .include(&yoga)
        .flag("-std=c++20")
        .flag("-fexceptions")
        .flag("-frtti")
        .define("NDEBUG", None)
        .opt_level(2)
        .warnings(false);
    if target_os == "macos" {
        build.define("FLT_MAX", "__FLT_MAX__");
    }
    for source in YOGA_CXX_SOURCES {
        build.file(yoga.join(source));
    }
    build.file(&bridge).file(&logger);
    build.compile("opentui-yoga");

    for path in [yoga.join("yoga").as_path(), bridge.as_path(), native.join("src/yoga-bridge.h").as_path(), &logger] {
        rerun(path);
    }
    rerun(&native.join("src/yoga-vocabulary.h"));
    rerun(&native.join("include/opentui.h"));
}

fn rerun(path: &Path) {
    println!("cargo:rerun-if-changed={}", path.display());
}
