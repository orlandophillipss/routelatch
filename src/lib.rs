use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, sync::Arc, time::Duration};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    pub description: String,
    pub cost_units: u32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub revision: String,
    pub min_confidence: f64,
    pub budget_units: u32,
    pub workflows: BTreeMap<String, Workflow>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub choice: String,
    pub confidence: f64,
    pub probabilities: BTreeMap<String, f64>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Evaluation {
    pub policy: Policy,
    pub decision: Decision,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RouteRequest {
    pub state: String,
    pub policy: Policy,
}
#[derive(Debug, Serialize, Deserialize, PartialEq)]
pub struct Receipt {
    pub outcome: String,
    pub reason: String,
    pub workflow: Option<String>,
    pub fingerprint: String,
}

fn validate_policy(p: &Policy) -> Result<(), String> {
    if p.revision.is_empty()
        || p.revision.len() > 80
        || !p.min_confidence.is_finite()
        || !(0.0..=1.0).contains(&p.min_confidence)
    {
        return Err("Invalid policy revision or confidence threshold".into());
    }
    if p.workflows.is_empty()
        || p.workflows.len() > 32
        || p.workflows.iter().any(|(id, w)| {
            id.is_empty() || id.len() > 64 || w.description.is_empty() || w.description.len() > 1000
        })
    {
        return Err("Provide 1–32 named workflows with bounded descriptions".into());
    }
    Ok(())
}
pub fn evaluate(e: &Evaluation) -> Result<Receipt, String> {
    validate_policy(&e.policy)?;
    let d = &e.decision;
    if !d.confidence.is_finite()
        || !(0.0..=1.0).contains(&d.confidence)
        || d.probabilities.len() != e.policy.workflows.len()
        || d.probabilities.keys().ne(e.policy.workflows.keys())
    {
        return Err("Decision must contain exactly the configured workflow options".into());
    }
    if d.probabilities
        .values()
        .any(|p| !p.is_finite() || !(0.0..=1.0).contains(p))
        || (d.probabilities.values().sum::<f64>() - 1.0).abs() > 0.001
    {
        return Err("Probabilities must be finite and sum to one".into());
    }
    let Some(workflow) = e.policy.workflows.get(&d.choice) else {
        return Err("Unknown decision option".into());
    };
    if d.probabilities
        .iter()
        .any(|(_, p)| *p > d.probabilities[&d.choice] + 0.000001)
    {
        return Err("Choice does not match the highest probability".into());
    }
    let (outcome, reason, selected) = if d.confidence < e.policy.min_confidence {
        ("review", "confidence_below_threshold", None)
    } else if workflow.cost_units > e.policy.budget_units {
        ("review", "budget_exceeded", None)
    } else {
        ("route", "policy_passed", Some(d.choice.clone()))
    };
    let fingerprint = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(e).map_err(|_| "Serialization failed")?)
    );
    Ok(Receipt {
        outcome: outcome.into(),
        reason: reason.into(),
        workflow: selected,
        fingerprint,
    })
}

