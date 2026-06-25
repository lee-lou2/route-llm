use super::*;
use sqlx::Row;

#[tokio::test]
async fn responses_request_maps_text_tools_and_tool_choice_to_chat() {
    let path = std::env::temp_dir().join(format!(
        "route-llm-responses-compat-{}.sqlite",
        std::process::id()
    ));
    let url = format!("sqlite://{}", path.display());
    let pool = db::connect(&url).await.unwrap();
    let body = Bytes::from_static(
        br#"{
            "model":"llm-model",
            "instructions":"Be terse.",
            "input":"ping",
            "stream":true,
            "tools":[{"type":"function","name":"lookup","description":"Lookup","parameters":{"type":"object"}}],
            "tool_choice":{"type":"function","name":"lookup"},
            "max_output_tokens":12
        }"#,
    );

    let prepared = prepare_request(&pool, Some(1), &body).await.unwrap();
    let outbound = prepared.body_for_candidate(Some("provider-llm")).unwrap();
    let value: Value = serde_json::from_slice(&outbound).unwrap();

    assert_eq!(value["model"], "provider-llm");
    assert_eq!(value["messages"][0]["role"], "system");
    assert_eq!(value["messages"][1]["content"], "ping");
    assert_eq!(value["stream_options"]["include_usage"], true);
    assert_eq!(value["tools"][0]["function"]["name"], "lookup");
    assert_eq!(value["tool_choice"]["function"]["name"], "lookup");
    assert_eq!(value["max_tokens"], 12);
    pool.close().await;
    remove_sqlite_files(path);
}

#[tokio::test]
async fn responses_request_maps_codex_namespace_and_custom_tools_for_chat_adapter() {
    let path = temp_sqlite_path("route-llm-responses-codex-tools");
    let url = format!("sqlite://{}", path.display());
    let pool = db::connect(&url).await.unwrap();
    let body = Bytes::from_static(
        br#"{
            "model":"llm-model",
            "input":"ping",
            "stream":true,
            "parallel_tool_calls":false,
            "tools":[
                {"type":"namespace","name":"mcp","tools":[{"type":"function","name":"lookup","description":"Lookup","parameters":{"type":"object"}}]},
                {"type":"web_search","search_context_size":"low"},
                {"type":"custom","name":"shell","description":"Run shell"},
                {"type":"image_generation","quality":"low"}
            ],
            "tool_choice":{"type":"web_search"}
        }"#,
    );

    let prepared = prepare_request(&pool, None, &body).await.unwrap();
    let outbound = prepared.body_for_candidate(Some("provider-llm")).unwrap();
    let outbound_value: Value = serde_json::from_slice(&outbound).unwrap();

    assert_eq!(outbound_value["model"], "provider-llm");
    assert_eq!(outbound_value["messages"][0]["content"], "ping");
    assert_eq!(outbound_value["stream_options"]["include_usage"], true);
    assert_eq!(outbound_value["parallel_tool_calls"], false);
    let tools = outbound_value["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 2);
    assert_eq!(tools[0]["function"]["name"], "mcp.lookup");
    assert_eq!(tools[1]["function"]["name"], "shell");
    assert_eq!(
        tools[1]["function"]["parameters"]["properties"]["input"]["type"],
        "string"
    );
    assert!(outbound_value.get("tool_choice").is_none());

    let chat_response = Bytes::from_static(
        br#"{"choices":[{"message":{"role":"assistant","content":"pong"}}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
    );
    let (body, usage) = convert_json_response(&pool, None, &prepared, &chat_response)
        .await
        .unwrap();
    let response_value: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(response_value["output_text"], "pong");
    assert_eq!(response_value["tools"].as_array().unwrap().len(), 4);
    assert_eq!(response_value["tool_choice"]["type"], "web_search");
    assert_eq!(usage.unwrap().total_tokens, Some(5));
    pool.close().await;
    remove_sqlite_files(path);
}

