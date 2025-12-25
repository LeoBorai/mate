use std::io::{self, Read};

use anyhow::Result;
use wstd::http::{Body, BodyExt, Client, Method, Request};

use mate_job::{mate_handler, mate_object};

#[mate_object]
struct Config {
    api_url: String,
    data: serde_json::Value,
}

#[mate_handler]
async fn send_http_request() -> Result<()> {
    let mut json_input = String::new();
    io::stdin().read_to_string(&mut json_input)?;

    let config: Config = serde_json::from_str(&json_input)?;
    let client = Client::new();
    let mut request = Request::builder();
    request = request.uri(config.api_url).method(Method::POST);

    let body = Body::from_json(&config.data).expect("Bad body");
    let request = request.body(body)?;
    let response = client.send(request).await?;
    let body = response.into_body().into_boxed_body().collect().await?;
    let bytes = body.to_bytes();
    let json = serde_json::from_slice::<serde_json::Value>(&bytes)?;

    println!("POST Response: {}", json);

    Ok(())
}
