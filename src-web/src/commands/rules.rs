use pebble_core::{new_id, now_timestamp, Rule};
use serde_json::Value;

use crate::blocking::run_blocking;
use crate::error::ApiError;
use crate::state::AppStateRef;

pub async fn list_rules(state: AppStateRef, _args: Value) -> Result<Value, ApiError> {
    let store = state.store.clone();
    let rules = run_blocking(move || store.list_rules()).await?;
    serde_json::to_value(rules).map_err(ApiError::from_serialize)
}

pub async fn create_rule(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        name: String,
        priority: i32,
        conditions: String,
        actions: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid create_rule args: {e}")))?;
    let now = now_timestamp();
    let rule = Rule {
        id: new_id(),
        name: args.name,
        priority: args.priority,
        conditions: args.conditions,
        actions: args.actions,
        is_enabled: true,
        created_at: now,
        updated_at: now,
    };
    let store = state.store.clone();
    run_blocking(move || {
        store.insert_rule(&rule)?;
        Ok(rule)
    })
    .await
    .and_then(|rule| serde_json::to_value(rule).map_err(ApiError::from_serialize))
}

pub async fn update_rule(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    // 与上游 Tauri 命令签名一致：前端 invoke 传 { rule: Rule }
    let rule: Rule = serde_json::from_value(args.get("rule").cloned().unwrap_or(Value::Null))
        .map_err(|e| ApiError::BadRequest(format!("invalid update_rule args: {e}")))?;
    let store = state.store.clone();
    run_blocking(move || store.update_rule(&rule)).await?;
    Ok(Value::Null)
}

pub async fn delete_rule(state: AppStateRef, args: Value) -> Result<Value, ApiError> {
    #[derive(serde::Deserialize)]
    struct Args {
        rule_id: String,
    }
    let args: Args = serde_json::from_value(args)
        .map_err(|e| ApiError::BadRequest(format!("invalid delete_rule args: {e}")))?;
    let store = state.store.clone();
    run_blocking(move || store.delete_rule(&args.rule_id)).await?;
    Ok(Value::Null)
}