#[tokio::test]
async fn json_response_restores_namespace_and_custom_tool_calls() {
    let path = temp_sqlite_path("route-llm-responses-tool-output-map");
    let url = format!("sqlite://{}", path.display());
    let pool = db::connect(&url).await.unwrap();
    let request = prepare_request(
        &pool,
        None,
        &Bytes::from_static(
            br#"{
                "model":"llm-model",
                "input":"ping",
                "tools":[
                    {"type":"namespace","name":"mcp","tools":[{"type":"function","name":"lookup","description":"Lookup","parameters":{"type":"object"}}]},
                    {"type":"custom","name":"terminal","description":"Terminal"}
                ]
            }"#,
        ),
    )
    .await
    .unwrap();
    let chat_response = Bytes::from_static(
        br#"{"choices":[{"message":{"role":"assistant","content":null,"tool_calls":[{"id":"call_lookup","type":"function","function":{"name":"mcp.lookup","arguments":"{\"q\":\"x\"}"}},{"id":"call_terminal","type":"function","function":{"name":"terminal","arguments":"{\"input\":\"echo hi\"}"}}]}}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
    );

    let (body, usage) = convert_json_response(&pool, None, &request, &chat_response)
        .await
        .unwrap();
    let response_value: Value = serde_json::from_slice(&body).unwrap();
    let output = response_value["output"].as_array().unwrap();

    assert_eq!(output[0]["type"], "function_call");
    assert_eq!(output[0]["namespace"], "mcp");
    assert_eq!(output[0]["name"], "lookup");
    assert_eq!(output[0]["arguments"], "{\"q\":\"x\"}");
    assert_eq!(output[1]["type"], "custom_tool_call");
    assert_eq!(output[1]["name"], "terminal");
    assert_eq!(output[1]["input"], "echo hi");
    assert_eq!(usage.unwrap().total_tokens, Some(5));
    pool.close().await;
    remove_sqlite_files(path);
}

#[tokio::test]
async fn responses_input_restores_tool_calls_for_chat_history() {
    let path = temp_sqlite_path("route-llm-responses-input-tool-calls");
    let url = format!("sqlite://{}", path.display());
    let pool = db::connect(&url).await.unwrap();
    let request = prepare_request(
        &pool,
        None,
        &Bytes::from_static(
            br#"{
                "model":"llm-model",
                "input":[
                    {"type":"message","role":"developer","content":[{"type":"input_text","text":"Be terse."}]},
                    {"type":"message","role":"user","content":[{"type":"input_text","text":"ping"}]},
                    {"type":"function_call","namespace":"mcp","name":"lookup","call_id":"call_lookup","arguments":"{\"q\":\"x\"}"},
                    {"type":"function_call_output","call_id":"call_lookup","output":"found"},
                    {"type":"custom_tool_call","name":"terminal","call_id":"call_terminal","input":"echo hi"},
                    {"type":"custom_tool_call_output","call_id":"call_terminal","output":"hi"}
                ]
            }"#,
        ),
    )
    .await
    .unwrap();

    assert_eq!(request.chat_messages[0]["role"], "system");
    assert_eq!(request.chat_messages[1]["role"], "user");
    assert_eq!(
        request.chat_messages[2]["tool_calls"][0]["function"]["name"],
        "mcp.lookup"
    );
    assert_eq!(request.chat_messages[3]["role"], "tool");
    assert_eq!(
        request.chat_messages[4]["tool_calls"][0]["function"]["arguments"],
        "{\"input\":\"echo hi\"}"
    );
    assert_eq!(request.chat_messages[5]["content"], "hi");
    pool.close().await;
    remove_sqlite_files(path);
}

#[tokio::test]
async fn responses_request_keeps_function_tools_when_ignoring_builtin_tools() {
    let path = temp_sqlite_path("route-llm-responses-mixed-tools");
    let url = format!("sqlite://{}", path.display());
    let pool = db::connect(&url).await.unwrap();
    let body = Bytes::from_static(
        br#"{
            "model":"llm-model",
            "input":"ping",
            "tools":[
                {"type":"namespace","name":"mcp"},
                {"type":"function","name":"lookup","description":"Lookup","parameters":{"type":"object"}},
                {"type":"web_search"}
            ],
            "tool_choice":{"type":"function","name":"lookup"}
        }"#,
    );

    let prepared = prepare_request(&pool, None, &body).await.unwrap();
    let outbound = prepared.body_for_candidate(Some("provider-llm")).unwrap();
    let value: Value = serde_json::from_slice(&outbound).unwrap();
    let tools = value["tools"].as_array().unwrap();

    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0]["type"], "function");
    assert_eq!(tools[0]["function"]["name"], "lookup");
    assert_eq!(value["tool_choice"]["function"]["name"], "lookup");
    pool.close().await;
    remove_sqlite_files(path);
}

