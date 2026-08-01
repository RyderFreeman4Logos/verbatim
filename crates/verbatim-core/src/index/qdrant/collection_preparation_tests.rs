use std::sync::mpsc;
use std::time::Duration;

struct ScriptedQdrantServer {
    url: String,
    stop: Option<mpsc::Sender<()>>,
    handle: Option<thread::JoinHandle<Vec<TestRequest>>>,
}

impl ScriptedQdrantServer {
    fn spawn(responses: Vec<(u16, String)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind scripted qdrant server");
        listener
            .set_nonblocking(true)
            .expect("set scripted qdrant server nonblocking");
        let address = listener.local_addr().expect("scripted qdrant server addr");
        let (stop_tx, stop_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        requests.push(read_request(&mut stream));
                        let (status, body) = responses
                            .get(requests.len() - 1)
                            .map(|(status, body)| (*status, body.as_str()))
                            .unwrap_or((500, r#"{"status":{"error":"unexpected request"}}"#));
                        write_response(&mut stream, status, body);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if stop_rx.try_recv().is_ok() {
                            break;
                        }
                        thread::sleep(Duration::from_millis(1));
                    }
                    Err(error) => panic!("accept scripted qdrant request: {error}"),
                }
            }
            requests
        });
        Self {
            url: format!("http://{address}"),
            stop: Some(stop_tx),
            handle: Some(handle),
        }
    }

    fn finish(mut self) -> Vec<TestRequest> {
        self.stop.take().expect("scripted server stop").send(()).ok();
        self.handle
            .take()
            .expect("scripted server handle")
            .join()
            .expect("join scripted qdrant server")
    }
}

impl Drop for ScriptedQdrantServer {
    fn drop(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.send(()).ok();
        }
        if let Some(handle) = self.handle.take() {
            handle.join().ok();
        }
    }
}

fn collection_info(dimension: usize, distance: &str, payload_schema: Value) -> String {
    serde_json::json!({
        "status": "ok",
        "result": {
            "config": {
                "params": {
                    "vectors": {
                        "size": dimension,
                        "distance": distance
                    }
                }
            },
            "payload_schema": payload_schema
        }
    })
    .to_string()
}

fn successful_mutation() -> String {
    r#"{"status":"ok","result":{"status":"acknowledged","operation_id":1}}"#.into()
}

fn keyword_indexes() -> Value {
    serde_json::json!({
        "profile_id": {"data_type": "keyword"},
        "source_id": {"data_type": "keyword"}
    })
}

#[tokio::test]
async fn upsert_records_creates_collection_and_sends_payload() {
    let server = ScriptedQdrantServer::spawn(vec![
        (404, r#"{"status":{"error":"missing"},"result":null}"#.into()),
        (200, r#"{"status":"ok","result":true}"#.into()),
        (200, collection_info(2, "Cosine", serde_json::json!({}))),
        (200, successful_mutation()),
        (200, successful_mutation()),
        (200, collection_info(2, "Cosine", keyword_indexes())),
        (200, successful_mutation()),
    ]);
    let client = QdrantClient::new(qdrant_config(server.url.clone()));

    client
        .upsert_records(&[record("src-1", "src-1-child-0", vec![0.1, 0.2])])
        .await
        .unwrap();
    let requests = server.finish();

    assert_eq!(requests[0].line, "GET /collections/verbatim HTTP/1.1");
    assert_eq!(requests[1].line, "PUT /collections/verbatim HTTP/1.1");
    let create: Value = serde_json::from_str(&requests[1].body).unwrap();
    assert_eq!(create["vectors"]["size"], 2);
    assert_eq!(create["vectors"]["distance"], "Cosine");
    assert_eq!(
        requests[6].line,
        "PUT /collections/verbatim/points?wait=true HTTP/1.1"
    );
    let upsert: Value = serde_json::from_str(&requests[6].body).unwrap();
    assert_eq!(upsert["points"][0]["payload"]["profile_id"], "default");
    assert_eq!(upsert["points"][0]["payload"]["profile_generation"], 7);
    assert_eq!(upsert["points"][0]["payload"]["chunk_id"], "src-1-child-0");
    assert_eq!(upsert["points"][0]["payload"]["source_id"], "src-1");
    assert_eq!(upsert["points"][0]["payload"]["heading_path"][0], "Intro");
    assert_eq!(
        upsert["points"][0]["payload"]["text_preview"],
        "preview text"
    );
    assert_ne!(upsert["points"][0]["id"], "src-1-child-0");
}

#[tokio::test]
async fn upsert_records_rejects_existing_collection_dimension_and_metric_mismatch() {
    for (info, expected, actual) in [
        (collection_info(3, "Cosine", keyword_indexes()), "dimension 2", "3"),
        (collection_info(2, "Dot", keyword_indexes()), "distance Cosine", "Dot"),
    ] {
        let server = ScriptedQdrantServer::spawn(vec![(200, info), (200, successful_mutation())]);
        let client = QdrantClient::new(qdrant_config(server.url.clone()));

        let error = client
            .upsert_records(&[record("src-1", "src-1-child-0", vec![0.1, 0.2])])
            .await
            .expect_err("incompatible collection schema must fail before upsert");
        let requests = server.finish();

        assert!(error.to_string().contains(expected), "{error:#}");
        assert!(error.to_string().contains(actual), "{error:#}");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].line, "GET /collections/verbatim HTTP/1.1");
    }
}

