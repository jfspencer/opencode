# opencode-yoga

The TUI's layout engine: OpenTUI 0.8.1's Yoga 3.2.1 (C++, with OpenTUI's patches) compiled from
`port/.vendor/opentui/packages/native/zig-deps/yoga`, behind a safe Rust port of OpenTUI's two wrappers.

| Module | OpenTUI 0.8.1 |
|---|---|
| `build.rs` | `packages/native/build.zig` `addYogaDependencies`: the same 19 Yoga sources plus `src/yoga-bridge.cpp`, the Yoga include root, `-std=c++20 -fexceptions -frtti`, `-DFLT_MAX=__FLT_MAX__` on macOS, and ReleaseFast's `-DNDEBUG -O2` in every cargo profile; clang++ when `CXX` is unset and it is on the path (Zig compiles C++ with clang). `src/logger.cpp` adds the `YGLogger` trampoline |
| `sys` | the Yoga C API and the bridge (`otYoga*`, `yoga-bridge.h`) |
| `Node`, `Config` | `packages/native/src/yoga.zig` (the checked exports and statuses, poisoning, the depth limit, native measure functions, `move_child`, `style_set_dimension`, `style_set_positions`, `invalidate_measure`, `forget_measurements`) and `packages/core/src/yoga.ts` (the public classes: typed setters, `ValueInput`, defaults on a freed node, `YogaError` texts) |
| `value` | `parseYogaValue` (`yoga.internal.ts`), with `Number.parseFloat` |
| `options` | `lib/yoga.options.ts`: the string forms of the enums Renderable options take |

The sources are read from `port/.vendor/opentui` (`port/scripts/vendor-opentui.sh` fetches it; `OPENCODE_OPENTUI_NATIVE`
names another `packages/native`). The C++ standard library is linked dynamically (libstdc++ on Linux, libc++ on
macOS), as the `cc` crate does by default.

## Using it

Methods are the TS ones in snake_case. yoga.ts names (`set_width`, `insert_child`, `calculate_layout`, ...) take the
typed enums and TS values; yoga.zig names (`style_set_enum`, `style_set_value`, `move_child`,
`calculate_layout_checked`, `set_native_measure_func`, ...) take the native ABI's raw `u32` kinds, edges and units, as
`scene.zig` calls them. Every Yoga call returns `Result<_, Error>`; `Error`'s `Display` is the TS message
(`yogaNodeInsertChildChecked failed: InvalidArgument (status 1)`, `Yoga node is freed`, `Invalid Yoga value: 12`).

```rust
use opencode_yoga::{Config, Direction, FlexDirection, MeasureMode, Node, Size};

// The renderer's context config (context.zig): upstream defaults plus absolute-edge rounding.
let config = Config::create()?;
config.set_rounds_positions_from_absolute_edges(true)?;
let root = Node::create(Some(&config))?;
root.set_flex_direction(FlexDirection::Row)?;
root.set_width("100%")?;
let text = Node::create(Some(&config))?;
text.set_flex_shrink(Some(1.0))?; // scene nodes: flex shrink 1, zero borders
root.insert_child(&text, 0)?;
// A scene text node: native measurement, the height cap only clamps.
text.set_native_measure_func(Some(Box::new(|width, mode, _, _| {
    let w = if mode == MeasureMode::Undefined { 30.0 } else { width.min(30.0) };
    Size { width: w, height: (30.0 / w).ceil() }
})), true)?;
root.calculate_layout(Some(80.0), Some(24.0), Direction::LTR)?;
let layout = text.get_computed_layout()?;
```

A `Node` is a reference-counted handle; the Yoga node is freed by `free()` or with its last handle, and a parent
keeps its children alive (dropping a root frees its tree; children held elsewhere become roots). A `Config` lives
while its nodes do. As in TS, a Yoga mutation inside a measure, dirtied or log callback fails (`Error::InCallback`),
a callback's panic resumes when the outermost Yoga call returns, and a node dropped inside a callback is freed after
that call. Configs made by `Config::create` keep Yoga's default logger (stdout) until `set_logger`; the renderer's
config should route it, as `context.zig` routes it to its diagnostics.

## Differential check

```sh
port/rust/crates/opencode-yoga/harness/run.sh                         # unit tests, 12 hand-picked + 20000 random programs
port/rust/crates/opencode-yoga/harness/run.sh --count 50000 --seed 2  # a deeper run
```

`harness/oracle.ts` generates programs (random trees with random styles in both APIs, some invalid; JS and native
measure functions with logged calls; dirtied functions; mutations, moves, frees, resets, config changes and
relayouts) and runs each op through the reference's own `libopentui.so` (`@opentui/core/yoga`, plus the native
exports only the scene calls); `examples/yoga-driver.rs` replays them through this crate. Every answer is compared:
statuses and error texts, every node's computed layout, margins, paddings and borders as f32 bits, dirty and new-layout
flags, style getters, measure calls with their arguments, dirtied calls, and what Yoga's default logger printed.

## Licences

Yoga: MIT, Copyright (c) Meta Platforms, Inc. and affiliates (`zig-deps/yoga/LICENSE`). OpenTUI (its Yoga patches,
`yoga-bridge.cpp`, and the wrappers ported here): MIT, Copyright (c) 2025 opentui (`port/.vendor/opentui/LICENSE`).