#[tokio::test]
async fn stream_adapter_completes_when_request_includes_ignored_tools() {
    let path = temp_sqlite_path("route-llm-responses-stream-ignored-tools");
    let url = format!("sqlite://{}", path.display());
    let pool = db::connect(&url).await.unwrap();
    let request = prepare_request(
        &pool,
        None,
        &Bytes::from_static(
            br#"{
                "model":"llm-model",
                "input":"ping",
                "stream":true,
                "tools":[
                    {"type":"namespace","name":"mcp"},
                    {"type":"web_search"},
                    {"type":"custom","name":"terminal"},
                    {"type":"image_generation"}
                ],
                "tool_choice":{"type":"custom","name":"terminal"}
            }"#,
        ),
    )
    .await
    .unwrap();
    let mut adapter = ChatStreamAdapter::new(request);
    let mut frames = adapter.start_frames();
    frames.extend(
        adapter
            .push_bytes(&Bytes::from_static(
                br#"data: {"choices":[{"delta":{"content":"po"}}]}
data: {"choices":[{"delta":{"content":"ng"}}]}
data: [DONE]

"#,
            ))
            .unwrap(),
    );
    let finalized = adapter.finish().unwrap();
    frames.extend(finalized.frames);
    let text = frames
        .iter()
        .map(|frame| String::from_utf8_lossy(frame).to_string())
        .collect::<String>();

    assert!(text.contains("event: response.created"));
    assert!(text.contains("event: response.output_text.delta"));
    assert!(text.contains("event: response.completed"));
    assert_eq!(finalized.output_text, "pong");
    pool.close().await;
    remove_sqlite_files(path);
}

#[tokio::test]
async fn json_response_stores_state_for_previous_response_id() {
    let path = std::env::temp_dir().join(format!(
        "route-llm-responses-state-{}.sqlite",
        std::process::id()
    ));
    let url = format!("sqlite://{}", path.display());
    let pool = db::connect(&url).await.unwrap();
    let client_id = db::upsert_client(&pool, "client", "client-test-token", true)
        .await
        .unwrap();
    let request = prepare_request(
        &pool,
        Some(client_id),
        &Bytes::from_static(br#"{"model":"llm-model","input":"ping"}"#),
    )
    .await
    .unwrap();
    let chat_response = Bytes::from_static(
        br#"{"choices":[{"message":{"role":"assistant","content":"pong"}}],"usage":{"prompt_tokens":3,"completion_tokens":2,"total_tokens":5}}"#,
    );

    let (body, usage) = convert_json_response(&pool, Some(client_id), &request, &chat_response)
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&body).unwrap();

    assert_eq!(value["object"], "response");
    assert_eq!(value["output_text"], "pong");
    assert_eq!(value["usage"]["input_tokens"], 3);
    assert_eq!(usage.unwrap().total_tokens, Some(5));

    let follow_up = prepare_request(
        &pool,
        Some(client_id),
        &Bytes::from(
            serde_json::json!({
                "model": "llm-model",
                "previous_response_id": request.response_id,
                "input": "again"
            })
            .to_string(),
        ),
    )
    .await
    .unwrap();
    assert_eq!(follow_up.chat_messages.len(), 3);
    assert_eq!(follow_up.chat_messages[1]["content"], "pong");
    assert_eq!(follow_up.chat_messages[2]["content"], "again");
    pool.close().await;
    remove_sqlite_files(path);
}

