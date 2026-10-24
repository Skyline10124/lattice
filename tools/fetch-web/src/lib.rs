// Copyright 2026 Ast Rin
// SPDX-License-Identifier: Apache-2.0

//! `fetch__web` Component guest: typed request and result shaping only.

#![no_std]
extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

mod bindings {
    lattice_tool_prelude::wit_bindgen::generate!({
        path: "generated",
    });
    use crate::FetchWeb;
    lattice_tool_prelude::export_tool!(FetchWeb);
}

use bindings::exports::lattice::tool_host::tool_api::{Guest, ToolOutput};
use bindings::lattice::tool_host::host_control;
use bindings::lattice::tool_host::network_fetch::{self, FetchError, FetchRequest, Header};

struct FetchWeb;

const RESULT_CELL: &str = "alloc:read:external-host";

impl Guest for FetchWeb {
    fn run(params_json: String) -> Result<ToolOutput, String> {
        if host_control::cancelled() {
            return Err(String::from("cancelled"));
        }

        let params: serde_json::Value =
            serde_json::from_str(&params_json).map_err(|err| format!("malformed params: {err}"))?;
        let url = params
            .get("url")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| String::from("params carry no url"))?
            .to_string();
        let headers: Vec<Header> = params
            .get("headers")
            .and_then(serde_json::Value::as_object)
            .into_iter()
            .flatten()
            .map(|(name, value)| Header {
                name: name.clone(),
                value: value.as_str().unwrap_or_default().to_string(),
            })
            .collect();

        let response = network_fetch::fetch(&FetchRequest { url, headers }).map_err(describe)?;
        let body = String::from_utf8(response.body)
            .map_err(|_| String::from("host returned a non-UTF-8 response body"))?;
        let mut value = serde_json::Map::new();
        value.insert("status".into(), response.status.into());
        value.insert("body".into(), body.into());
        if let Some(content_type) = response.content_type {
            value.insert("content_type".into(), content_type.into());
        }
        value.insert("truncated".into(), response.truncated.into());
        value.insert("final_origin".into(), response.final_origin.into());
        value.insert("redirect_count".into(), response.redirect_count.into());

        Ok(ToolOutput {
            result_cell: String::from(RESULT_CELL),
            value_json: serde_json::Value::Object(value).to_string(),
        })
    }
}

fn describe(error: FetchError) -> String {
    String::from(match error {
        FetchError::InvalidRequest => "invalid network request",
        FetchError::Denied => "network request denied",
        FetchError::Unavailable => "network request unavailable",
        FetchError::LimitExceeded => "network request limit exceeded",
        FetchError::Timeout => "network request timed out",
        FetchError::Cancelled => "cancelled",
    })
}