#[tokio::test]
async fn upsert_records_rejects_malformed_or_ambiguous_collection_info() {
    let malformed = [
        r#"{"status":"ok","result":null}"#.to_owned(),
        r#"{"status":"ok","result":{}}"#.to_owned(),
        serde_json::json!({
            "status": "ok",
            "result": {
                "config": {"params": {"vectors": {"named": {"size": 2, "distance": "Cosine"}}}},
                "payload_schema": {}
            }
        })
        .to_string(),
        serde_json::json!({
            "status": "ok",
            "result": {
                "config": {"params": {"vectors": {
                    "size": 2,
                    "distance": "Cosine",
                    "named": {"size": 2, "distance": "Cosine"}
                }}},
                "payload_schema": {}
            }
        })
        .to_string(),
        serde_json::json!({
            "status": "ok",
            "result": {"config": {"params": {"vectors": {"size": 2, "distance": "Cosine"}}}}
        })
        .to_string(),
    ];

    for info in malformed {
        let server = ScriptedQdrantServer::spawn(vec![(200, info), (200, successful_mutation())]);
        let client = QdrantClient::new(qdrant_config(server.url.clone()));

        let error = client
            .upsert_records(&[record("src-1", "src-1-child-0", vec![0.1, 0.2])])
            .await
            .expect_err("malformed collection info must fail closed");
        let requests = server.finish();

        assert!(error.to_string().contains("collection schema"), "{error:#}");
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].line, "GET /collections/verbatim HTTP/1.1");
    }
}

#[tokio::test]
async fn upsert_records_creates_and_verifies_keyword_payload_indexes() {
    let server = ScriptedQdrantServer::spawn(vec![
        (200, collection_info(2, "Cosine", serde_json::json!({}))),
        (200, successful_mutation()),
        (200, successful_mutation()),
        (200, collection_info(2, "Cosine", keyword_indexes())),
        (200, successful_mutation()),
    ]);
    let client = QdrantClient::new(qdrant_config(server.url.clone()));

    client
        .upsert_records(&[record("src-1", "src-1-child-0", vec![0.1, 0.2])])
        .await
        .unwrap();
    let requests = server.finish();

    assert_eq!(
        requests.iter().map(|request| request.line.as_str()).collect::<Vec<_>>(),
        [
            "GET /collections/verbatim HTTP/1.1",
            "PUT /collections/verbatim/index?wait=true HTTP/1.1",
            "PUT /collections/verbatim/index?wait=true HTTP/1.1",
            "GET /collections/verbatim HTTP/1.1",
            "PUT /collections/verbatim/points?wait=true HTTP/1.1",
        ]
    );
    for (request, field_name) in requests[1..3].iter().zip(["profile_id", "source_id"]) {
        let body: Value = serde_json::from_str(&request.body).unwrap();
        assert_eq!(body, serde_json::json!({
            "field_name": field_name,
            "field_schema": "keyword"
        }));
    }
}

#[tokio::test]
async fn upsert_records_reuses_matching_keyword_payload_indexes() {
    let server = ScriptedQdrantServer::spawn(vec![
        (200, collection_info(2, "Cosine", keyword_indexes())),
        (200, successful_mutation()),
    ]);
    let client = QdrantClient::new(qdrant_config(server.url.clone()));

    client
        .upsert_records(&[record("src-1", "src-1-child-0", vec![0.1, 0.2])])
        .await
        .unwrap();
    let requests = server.finish();

    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].line, "GET /collections/verbatim HTTP/1.1");
    assert_eq!(
        requests[1].line,
        "PUT /collections/verbatim/points?wait=true HTTP/1.1"
    );
}

#[tokio::test]
async fn upsert_records_rejects_wrong_payload_index_type_before_mutation() {
    let server = ScriptedQdrantServer::spawn(vec![
        (
            200,
            collection_info(
                2,
                "Cosine",
                serde_json::json!({
                    "profile_id": {"data_type": "integer"},
                    "source_id": {"data_type": "text"}
                }),
            ),
        ),
        (200, successful_mutation()),
    ]);
    let client = QdrantClient::new(qdrant_config(server.url.clone()));

    let error = client
        .upsert_records(&[record("src-1", "src-1-child-0", vec![0.1, 0.2])])
        .await
        .expect_err("wrong payload index type must fail before mutation");
    let requests = server.finish();

    assert!(error.to_string().contains("profile_id"), "{error:#}");
    assert!(error.to_string().contains("expected keyword"), "{error:#}");
    assert!(error.to_string().contains("integer"), "{error:#}");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].line, "GET /collections/verbatim HTTP/1.1");
}

