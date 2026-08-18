use super::*;

/// Serves capability discovery (with the given `support_dimensions` value, if any), then a single
/// embedding response of `embedding_len` dimensions. Records every request.
fn spawn_discovery_then_embedding_server(
    support_dimensions: Option<bool>,
    embedding_len: usize,
) -> (
    String,
    Arc<Mutex<mpsc::Receiver<RecordedRequest>>>,
    thread::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
    let base_url = format!("http://{}", listener.local_addr().expect("server addr"));
    let (request_tx, request_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept discovery request");
        let request = read_http_request(&mut stream);
        request_tx.send(request).expect("record discovery request");
        let discovery = match support_dimensions {
            Some(value) => {
                format!(r#"{{"data":[{{"id":"embedding-model","support_dimensions":{value}}}]}}"#)
            }
            None => r#"{"data":[{"id":"embedding-model"}]}"#.to_string(),
        };
        write_http_response(&mut stream, "200 OK", "application/json", &discovery);

        let (mut stream, _) = listener.accept().expect("accept embedding request");
        let request = read_http_request(&mut stream);
        request_tx.send(request).expect("record embedding request");
        let embedding = format!(
            r#"{{"data":[{{"embedding":[{}]}}]}}"#,
            vec!["0.5"; embedding_len].join(",")
        );
        write_http_response(&mut stream, "200 OK", "application/json", &embedding);
    });
    (base_url, Arc::new(Mutex::new(request_rx)), handle)
}

#[tokio::test]
async fn embedding_request_emits_configured_dimensions_when_capability_supports_it() {
    let (base_url, request_rx, handle) = spawn_discovery_then_embedding_server(Some(true), 1024);
    let runtime = test_runtime_config(1, 0, 1);
    let mut model = embedding_model_with_runtime(&base_url, &runtime);
    model.dimension = 1024;

    // Seed the capability cache from model discovery so `embed_prepared` sees support.
    model
        .endpoint_capabilities()
        .await
        .expect("capability loads");

    let embeddings = model
        .embed_prepared(vec!["document".into()])
        .await
        .expect("embedding succeeds");

    assert_eq!(embeddings[0].len(), 1024);

    let requests = collect_recorded_requests(request_rx, 2).await;
    handle.join().expect("server thread joins");
    assert_eq!(requests[0].path, "/v1/models");
    let embedding_body: serde_json::Value =
        serde_json::from_str(&requests[1].body).expect("embedding body");
    assert_eq!(embedding_body["dimensions"], 1024);
}

#[tokio::test]
async fn embedding_request_omits_dimensions_when_capability_does_not_support_it() {
    let (base_url, request_rx, handle) = spawn_discovery_then_embedding_server(Some(false), 3);
    let runtime = test_runtime_config(1, 0, 1);
    let model = embedding_model_with_runtime(&base_url, &runtime);

    model
        .endpoint_capabilities()
        .await
        .expect("capability loads");

    let embeddings = model
        .embed_prepared(vec!["document".into()])
        .await
        .expect("embedding succeeds");

    assert_eq!(embeddings, vec![vec![0.5, 0.5, 0.5]]);

    let requests = collect_recorded_requests(request_rx, 2).await;
    handle.join().expect("server thread joins");
    let embedding_body: serde_json::Value =
        serde_json::from_str(&requests[1].body).expect("embedding body");
    assert!(embedding_body.get("dimensions").is_none());
}

#[tokio::test]
async fn embedding_request_does_not_duplicate_instruction_owned_by_prepared_input() {
    let (base_url, request_rx, handle) = spawn_discovery_then_embedding_server(None, 3);
    let runtime = test_runtime_config(1, 0, 1);
    let mut model = embedding_model_with_runtime(&base_url, &runtime);
    model.query_instruction = "search the query".into();

    model
        .endpoint_capabilities()
        .await
        .expect("capability loads");

    // The prepared input already owns the instruction text.
    let prepared = model.prepare_query("my query");
    assert_eq!(prepared, "Instruct: search the query\nQuery: my query");

    model
        .embed_prepared(vec![prepared])
        .await
        .expect("embedding succeeds");

    let requests = collect_recorded_requests(request_rx, 2).await;
    handle.join().expect("server thread joins");
    let embedding_body: serde_json::Value =
        serde_json::from_str(&requests[1].body).expect("embedding body");
    assert!(embedding_body.get("instruction").is_none());
    assert!(embedding_body["input"][0]
        .as_str()
        .expect("input text")
        .contains("search the query"));
}

#[test]
fn serializes_text_chat_request_shape() {
    let model = OpenAiCompatibleChatModel {
        endpoint: OpenAiEndpoint::new("http://127.0.0.1:8000/v1", "model", "", 120),
        temperature: 0.2,
    };
    let body = model.chat_body(
        ChatRequest::new(vec![
            ChatMessage::system("system prompt"),
            ChatMessage::user("user prompt"),
        ])
        .with_max_tokens(42),
        false,
    );

    let value = serde_json::to_value(body).expect("serialize chat request");

    assert_eq!(value["model"], "model");
    assert_eq!(value["stream"], false);
    assert_eq!(value["max_tokens"], 42);
    assert_eq!(value["messages"][0]["role"], "system");
    assert_eq!(value["messages"][1]["content"], "user prompt");
}

#[test]
fn serializes_vision_message_with_data_uri() {
    let message = ChatMessage::user_parts(vec![
        ChatContentPart::Text {
            text: "Describe".into(),
        },
        ChatContentPart::ImageUrl {
            image_url: ImageUrl {
                url: ImageInput::data_uri("data:image/png;base64,abc").to_openai_url(),
                detail: Some("high".into()),
            },
        },
    ]);

    let value = serde_json::to_value(message).expect("serialize vision message");

    assert_eq!(value["role"], "user");
    assert_eq!(value["content"][0]["type"], "text");
    assert_eq!(value["content"][1]["type"], "image_url");
    assert_eq!(
        value["content"][1]["image_url"]["url"],
        "data:image/png;base64,abc"
    );
    assert_eq!(value["content"][1]["image_url"]["detail"], "high");
}
