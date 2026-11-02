//! `GET /_stats`: what each URL was asked and sent; `POST /_reset` forgets it.

use std::sync::atomic::Ordering;
use std::sync::{Arc, PoisonError};

use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;

use crate::server::AppState;

#[derive(Serialize)]
pub(crate) struct UrlStats {
    url: String,
    gets: u64,
    active: u32,
    peak_active: u32,
    /// Requests turned away by `max_conns`.
    refused: u64,
    bytes_sent: u64,
    /// Requests that arrived over HTTP/2.
    h2_gets: u64,
    /// TCP connections the requests came on; 0 when the server has no connect info.
    connections: usize,
    user_agent: Option<String>,
}

pub(crate) async fn stats(State(app): State<Arc<AppState>>) -> axum::Json<Vec<UrlStats>> {
    let urls = app.urls.lock().unwrap_or_else(PoisonError::into_inner);
    let mut out: Vec<UrlStats> = urls
        .iter()
        .map(|(url, s)| UrlStats {
            url: url.clone(),
            gets: s.gets.load(Ordering::Relaxed),
            active: s.active.load(Ordering::Relaxed),
            peak_active: s.peak_active.load(Ordering::Relaxed),
            refused: s.refused.load(Ordering::Relaxed),
            bytes_sent: s.bytes_sent.load(Ordering::Relaxed),
            h2_gets: s.h2_gets.load(Ordering::Relaxed),
            connections: s.peers.lock().unwrap_or_else(PoisonError::into_inner).len(),
            user_agent: s
                .user_agent
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
        })
        .collect();
    out.sort_by(|a, b| a.url.cmp(&b.url));
    axum::Json(out)
}

pub(crate) async fn reset(State(app): State<Arc<AppState>>) -> StatusCode {
    app.urls
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    StatusCode::NO_CONTENT
}