#[tokio::test]
async fn upsert_records_validates_all_existing_payload_indexes_before_mutation() {
    for (payload_schema, field_name, actual) in [
        (
            serde_json::json!({"profile_id": {}, "source_id": {"data_type": "keyword"}}),
            "profile_id",
            "data_type string",
        ),
        (
            serde_json::json!({"source_id": {"data_type": "integer"}}),
            "source_id",
            "integer",
        ),
    ] {
        let server = ScriptedQdrantServer::spawn(vec![
            (200, collection_info(2, "Cosine", payload_schema)),
            (200, successful_mutation()),
        ]);
        let client = QdrantClient::new(qdrant_config(server.url.clone()));

        let error = client
            .upsert_records(&[record("src-1", "src-1-child-0", vec![0.1, 0.2])])
            .await
            .expect_err("malformed or wrong payload indexes must fail before mutation");
        let requests = server.finish();

        assert!(error.to_string().contains(field_name), "{error:#}");
        assert!(error.to_string().contains(actual), "{error:#}");
        assert_eq!(requests.len(), 1);
    }
}

#[tokio::test]
async fn upsert_records_requires_verified_payload_indexes_after_successful_puts() {
    let server = ScriptedQdrantServer::spawn(vec![
        (200, collection_info(2, "Cosine", serde_json::json!({}))),
        (200, successful_mutation()),
        (200, successful_mutation()),
        (
            200,
            collection_info(
                2,
                "Cosine",
                serde_json::json!({"profile_id": {"data_type": "keyword"}}),
            ),
        ),
        (200, successful_mutation()),
    ]);
    let client = QdrantClient::new(qdrant_config(server.url.clone()));

    let error = client
        .upsert_records(&[record("src-1", "src-1-child-0", vec![0.1, 0.2])])
        .await
        .expect_err("successful index responses require collection verification");
    let requests = server.finish();

    assert!(error.to_string().contains("verification failed"), "{error:#}");
    assert!(error.to_string().contains("source_id"), "{error:#}");
    assert_eq!(requests.len(), 4);
    assert!(requests
        .iter()
        .all(|request| request.line != "PUT /collections/verbatim/points?wait=true HTTP/1.1"));
}

#[tokio::test]
async fn upsert_records_retries_after_partial_payload_index_creation() {
    let server = ScriptedQdrantServer::spawn(vec![
        (200, collection_info(2, "Cosine", serde_json::json!({}))),
        (200, successful_mutation()),
        (
            500,
            r#"{"status":{"error":"source index unavailable"},"result":null}"#.into(),
        ),
        (
            200,
            collection_info(
                2,
                "Cosine",
                serde_json::json!({"profile_id": {"data_type": "keyword"}}),
            ),
        ),
        (200, successful_mutation()),
        (200, collection_info(2, "Cosine", keyword_indexes())),
        (200, successful_mutation()),
    ]);
    let client = QdrantClient::new(qdrant_config(server.url.clone()));
    let records = [record("src-1", "src-1-child-0", vec![0.1, 0.2])];

    client
        .upsert_records(&records)
        .await
        .expect_err("partial index creation must not upsert");
    client.upsert_records(&records).await.unwrap();
    let requests = server.finish();

    assert_eq!(
        requests.iter().map(|request| request.line.as_str()).collect::<Vec<_>>(),
        [
            "GET /collections/verbatim HTTP/1.1",
            "PUT /collections/verbatim/index?wait=true HTTP/1.1",
            "PUT /collections/verbatim/index?wait=true HTTP/1.1",
            "GET /collections/verbatim HTTP/1.1",
            "PUT /collections/verbatim/index?wait=true HTTP/1.1",
            "GET /collections/verbatim HTTP/1.1",
            "PUT /collections/verbatim/points?wait=true HTTP/1.1",
        ]
    );
    let retry_body: Value = serde_json::from_str(&requests[4].body).unwrap();
    assert_eq!(retry_body["field_name"], "source_id");
}

#[tokio::test]
async fn upsert_records_prepares_new_collection_before_upsert() {
    let server = ScriptedQdrantServer::spawn(vec![
        (404, r#"{"status":{"error":"missing"},"result":null}"#.into()),
        (200, r#"{"status":"ok","result":false}"#.into()),
        (200, collection_info(2, "Cosine", serde_json::json!({}))),
        (200, successful_mutation()),
        (200, successful_mutation()),
        (200, collection_info(2, "Cosine", keyword_indexes())),
        (200, successful_mutation()),
    ]);
    let client = QdrantClient::new(qdrant_config(server.url.clone()));

    client
        .upsert_records(&[record("src-1", "src-1-child-0", vec![0.1, 0.2])])
        .await
        .unwrap();
    let requests = server.finish();

    assert_eq!(
        requests.iter().map(|request| request.line.as_str()).collect::<Vec<_>>(),
        [
            "GET /collections/verbatim HTTP/1.1",
            "PUT /collections/verbatim HTTP/1.1",
            "GET /collections/verbatim HTTP/1.1",
            "PUT /collections/verbatim/index?wait=true HTTP/1.1",
            "PUT /collections/verbatim/index?wait=true HTTP/1.1",
            "GET /collections/verbatim HTTP/1.1",
            "PUT /collections/verbatim/points?wait=true HTTP/1.1",
        ]
    );
}
