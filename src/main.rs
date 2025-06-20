use std::{net::IpAddr, str::FromStr};

use opentelemetry::{
    global,
    trace::{get_active_span, FutureExt, Span, Status, TraceContextExt, Tracer},
    Context, KeyValue,
};

use axum::extract::State;
use axum::{
    body::BoxBody,
    http::{header, HeaderMap},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};
use locat::Locat;
use reqwest::StatusCode;
use std::sync::Arc;
use tracing::{info, warn, Level};
use tracing_subscriber::{filter::Targets, layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Clone)]
struct ServerState {
    client: reqwest::Client,
    locat: Arc<Locat>,
}

#[tokio::main]
async fn main() {
    let (_honeyguard, _tracer) = opentelemetry_honeycomb::new_pipeline(
        std::env::var("HONEYCOMB_API_KEY").expect("$HONEYCOMB_API_KEY should be set"),
        "catscii".into(),
    )
    .install()
    .unwrap();

    let filter = Targets::from_str(std::env::var("RUST_LOG").as_deref().unwrap_or("info"))
        .expect("RUST_LOG should be a valid tracing filter");
    tracing_subscriber::fmt()
        .with_max_level(Level::TRACE)
        .json()
        .finish()
        .with(filter)
        .init();

    let country_db_env_var = "GEOLITE2_COUNTRY_DB";
    let country_db_path = std::env::var(country_db_env_var)
        .unwrap_or_else(|_| panic!("${country_db_env_var} must be set"));
    println!("{country_db_path}");

    let analytics_db_env_var = "ANALYTICS_DB";
    let analytics_db_path = std::env::var(analytics_db_env_var)
        .unwrap_or_else(|_| panic!("${analytics_db_env_var} must be set"));
    println!("{analytics_db_path}");

    let state = ServerState {
        client: Default::default(),
        locat: Arc::new(Locat::new(&country_db_path, &analytics_db_path).unwrap()),
    };

    let app = Router::new()
        .route("/", get(root_get))
        .route("/analytics", get(analytics_get))
        .route("/panic", get(|| async { panic!("This is a test panic") }))
        .with_state(state);

    let quit_sig = async {
        _ = tokio::signal::ctrl_c().await;
        warn!("Initiating graceful shutdown");
    };

    let addr = "0.0.0.0:8080".parse().unwrap();
    info!("Listening on {addr}");
    axum::Server::bind(&addr)
        .serve(app.into_make_service())
        .with_graceful_shutdown(quit_sig)
        .await
        .unwrap();
}

async fn analytics_get(State(state): State<ServerState>) -> Response<BoxBody> {
    let analytics = state.locat.get_analytics().await.unwrap();
    let mut response = String::new();
    use std::fmt::Write;
    for (country, count) in analytics {
        _ = writeln!(&mut response, "{country}: {count}");
    }
    response.into_response()
}

fn get_client_addr(headers: &HeaderMap) -> Option<IpAddr> {
    let header = headers.get("fly-client-ip")?;
    let header = header.to_str().ok()?;
    let addr = header.parse::<IpAddr>().ok()?;
    Some(addr)
}

async fn root_get(headers: HeaderMap, State(state): State<ServerState>) -> Response<BoxBody> {
    let tracer = global::tracer("");
    let mut span = tracer.start("root_get");
    span.set_attribute(KeyValue::new(
        "user_agent",
        headers
            .get(header::USER_AGENT)
            .map(|h| h.to_str().unwrap_or_default().to_owned())
            .unwrap_or_default(),
    ));

    if let Some(addr) = get_client_addr(&headers) {
        match state.locat.ip_to_iso_code(addr).await {
            Some(country) => {
                info!("Got request from {country}");
                span.set_attribute(KeyValue::new("country", country.to_string()));
            }
            None => warn!("Could not determine country for IP address"),
        }
    }

    root_get_inner(state)
        .with_context(Context::current_with_span(span))
        .await
}
//               to here 👇
async fn root_get_inner(state: ServerState) -> Response<BoxBody> {
    let tracer = global::tracer("");

    //       passing the client 👇
    match get_cat_ascii_art(&state.client)
        .with_context(Context::current_with_span(
            tracer.start("get_cat_ascii_art"),
        ))
        .await
    {
        Ok(art) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            art,
        )
            .into_response(),
        Err(e) => {
            get_active_span(|span| {
                span.set_status(Status::Error {
                    description: format!("{e}").into(),
                })
            });
            (StatusCode::INTERNAL_SERVER_ERROR, "Something went wrong").into_response()
        }
    }
}

//                   to here 👇
async fn get_cat_ascii_art(client: &reqwest::Client) -> color_eyre::Result<String> {
    let tracer = global::tracer("");

    //   and then our helper functions 👇
    let image_url = get_cat_image_url(client)
        .with_context(Context::current_with_span(
            tracer.start("get_cat_image_url"),
        ))
        .await?;

    let image_bytes = download_file(client, &image_url)
        .with_context(Context::current_with_span(tracer.start("download_file")))
        .await?;

    let image = tracer.in_span("image::load_from_memory", |cx| {
        let img = image::load_from_memory(&image_bytes)?;
        cx.span()
            .set_attribute(KeyValue::new("width", img.width() as i64));
        cx.span()
            .set_attribute(KeyValue::new("height", img.height() as i64));
        Ok::<_, color_eyre::eyre::Report>(img)
    })?;

    let ascii_art = tracer.in_span("artem::convert", |_cx| {
        artem::convert(
            image,
            artem::options::OptionBuilder::new()
                .target(artem::options::TargetType::HtmlFile(true, true))
                .build(),
        )
    });

    Ok(ascii_art)
}

async fn get_cat_image_url(client: &reqwest::Client) -> color_eyre::Result<String> {
    #[derive(serde::Deserialize)]
    struct CatImage {
        url: String,
    }

    let api_url = "https://api.thecatapi.com/v1/images/search";

    let image = client
        .get(api_url)
        .send()
        .await?
        .error_for_status()?
        .json::<Vec<CatImage>>()
        .await?
        .pop()
        .ok_or_else(|| color_eyre::eyre::eyre!("The Cat API returned no images"))?;
    Ok(image.url)
}

async fn download_file(client: &reqwest::Client, url: &str) -> color_eyre::Result<Vec<u8>> {
    let bytes = client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_db() {
        let country_db_env_var = "GEOLITE2_COUNTRY_DB";
        let country_db_path = std::env::var(country_db_env_var)
            .unwrap_or_else(|_| panic!("${country_db_env_var} must be set"));

        let analytics_db_env_var = "ANALYTICS_DB";
        let analytics_db_path = std::env::var(analytics_db_env_var)
            .unwrap_or_else(|_| panic!("${analytics_db_env_var} must be set"));
        let _locat = Locat::new(&country_db_path, &analytics_db_path).unwrap();

        println!("{country_db_path:?}");
        println!("{analytics_db_path:?}");
    }
}
