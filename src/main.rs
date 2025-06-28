mod random;

use std::ffi::OsStr;
use std::fs;
use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    extract::{Path, State},
    http::{StatusCode, header},
    response::{Html, Response},
    routing::get,
};
use minijinja::{Environment, context};
use resvg::{
    tiny_skia,
    usvg::{self, Transform},
};
use serde::Deserialize;
use tokio::{net::TcpListener, sync::RwLock};

const API_DOCS_PATH: &str = "api_docs.html";
const CONFIG_PATH: &str = "no-face.toml";
const STYLES_PATH: &str = "styles";

#[derive(Debug, Default, Clone, Deserialize)]
struct StartupConfig {
    bind: Option<SocketAddr>,
}

#[derive(Debug, Default, Clone, Deserialize)]
struct Config {
    api_root: Option<String>,
    max_raster_size: Option<u64>,
    startup: StartupConfig,
}

#[derive(Default, Clone)]
struct ApiState {
    config: Config,
    jinja_env: Environment<'static>,
    styles: Vec<String>,
}

type StateExtractor = State<Arc<RwLock<ApiState>>>;

#[derive(Debug, Clone, Copy, PartialEq)]
enum ImageFormat {
    Svg,
    Png,
}

fn load_config(state: &mut ApiState) {
    let Ok(config_string) = fs::read_to_string(CONFIG_PATH) else {
        println!("Unable to read no-face config!");
        return;
    };
    let Ok(new_config) = toml::from_str(&config_string) else {
        println!("Unable to parse no-face config!");
        return;
    };
    state.config = new_config;
}

fn load_docs(state: &mut ApiState) {
    state
        .jinja_env
        .add_template(API_DOCS_PATH, include_str!("../api_docs.html"))
        .unwrap();
}

fn load_styles(state: &mut ApiState) {
    let Ok(dir) = fs::read_dir(STYLES_PATH) else {
        println!("Unable to read styles directory!");
        return;
    };

    for entry in dir {
        // Any failures in this loop should continue, not return.
        // That way it loads as many styles as possible.
        let Ok(entry) = entry else {
            continue;
        };

        let path = entry.path();
        if path.extension() != Some(OsStr::new("svg")) {
            continue;
        }

        let Some(stem) = path.file_stem() else {
            continue;
        };
        let name = stem.to_string_lossy().into_owned();

        let Ok(template_string) = fs::read_to_string(&path) else {
            println!("Unable to read style {}!", &name);
            continue;
        };
        if let Err(e) = state
            .jinja_env
            .add_template_owned(name.clone(), template_string)
        {
            println!("Unable to add style {}!\n{e}", &name);
        }

        // Hiding symlinks allows them to be used as a sort of compatibility alias.
        // Not tested on Windows.
        if !path.is_symlink() {
            state.styles.push(name);
        }
    }
    state.styles.sort();
}

fn load_data(state: &mut ApiState) {
    load_config(state);

    state.jinja_env.clear_templates();
    state.styles.clear();

    load_docs(state);
    load_styles(state);
}

#[cfg(target_family = "unix")]
fn start_reload_listener(state: Arc<RwLock<ApiState>>) {
    use tokio::signal::unix::{SignalKind, signal};

    tokio::spawn(async move {
        let mut signal = signal(SignalKind::hangup()).unwrap();
        loop {
            signal.recv().await;
            println!("Received SIGHUP, reloading config.");
            // I think this could get stuck under high load.
            // If for some strange reason this API is ever under high load, rethink it.
            load_data(&mut *state.write().await);
            println!("Config reloaded.");
        }
    });
}

#[cfg(not(target_family = "unix"))]
fn start_reload_listener(_: Arc<RwLock<ApiState>>) {
    // This really just means Windows. It's not a critical feature anyway.
    println!("Warning: live config reloading is not supported on the current platform.");
}

fn register_functions(env: &mut Environment) {
    env.add_function("sin", f64::sin);
    env.add_function("cos", f64::cos);
    env.add_function("tan", f64::tan);
    env.add_function("asin", f64::asin);
    env.add_function("acos", f64::acos);
    env.add_function("atan", f64::atan);
    env.add_function("random", random::number);
    env.add_function("random_color", random::color);
    env.add_function("inverted_color", random::inverted_color);
}

#[tokio::main]
async fn main() {
    let mut api_state = ApiState::default();
    api_state.jinja_env = Environment::new();
    load_data(&mut api_state);
    register_functions(&mut api_state.jinja_env);

    let bind_address = api_state
        .config
        .startup
        .bind
        .unwrap_or("0.0.0.0:8080".parse().unwrap());

    let api_state = Arc::new(RwLock::new(api_state));
    start_reload_listener(Arc::clone(&api_state));

    let app = Router::new()
        .route("/", get(docs_handler))
        .route("/{style}/{size}/{id}", get(avatar_handler))
        .with_state(api_state);

    let listener = TcpListener::bind(bind_address).await.unwrap();
    println!("Listening on {bind_address}");
    axum::serve(listener, app).await.unwrap();
}

async fn docs_handler(State(state): StateExtractor) -> Html<String> {
    let state = state.read().await;

    let jinja_env = &state.jinja_env;
    let template = jinja_env.get_template(API_DOCS_PATH).unwrap();
    Html(
        template
            .render(context! {
                styles => state.styles,
                api_root => state.config.api_root,
                max_size => state.config.max_raster_size,
            })
            .unwrap(),
    )
}

async fn avatar_handler(
    State(state): StateExtractor,
    Path((style, size, id)): Path<(String, u64, String)>,
) -> Response<Body> {
    let state = state.read().await;
    let not_found = Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body("Not Found".into())
        .unwrap();

    // SVG seems like a good default, both for compatibility with avatars.soxfox.me, and because it skips a whole rasterise step!
    let (id, ext) = id.rsplit_once('.').unwrap_or((&id, "svg"));
    let format = match ext {
        "svg" => ImageFormat::Svg,
        "png" => ImageFormat::Png,
        _ => return not_found,
    };

    // Size limit only matters if we're the ones rasterising.
    if format != ImageFormat::Svg && size > state.config.max_raster_size.unwrap_or(1024) {
        return not_found;
    }

    let jinja_env = &state.jinja_env;
    let Ok(template) = jinja_env.get_template(&style) else {
        return not_found;
    };

    let context = context! { style, size, name => id };
    let rendered = match template.render(context) {
        Ok(rendered) => rendered,
        Err(e) => {
            println!("Error rendering {style}: {e}");
            return not_found;
        }
    };

    match format {
        ImageFormat::Svg => Response::builder()
            .status(StatusCode::OK)
            .header(header::CONTENT_TYPE, "image/svg+xml")
            .body(rendered.into())
            .unwrap(),
        ImageFormat::Png => match render_to_png(&rendered, size) {
            Ok(png_data) => Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, "image/png")
                .body(png_data.into())
                .unwrap(),
            Err(e) => {
                println!("Error rasterising {style}: {e}");
                not_found
            }
        },
    }
}

fn render_to_png(rendered: &str, size: u64) -> Result<Vec<u8>, String> {
    let mut options = usvg::Options::default();
    options.fontdb_mut().load_system_fonts();
    let tree =
        usvg::Tree::from_data(rendered.as_bytes(), &options).map_err(|_| "Error parsing SVG")?;
    let mut pixmap =
        tiny_skia::Pixmap::new(size as u32, size as u32).ok_or("Error creating pixmap")?;
    resvg::render(&tree, Transform::identity(), &mut pixmap.as_mut());
    pixmap.encode_png().map_err(|_| "Error encoding PNG".into())
}