#[test]
fn stream_adapter_emits_response_events_and_usage() {
    let request = PreparedResponseRequest {
        response_id: "resp_test".to_string(),
        message_id: "msg_test".to_string(),
        created_at: 1,
        model: "llm-model".to_string(),
        instructions: None,
        previous_response_id: None,
        stream: true,
        chat_messages: vec![json!({"role":"user","content":"ping"})],
        chat_body: json!({}),
        tool_map: ResponseToolMap::default(),
        response_tools: json!([]),
        response_tool_choice: Value::String("auto".to_string()),
        parallel_tool_calls: Value::Bool(true),
        max_output_tokens: Value::Null,
        temperature: Value::Null,
        top_p: Value::Null,
        store: Value::Bool(true),
        reasoning: json!({"effort": Value::Null, "summary": Value::Null}),
        text: json!({"format": {"type": "text"}}),
        truncation: Value::String("disabled".to_string()),
        metadata: json!({}),
        user: Value::Null,
    };
    let mut adapter = ChatStreamAdapter::new(request);
    let mut frames = adapter.start_frames();
    frames.extend(
        adapter
            .push_bytes(&Bytes::from_static(
                br#"data: {"choices":[{"delta":{"content":"hel"}}]}
data: {"choices":[{"delta":{"content":"lo"}}]}
data: {"choices":[],"usage":{"prompt_tokens":4,"completion_tokens":2,"total_tokens":6}}
data: [DONE]

"#,
            ))
            .unwrap(),
    );
    let finalized = adapter.finish().unwrap();
    frames.extend(finalized.frames);
    let text = frames
        .iter()
        .map(|frame| String::from_utf8_lossy(frame).to_string())
        .collect::<String>();

    assert!(text.contains("event: response.created"));
    assert!(text.contains("event: response.output_text.delta"));
    assert!(text.contains("\"delta\":\"hel\""));
    assert!(text.contains("event: response.completed"));
    assert_eq!(finalized.output_text, "hello");
    assert_eq!(finalized.usage.unwrap().total_tokens, Some(6));
}

#[test]
fn stream_adapter_emits_tool_call_events() {
    let request = PreparedResponseRequest {
        response_id: "resp_test".to_string(),
        message_id: "msg_test".to_string(),
        created_at: 1,
        model: "llm-model".to_string(),
        instructions: None,
        previous_response_id: None,
        stream: true,
        chat_messages: vec![json!({"role":"user","content":"ping"})],
        chat_body: json!({}),
        tool_map: ResponseToolMap::default(),
        response_tools: json!([]),
        response_tool_choice: Value::String("auto".to_string()),
        parallel_tool_calls: Value::Bool(true),
        max_output_tokens: Value::Null,
        temperature: Value::Null,
        top_p: Value::Null,
        store: Value::Bool(true),
        reasoning: json!({"effort": Value::Null, "summary": Value::Null}),
        text: json!({"format": {"type": "text"}}),
        truncation: Value::String("disabled".to_string()),
        metadata: json!({}),
        user: Value::Null,
    };
    let mut adapter = ChatStreamAdapter::new(request);
    let frames = adapter
        .push_bytes(&Bytes::from_static(
            br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"lookup","arguments":"{\"q\""}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":\"x\"}"}}]}}]}

"#,
        ))
        .unwrap();
    let finalized = adapter.finish().unwrap();
    let text = frames
        .into_iter()
        .chain(finalized.frames)
        .map(|frame| String::from_utf8_lossy(&frame).to_string())
        .collect::<String>();

    assert!(text.contains("response.function_call_arguments.delta"));
    assert!(text.contains("response.function_call_arguments.done"));
    assert_eq!(finalized.output[0]["type"], "function_call");
    assert_eq!(finalized.output[0]["name"], "lookup");
    assert_eq!(finalized.output[0]["arguments"], "{\"q\":\"x\"}");
}

