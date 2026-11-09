// TODO(port-tui): opencode-yoga. Until the input slice's crate lands, Yoga 3.2.1 and OpenTUI's bridge are compiled
// here from the vendored sources (port/.vendor/opentui, port/scripts/vendor-opentui.sh) with build.zig's files and
// flags; src/layout.rs wraps them.
use std::path::PathBuf;

const SOURCES: &[&str] = &[
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

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let native = manifest.join("../../../.vendor/opentui/packages/native");
    let yoga = native.join("zig-deps/yoga");
    if !yoga.exists() {
        panic!("Yoga sources missing at {}: run port/scripts/vendor-opentui.sh", yoga.display());
    }
    let mut build = cc::Build::new();
    build.cpp(true).include(&yoga).include(native.join("include")).warnings(false);
    for flag in ["-std=c++20", "-fexceptions", "-frtti"] {
        build.flag(flag);
    }
    for source in SOURCES {
        build.file(yoga.join(source));
        println!("cargo:rerun-if-changed={}", yoga.join(source).display());
    }
    build.file(native.join("src/yoga-bridge.cpp"));
    build.compile("opentui_yoga");
}
