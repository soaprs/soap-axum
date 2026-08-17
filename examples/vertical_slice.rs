//! Runnable request → RouteIo → UseCase → RouteIo → response example.

use std::sync::Arc;

use http::{Method, StatusCode};
use serde::{Deserialize, Serialize};
use soaprs_axum::{EndpointBinding, JsonResponse, RouteRequest, SoapRouter, TypedJsonRouteIo};
use soaprs_core::{BoxFuture, SoapResult, UseCase};
use soaprs_http::{EndpointCatalog, EndpointMetadata, RoutePath};

#[derive(Deserialize)]
struct GreetingPath {
    language: String,
}

#[derive(Deserialize)]
struct GreetingBody {
    name: String,
}

struct GreetInput {
    name: String,
    language: String,
}

struct GreetOutput {
    message: String,
}

#[derive(Serialize)]
struct GreetingResponse {
    message: String,
}

struct Greet;

impl UseCase for Greet {
    type Input = GreetInput;
    type Output = GreetOutput;

    fn execute(&self, input: Self::Input) -> BoxFuture<'_, SoapResult<Self::Output>> {
        Box::pin(async move {
            let greeting = if input.language == "pl" {
                "Cześć"
            } else {
                "Hello"
            };
            Ok(GreetOutput {
                message: format!("{greeting}, {}!", input.name),
            })
        })
    }
}

fn application() -> SoapResult<axum::Router> {
    let endpoint = EndpointMetadata::new(
        "greetings.create",
        Method::POST,
        RoutePath::new("/greetings/{language}")?,
    )?
    .success_status(StatusCode::CREATED)?;
    let mut catalog = EndpointCatalog::new();
    catalog.register(endpoint)?;

    let route_io = TypedJsonRouteIo::new(
        |request: &RouteRequest, body: GreetingBody| {
            let path: GreetingPath = request.decode_path()?;
            Ok(GreetInput {
                name: body.name.trim().to_owned(),
                language: path.language,
            })
        },
        |output: GreetOutput, _endpoint: &EndpointMetadata| {
            Ok(JsonResponse::new(GreetingResponse {
                message: output.message,
            }))
        },
    );
    let binding = EndpointBinding::use_case(Arc::new(Greet)).route_io(route_io);
    SoapRouter::builder(catalog)
        .bind("greetings.create", binding)?
        .build()
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = application()?;
    if std::env::args_os().any(|argument| argument == "--check") {
        println!("vertical slice router built successfully");
        return Ok(());
    }

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000").await?;
    println!("POST http://127.0.0.1:3000/greetings/pl with JSON {{\"name\":\"Ada\"}}");
    axum::serve(listener, app).await?;
    Ok(())
}