#[tokio::test]
async fn stream_adapter_restores_namespace_custom_tool_call_events() {
    let path = temp_sqlite_path("route-llm-responses-stream-custom-tool");
    let url = format!("sqlite://{}", path.display());
    let pool = db::connect(&url).await.unwrap();
    let request = prepare_request(
        &pool,
        None,
        &Bytes::from_static(
            br#"{
                "model":"llm-model",
                "input":"ping",
                "stream":true,
                "tools":[
                    {"type":"namespace","name":"mcp","tools":[{"type":"custom","name":"terminal","description":"Terminal"}]}
                ]
            }"#,
        ),
    )
    .await
    .unwrap();
    let mut adapter = ChatStreamAdapter::new(request);
    let frames = adapter
        .push_bytes(&Bytes::from_static(
            br#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"mcp.terminal","arguments":"{\"input\":\"echo"}}]}}]}
data: {"choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":" hi\"}"}}]}}]}

"#,
        ))
        .unwrap();
    let finalized = adapter.finish().unwrap();
    let text = frames
        .into_iter()
        .chain(finalized.frames)
        .map(|frame| String::from_utf8_lossy(&frame).to_string())
        .collect::<String>();

    assert!(text.contains("response.custom_tool_call_input.delta"));
    assert!(text.contains("response.custom_tool_call_input.done"));
    assert_eq!(finalized.output[0]["type"], "custom_tool_call");
    assert_eq!(finalized.output[0]["namespace"], "mcp");
    assert_eq!(finalized.output[0]["name"], "terminal");
    assert_eq!(finalized.output[0]["input"], "echo hi");
    pool.close().await;
    remove_sqlite_files(path);
}

#[tokio::test]
async fn streaming_conversion_error_updates_request_audit_outcome() {
    let path = temp_sqlite_path("route-llm-responses-stream-audit-error");
    let url = format!("sqlite://{}", path.display());
    let pool = db::connect(&url).await.unwrap();
    let request = prepare_request(
        &pool,
        None,
        &Bytes::from_static(br#"{"model":"llm-model","input":"ping","stream":true}"#),
    )
    .await
    .unwrap();
    let audit_id = db::insert_request_audit(
        &pool,
        &db::RequestAudit {
            completed_at: db::now_epoch(),
            duration_ms: 1,
            client_id: None,
            client_name: None,
            client_token_id: None,
            client_token_name: None,
            client_key_hash: None,
            client_ip: None,
            client_ip_source: None,
            cf_ray: None,
            cf_country: None,
            method: "POST".to_string(),
            path: "/v1/responses".to_string(),
            route_kind: "responses".to_string(),
            has_query: false,
            query_hash: None,
            model: Some("llm-model".to_string()),
            stream: Some(true),
            content_type: Some("application/json".to_string()),
            request_body_bytes: Some(64),
            user_agent_hash: None,
            upstream_id: None,
            upstream_name: Some("provider".to_string()),
            upstream_key_id: None,
            upstream_key_name: Some("key".to_string()),
            status: Some(200),
            outcome: "success".to_string(),
            error_class: None,
            error_message: None,
            attempts: 1,
            input_tokens: None,
            output_tokens: None,
            total_tokens: None,
        },
        &[],
    )
    .await
    .unwrap();
    let audit_handle = StreamUsageAuditHandle::new();
    audit_handle.set_audit_id(audit_id);
    let upstream =
        futures_util::stream::iter(vec![Ok(Bytes::from_static(b"data: {not-json}\n\n"))]);

    let body = convert_streaming_response(pool.clone(), upstream, request, None, audit_handle);
    let _ = axum::body::to_bytes(body, 1024 * 1024).await.unwrap();

    let row = sqlx::query(
        r#"
        SELECT outcome, error_class, error_message
        FROM request_audits
        WHERE id = ?;
        "#,
    )
    .bind(audit_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(row.get::<String, _>("outcome"), "response_stream_error");
    assert_eq!(
        row.get::<String, _>("error_class"),
        "response_stream_conversion_error"
    );
    assert!(!row.get::<String, _>("error_message").is_empty());

    pool.close().await;
    remove_sqlite_files(path);
}

fn temp_sqlite_path(name: &str) -> std::path::PathBuf {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("{name}-{}-{unique}.sqlite", std::process::id()))
}

fn remove_sqlite_files(path: std::path::PathBuf) {
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(path.with_extension("sqlite-shm"));
    let _ = std::fs::remove_file(path.with_extension("sqlite-wal"));
}
