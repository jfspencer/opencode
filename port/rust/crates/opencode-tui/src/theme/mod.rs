//! The TUI's themes: `@opencode/theme` (`packages/theme/src/tui`) and `packages/tui/src/theme`, plus the
//! non-reactive logic of `packages/tui/src/context/theme.tsx`.
//!
//! - [`registry::ThemeRegistry`]: the built-in (V1 files and the v2 `opencode` document), custom and system themes
//!   (`allThemes`, `setCustomThemes`, `setSystemTheme`, `hasTheme`), `parseTheme`, `loadTheme` and the theme
//!   context's `selected` fallback ([`registry::ThemeRegistry::select`]).
//! - [`resolve`]: `parseThemeDocument` (Effect decoding, [`schema`]) and `resolveThemeDocument`, with [`expand`] and
//!   [`select`] (modes).
//! - [`resolved::ResolvedTheme`]: the token tree components read (`theme.text.muted`, `theme.background.raised.base`,
//!   `theme.hue.accent[200]`, `theme.surface(SurfaceName::Dialog)`, `theme.decrease(color, 1.0)`).
//! - [`v1`]: V1 theme resolution and the two migrations to documents.
//! - [`system`], [`discovery`], [`syntax`], [`color`] (`tint`, ANSI and OKLCH conversions).
//!
//! The reactive wiring of context/theme.tsx (the store of `mode`/`lock`/`active`/`ready`, effects on config changes,
//! palette probing, SIGUSR2 refresh, `createComponentTheme`, `ThemeContextProvider`) is the app shell's; it composes
//! these functions. Colors are [`value::Color`]: an `RGBA` plus the hue-scale identity `increase`/`decrease` need.
pub mod color;
pub mod discovery;
pub mod expand;
pub(crate) mod js;
pub mod registry;
pub mod resolve;
pub mod resolved;
pub mod rgba;
pub mod schema;
pub mod select;
pub mod syntax;
pub mod system;
pub mod v1;
pub mod value;

pub use registry::{LoadedTheme, Selected, ThemeRegistry, ThemeSource};
pub use resolve::{ThemeError, parse_theme_document, resolve_theme_document};
pub use resolved::{ActionStates, HueScale, ResolvedTheme, StatefulColor, SurfaceName, Tokens};
pub use rgba::RGBA;
pub use select::Mode;
pub use value::Color;
