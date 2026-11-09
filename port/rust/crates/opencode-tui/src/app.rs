//! `run(input)` and `<App>` (`packages/tui/src/app.tsx`): the config, the client and the location, the renderer
//! with the TUI's options, the theme mode, the providers, the routes, and the exit.
//!
//! TODO(port-tui): of App's commands, keybinds, toasts, dialogs, session tabs, panels, plugins, the update
//! notification, the attention and terminal-title effects, only what the home screen shows is here so far.
use std::cell::RefCell;
use std::path::PathBuf;
use std::rc::Rc;

use opencode_tui_core::layout::Value;
use opencode_tui_core::reactive::*;
use opencode_tui_core::renderer::{self, RendererConfig};
use opencode_tui_core::solid::*;
use opencode_tui_core::terminal::ThemeMode;
use opencode_tui_core::tree::{self, FlexDirection};
use opencode_tui_sync::client::{Client, ClientOptions};
use opencode_tui_sync::transport::HttpTransport;

use crate::ctx::{AppInfo, Args, Paths, Route, Tui, context, theme_signal};
use crate::data::Data;
use crate::theme::{Mode, ThemeRegistry};
use crate::util::rgba;

/// What `opencode` passes the TUI (`TuiInput`).
pub struct TuiInput {
    pub app: AppInfo,
    /// The server's URL and the headers every request carries (its credentials).
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub args: Args,
    /// The server runs on this machine (no explicit `--server`).
    pub local: bool,
}

fn xdg(variable: &str, fallback: &str) -> PathBuf {
    std::env::var(variable)
        .ok()
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(fallback))
        .join("opencode")
}

/// `Tui.run(input)`: runs until the TUI exits; returns the exit code.
pub async fn run(input: TuiInput) -> i32 {
    let home = std::env::var("HOME").unwrap_or_default();
    let config_dir = xdg("XDG_CONFIG_HOME", ".config");
    let state_dir = xdg("XDG_STATE_HOME", ".local/state");
    let cli_config = crate::config::CliConfig::new(&config_dir, &state_dir);
    let info = tokio::task::spawn_blocking(move || cli_config.get()).await.unwrap_or_default();
    let resolved = match crate::config::resolve(&info, &crate::config::ResolveOptions::default()) {
        Ok(resolved) => resolved,
        Err(message) => {
            eprintln!("{message}");
            return 1;
        }
    };
    let client = Client::make(ClientOptions { base_url: input.url.clone(), headers: input.headers.clone(), transport: Rc::new(HttpTransport) });
    let data = Data::new(client);
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned()).unwrap_or_default();
    data.load_location(&cwd).await;

    if let Err(error) = renderer::create_cli_renderer(RendererConfig { use_mouse: resolved.mouse, ..RendererConfig::default() }).await {
        eprintln!("{error}");
        return 1;
    }
    let mode = match renderer::wait_for_theme_mode(1000).await {
        Some(ThemeMode::Light) => Mode::Light,
        _ => Mode::Dark,
    };
    if renderer::is_destroyed() {
        return 0;
    }
    let mut registry = ThemeRegistry::new();
    let name = resolved.theme.as_ref().and_then(|t| t.name.clone()).unwrap_or_else(|| "opencode".to_string());
    let new_designs = resolved.new_designs();
    let selected = match registry.select(&name, mode, new_designs) {
        Ok(selected) => selected,
        Err(_) => match registry.select("opencode", mode, new_designs) {
            Ok(selected) => selected,
            Err(error) => {
                renderer::destroy();
                renderer::closed().await;
                eprintln!("{error:?}");
                return 1;
            }
        },
    };
    let app = Rc::new(input.app);
    let args = Rc::new(input.args);
    let paths = Rc::new(Paths { cwd, home, state: state_dir.to_string_lossy().into_owned() });
    let resolved = Rc::new(resolved);
    let root = tree::root().expect("the renderer's root");
    let theme = selected.loaded.theme;
    let data_for_load = data.clone();
    let _dispose = create_root(move |_dispose| {
        let dimensions = create_signal((renderer::width(), renderer::height())).0;
        renderer::on_resize(move |w, h| dimensions.set((w, h)));
        let t = Tui {
            app,
            args,
            paths,
            route: create_signal(Route::Home).0,
            theme: theme_signal(theme),
            dimensions,
            data,
            config: resolved,
            prompt: Rc::new(RefCell::new(None)),
            exit: Rc::new(|| renderer::destroy()),
        };
        provide_context(context(), t.clone());
        crate::keymap::install(&t);
        let view = component(app_view);
        insert(root, view, At::Only);
    });
    tokio::task::spawn_local(data_for_load.load_location_data());
    renderer::request_render();
    renderer::closed().await;
    0
}

/// `<App>`.
fn app_view() -> View {
    let t = crate::ctx::tui();
    let dimensions = t.dimensions;
    let theme = t.theme;
    let route = t.route;
    let root = opencode_tui_core::elements::box_();
    bind(move || dimensions.get(), move |(w, h)| {
        root.set_width(Some(Value::Point(w as f32)));
        root.set_height(Some(Value::Point(h as f32)));
    });
    root.set_flex_direction(FlexDirection::Column);
    bind_always(move || rgba(theme.with(|th| th.background.base)), move |c| root.set_background_color(Some(c)));
    let main = opencode_tui_core::elements::box_();
    insert_node(root, main, None);
    main.set_flex_grow(Some(1.0));
    main.set_min_height(Some(Value::Point(0.0)));
    main.set_flex_direction(FlexDirection::Row);
    main.set_position_type(false);
    let column = opencode_tui_core::elements::box_();
    insert_node(main, column, None);
    column.set_flex_grow(Some(1.0));
    column.set_min_width(Some(Value::Point(0.0)));
    column.set_flex_direction(FlexDirection::Column);
    let routes = opencode_tui_core::elements::box_();
    insert_node(column, routes, None);
    routes.set_flex_grow(Some(1.0));
    routes.set_min_height(Some(Value::Point(0.0)));
    routes.set_flex_direction(FlexDirection::Column);
    insert(
        routes,
        switch(
            vec![
                when(move || route.with(|r| *r == Route::Home), || component(crate::home::home)),
                when(move || route.with(|r| matches!(r, Route::Session { .. })), || View::Empty),
            ],
            None,
        ),
        At::Only,
    );
    View::Node(root)
}
