use std::sync::mpsc::channel;

use reqwest::get;
use tokio::runtime::Builder;

use mate_job::mate_handler;

#[mate_handler]
fn execute(_: String) -> String {
    let response = get("https://google.com").unwrap();
    let body = response.text().unwrap();
    body
}
