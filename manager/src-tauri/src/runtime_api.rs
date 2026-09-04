//! The two read-only calls this window makes against the runtime's HTTP API.
//!
//! Both answer while the runtime is DOWN, because down is the normal state
//! before provisioning has ever run: a panel whose first render throws would make a
//! fresh install look broken. There is no privileged path here either — this app
//! is a client of the same API an agent or the CLI uses, with the same bearer
//! token read from the same file.

use serde_json::Value;
use tauri::{AppHandle, Manager};

use crate::config_edit;
use crate::contract::{Pack, Status};
use crate::host::Host;

#[tauri::command]
pub async fn runtime_status(app: AppHandle) -> Status {
    let host = app.state::<Host>();
    let Some(token) = host.token() else {
        return Status {
            reachable: false,
            error: Some(
                "no token yet: data\\token.txt appears the first time the runtime starts"
                    .to_string(),
            ),
            body: None,
        };
    };

    let url = format!("{}/api/status", host.base_url);
    match host.http.get(url).bearer_auth(token).send().await {
        Ok(response) if response.status().is_success() => match response.json::<Value>().await {
            Ok(body) => Status {
                reachable: true,
                error: None,
                body: Some(body),
            },
            Err(err) => Status {
                reachable: true,
                error: Some(format!("status body was unreadable: {err}")),
                body: None,
            },
        },
        // It answered, just not with a status. 401 here means the token on disk is
        // not the token the running runtime minted — a real situation after a data
        // dir has been moved, and one no retry will fix.
        Ok(response) => {
            let code = response.status().as_u16();
            let detail = response.text().await.unwrap_or_default();
            let detail = detail.trim();
            Status {
                reachable: true,
                error: Some(if detail.is_empty() {
                    format!("runtime answered HTTP {code}")
                } else {
                    format!("runtime answered HTTP {code}: {detail}")
                }),
                body: None,
            }
        }
        Err(err) => Status {
            reachable: false,
            error: Some(reason(&host, err)),
            body: None,
        },
    }
}

/// The runtime's voice list, or the registry file when the runtime is down.
///
/// The fallback is not a guess: `config.json`'s `voicePacks` is exactly what the
/// runtime serves from, and it is the file this app writes, so a pack shows up in
/// the panel the moment it is registered rather than after a backend start.
#[tauri::command]
pub async fn list_voices(app: AppHandle) -> Vec<Pack> {
    let host = app.state::<Host>();
    if let Some(token) = host.token() {
        let url = format!("{}/api/voices", host.base_url);
        if let Ok(response) = host.http.get(url).bearer_auth(token).send().await {
            if response.status().is_success() {
                if let Ok(items) = response.json::<Vec<Value>>().await {
                    // Per item, so one hand-edited entry cannot empty the list.
                    return items
                        .into_iter()
                        .filter_map(|item| serde_json::from_value(item).ok())
                        .collect();
                }
            }
        }
    }
    config_edit::read_packs(&host)
}

/// A sentence a user can act on.
///
/// reqwest's own message is mostly the URL and its causes are nested three deep;
/// the innermost one is the OS text ("No connection could be made because the
/// target machine actively refused it"), and for the two cases that matter here
/// even that is more than the panel needs.
fn reason(host: &Host, err: reqwest::Error) -> String {
    if err.is_connect() {
        return format!("runtime is not listening on {}", host.base_url);
    }
    if err.is_timeout() {
        return format!("runtime did not answer in time on {}", host.base_url);
    }
    // In scope for `source()` on the `dyn Error` links of the chain.
    use std::error::Error as _;
    let mut message = err.to_string();
    let mut source = err.source();
    while let Some(inner) = source {
        message = inner.to_string();
        source = inner.source();
    }
    message
}