pub struct AppState {
    pub key: Option<String>,
    pub endpoint: String,
    pub client: reqwest::Client,
}
impl AppState {
    pub fn new(key: Option<String>, endpoint: String) -> Self {
        Self {
            key,
            endpoint,
            client: reqwest::Client::builder()
                .timeout(Duration::from_secs(12))
                .build()
                .expect("HTTP client"),
        }
    }
}
type ApiError = (StatusCode, Json<serde_json::Value>);
fn error(code: StatusCode, message: &str) -> ApiError {
    (code, Json(serde_json::json!({"error":message})))
}
async fn evaluate_handler(Json(e): Json<Evaluation>) -> Result<Json<Receipt>, ApiError> {
    evaluate(&e)
        .map(Json)
        .map_err(|m| error(StatusCode::UNPROCESSABLE_ENTITY, &m))
}
async fn route_handler(
    State(s): State<Arc<AppState>>,
    Json(r): Json<RouteRequest>,
) -> Result<Json<Receipt>, ApiError> {
    validate_policy(&r.policy).map_err(|m| error(StatusCode::UNPROCESSABLE_ENTITY, &m))?;
    if r.state.trim().is_empty() || r.state.len() > 16000 {
        return Err(error(
            StatusCode::UNPROCESSABLE_ENTITY,
            "State must contain 1–16000 bytes",
        ));
    }
    let key = s.key.as_ref().ok_or_else(|| {
        error(
            StatusCode::SERVICE_UNAVAILABLE,
            "Set TYPESAFE_API_KEY to enable live Jev routing; /v1/evaluate is available offline",
        )
    })?;
    let criteria: BTreeMap<_, _> = r
        .policy
        .workflows
        .iter()
        .map(|(id, w)| (id.clone(), w.description.clone()))
        .collect();
    let response = s.client.post(&s.endpoint).bearer_auth(key).json(&serde_json::json!({
        "model":"jev-latest", "state":r.state,
        "questions":{"workflow":{"type":"choice","instructions":"Which available workflow best handles this state?","criteria":criteria}}
    })).send().await.map_err(|_|error(StatusCode::BAD_GATEWAY,"Decision provider unavailable"))?;
    if !response.status().is_success() {
        return Err(error(
            StatusCode::BAD_GATEWAY,
            "Decision provider rejected request",
        ));
    }
    // Bound provider responses rather than accepting an unbounded JSON body.
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "Invalid provider response"))?
    {
        if bytes.len() + chunk.len() > 65536 {
            return Err(error(
                StatusCode::BAD_GATEWAY,
                "Provider response too large",
            ));
        }
        bytes.extend_from_slice(&chunk);
    }
    let body: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|_| error(StatusCode::BAD_GATEWAY, "Invalid provider JSON"))?;
    let answer = &body["answers"]["workflow"];
    let decision = Decision {
        choice: answer["choice"].as_str().unwrap_or("").into(),
        confidence: answer["confidence"].as_f64().unwrap_or(-1.0),
        probabilities: serde_json::from_value(answer["probabilities"].clone())
            .map_err(|_| error(StatusCode::BAD_GATEWAY, "Invalid probability map"))?,
    };
    evaluate(&Evaluation {
        policy: r.policy,
        decision,
    })
    .map(Json)
    .map_err(|m| error(StatusCode::BAD_GATEWAY, &m))
}
pub fn app(state: AppState) -> Router {
    Router::new()
        .route(
            "/health",
            get(|| async { Json(serde_json::json!({"status":"ok"})) }),
        )
        .route("/v1/evaluate", post(evaluate_handler))
        .route("/v1/route", post(route_handler))
        .layer(axum::extract::DefaultBodyLimit::max(32768))
        .with_state(Arc::new(state))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn sample() -> Evaluation {
        serde_json::from_str(include_str!("../examples/evaluate.json")).unwrap()
    }
    #[test]
    fn routes_and_replays() {
        let e = sample();
        let a = evaluate(&e).unwrap();
        assert_eq!(a.outcome, "route");
        assert_eq!(a.workflow.as_deref(), Some("research"));
        assert_eq!(a, evaluate(&e).unwrap());
    }
    #[test]
    fn gates_confidence_and_budget() {
        let mut e = sample();
        e.policy.min_confidence = 0.99;
        assert_eq!(evaluate(&e).unwrap().reason, "confidence_below_threshold");
        e.policy.min_confidence = 0.5;
        e.policy.budget_units = 0;
        assert_eq!(evaluate(&e).unwrap().reason, "budget_exceeded");
    }
    #[test]
    fn rejects_unknown_and_invalid_distributions() {
        let mut e = sample();
        e.decision.choice = "shell".into();
        assert!(evaluate(&e).is_err());
        e = sample();
        e.decision.probabilities.insert("research".into(), 0.2);
        assert!(evaluate(&e).is_err());
    }
    #[test]
    fn policy_revision_changes_receipt() {
        let a = sample();
        let mut b = a.clone();
        b.policy.revision = "v2".into();
        assert_ne!(
            evaluate(&a).unwrap().fingerprint,
            evaluate(&b).unwrap().fingerprint
        );
    }
    #[tokio::test]
    async fn http_and_missing_key() {
        use tower::ServiceExt;
        let router = app(AppState::new(None, "http://127.0.0.1:1".into()));
        let res = router
            .clone()
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/evaluate")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::to_vec(&sample()).unwrap(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let res = router
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/route")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({"state":"Find documentation","policy":sample().policy})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
    #[tokio::test]
    async fn provider_contract() {
        use tower::ServiceExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let provider=Router::new().route("/",post(|headers:axum::http::HeaderMap,Json(v):Json<serde_json::Value>|async move {
            assert_eq!(headers["authorization"],"Bearer fixture-key");assert_eq!(v["model"],"jev-latest");assert_eq!(v["questions"]["workflow"]["type"],"choice");
            Json(serde_json::json!({"answers":{"workflow":{"type":"choice","choice":"research","confidence":0.9,"probabilities":{"research":0.95,"summarise":0.05}}}}))
        }));
        let task = tokio::spawn(async move { axum::serve(listener, provider).await.unwrap() });
        let router = app(AppState::new(
            Some("fixture-key".into()),
            format!("http://{addr}/"),
        ));
        let res = router
            .oneshot(
                axum::http::Request::builder()
                    .method("POST")
                    .uri("/v1/route")
                    .header("content-type", "application/json")
                    .body(axum::body::Body::from(
                        serde_json::json!({"state":"Find docs","policy":sample().policy})
                            .to_string(),
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        task.abort();
    }
}
