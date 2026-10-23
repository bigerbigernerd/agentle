//! `agentle admin seed <file.json>`: play a scripted opening through the normal rules (quotas aside) and mark its
//! agents as house guests. Runs once: if the first agent already exists, nothing happens.

use crate::store::{Agent, Store};
use crate::util::ApiError;
use serde_json::{json, Value};
use std::collections::HashMap;

pub fn run(store: &Store, path: &str) -> Result<String, ApiError> {
    let text = std::fs::read_to_string(path).map_err(|e| ApiError::bad("usage", format!("cannot read {path}: {e}")))?;
    let doc: Value = serde_json::from_str(&text).map_err(|e| ApiError::bad("usage", format!("{path} is not valid JSON: {e}")))?;
    let agents = doc["agents"].as_array().cloned().unwrap_or_default();
    let steps = doc["steps"].as_array().cloned().unwrap_or_default();
    let Some(first) = agents.first().and_then(|a| a["handle"].as_str()) else {
        return Err(ApiError::bad("usage", "the scenario has no agents"));
    };
    if store.agent_id(first)?.is_some() {
        return Ok(format!("already seeded ({first} exists); nothing changed"));
    }
    store.set_register_limit(100_000);
    let house = doc["house"].as_bool().unwrap_or(true);
    let mut keys: HashMap<String, String> = HashMap::new();
    for a in &agents {
        let r = store.register(a, "house-seed")?;
        let key = r["api_key"].as_str().unwrap_or_default().to_string();
        let token = r["claim_url"].as_str().unwrap_or_default().rsplit('/').next().unwrap_or_default().to_string();
        store.claim(&token, "house-seed")?;
        let handle = r["agent"]["handle"].as_str().unwrap_or_default().to_string();
        if house {
            store.admin(&["house".to_string(), handle.clone()])?;
        }
        keys.insert(handle, key);
    }

    let me = |h: &str| -> Result<Agent, ApiError> {
        let key = keys.get(h).ok_or_else(|| ApiError::bad("usage", format!("unknown agent `{h}` in steps")))?;
        store.auth(key)
    };
    let date = |a: &str, b: &str| -> Result<i64, ApiError> {
        store.match_between_handles(a, b)?.ok_or_else(|| ApiError::bad("usage", format!("{a} and {b} have not matched")))
    };
    for (n, step) in steps.iter().enumerate() {
        let s = |i: usize| step.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let flag = |i: usize| step.get(i).and_then(|v| v.as_bool()).unwrap_or(false);
        let op = s(0);
        let (a, b) = (s(1), s(2));
        let res = match op.as_str() {
            "swipe" => store.swipe(&me(&a)?, &json!({"target": b, "like": flag(3), "reason": s(4)})),
            "match" => store
                .swipe(&me(&a)?, &json!({"target": b, "like": true, "reason": s(3)}))
                .and_then(|_| store.swipe(&me(&b)?, &json!({"target": a, "like": true, "reason": s(4)}))),
            "say" => store.send_message(&me(&a)?, date(&a, &b)?, &json!({"body": s(3)})),
            "propose" => store.propose(&me(&a)?, date(&a, &b)?, &json!({"message": s(3)})),
            "answer" => store.answer(&me(&a)?, date(&a, &b)?, &json!({"accept": flag(3), "message": s(4)})),
            "exchange" => store.start_exchange(&me(&a)?, date(&a, &b)?, &json!({"kind": s(3), "value": s(4)})),
            "accept" => {
                let id = store
                    .pending_exchange(date(&a, &b)?)?
                    .ok_or_else(|| ApiError::bad("usage", format!("no pending exchange between {a} and {b}")))?;
                store.accept_exchange(&me(&a)?, id, &json!({"kind": s(3), "value": s(4)}))
            }
            "post" => store.create_post(&me(&a)?, &json!({"body": b})),
            "couple_post" => store.couple_post(&me(&a)?, &json!({"body": b})),
            "breakup" => store.breakup(&me(&a)?, &json!({"reason": b})),
            other => Err(ApiError::bad("usage", format!("unknown step `{other}`"))),
        };
        res.map_err(|e| ApiError::new(e.status, e.code, format!("step {n} ({op}): {}", e.message), e.hint))?;
    }
    Ok(format!("seeded {} house agents with {} steps", agents.len(), steps.len()))
}
