// Copyright 2023 Greptime Team
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::collections::HashMap;
use std::time::Instant;

use aide::transform::TransformOperation;
use axum::extract::{Json, Query, State};
use axum::{Extension, Form};
use common_error::status_code::StatusCode;
use common_telemetry::timer;
use query::parser::PromQuery;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use session::context::UserInfo;

use crate::http::{ApiState, JsonResponse};
use crate::metrics_handler::MetricsHandler;

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct SqlQuery {
    pub db: Option<String>,
    pub sql: Option<String>,
}

/// Handler to execute sql
#[axum_macros::debug_handler]
pub async fn sql(
    State(state): State<ApiState>,
    Query(query_params): Query<SqlQuery>,
    // TODO(fys): pass _user_info into query context
    _user_info: Extension<UserInfo>,
    Form(form_params): Form<SqlQuery>,
) -> Json<JsonResponse> {
    let _timer = timer!(crate::metrics::METRIC_HTTP_SQL_ELAPSED);

    let sql_handler = &state.sql_handler;

    let start = Instant::now();
    let sql = query_params.sql.or(form_params.sql);
    let db = query_params.db.or(form_params.db);

    let resp = if let Some(sql) = &sql {
        match crate::http::query_context_from_db(sql_handler.clone(), db).await {
            Ok(query_ctx) => {
                JsonResponse::from_output(sql_handler.do_query(sql, query_ctx).await).await
            }
            Err(resp) => resp,
        }
    } else {
        JsonResponse::with_error(
            "sql parameter is required.".to_string(),
            StatusCode::InvalidArguments,
        )
    };

    Json(resp.with_execution_time(start.elapsed().as_millis()))
}

#[derive(Debug, Default, Serialize, Deserialize, JsonSchema)]
pub struct PromqlQuery {
    pub query: String,
    pub start: String,
    pub end: String,
    pub step: String,
    pub db: Option<String>,
}

impl From<PromqlQuery> for PromQuery {
    fn from(query: PromqlQuery) -> Self {
        PromQuery {
            query: query.query,
            start: query.start,
            end: query.end,
            step: query.step,
        }
    }
}

/// Handler to execute promql
#[axum_macros::debug_handler]
pub async fn promql(
    State(state): State<ApiState>,
    Query(params): Query<PromqlQuery>,
    // TODO(fys): pass _user_info into query context
    _user_info: Extension<UserInfo>,
) -> Json<JsonResponse> {
    let _timer = timer!(crate::metrics::METRIC_HTTP_PROMQL_ELAPSED);

    let sql_handler = &state.sql_handler;
    let exec_start = Instant::now();
    let db = params.db.clone();
    let prom_query = params.into();
    let resp = match super::query_context_from_db(sql_handler.clone(), db).await {
        Ok(query_ctx) => {
            JsonResponse::from_output(sql_handler.do_promql_query(&prom_query, query_ctx).await)
                .await
        }
        Err(resp) => resp,
    };

    Json(resp.with_execution_time(exec_start.elapsed().as_millis()))
}

pub(crate) fn sql_docs(op: TransformOperation) -> TransformOperation {
    op.response::<200, Json<JsonResponse>>()
}

/// Handler to export metrics
#[axum_macros::debug_handler]
pub async fn metrics(
    State(state): State<MetricsHandler>,
    Query(_params): Query<HashMap<String, String>>,
) -> String {
    state.render()
}

#[derive(Debug, Serialize, Deserialize, JsonSchema)]
pub struct HealthQuery {}

#[derive(Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq)]
pub struct HealthResponse {}

/// Handler to export healthy check
///
/// Currently simply return status "200 OK" (default) with an empty json payload "{}"
#[axum_macros::debug_handler]
pub async fn health(Query(_params): Query<HealthQuery>) -> Json<HealthResponse> {
    Json(HealthResponse {})
}
