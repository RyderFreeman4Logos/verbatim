    #[test]
    fn http_collection_routes_are_plumbed_from_shared_inventory() {
        let collection = serde_json::to_string(
            &CollectionResponse::new(
                serde_json::from_str(
                    "{\"name\":\"articles\",\"created_at\":\"1\",\"updated_at\":\"2\"}",
                )
                .unwrap(),
                Vec::new(),
                Vec::new(),
            )
            .unwrap(),
        )
        .unwrap();
        let collection_root = COLLECTION_ROOT_RESPONSE;
        let collection_list = serde_json::to_string(
            &CollectionListResponse::new(vec![
                serde_json::from_str(
                    "{\"name\":\"articles\",\"created_at\":\"1\",\"updated_at\":\"2\"}",
                )
                .unwrap(),
            ])
            .unwrap(),
        )
        .unwrap();
        let sync = COLLECTION_SYNC_RESPONSE;
        let status = COLLECTION_STATUS_RESPONSE;
        let watcher_status = concat!(
            "{\"collection_name\":\"articles\",\"watch_enabled\":true,",
            "\"auto_index_enabled\":false,\"active\":true,\"ignored_by_config\":false,",
            "\"watched_root_count\":1,\"pending_event_count\":0}"
        );
        let watcher = serde_json::to_string(
            &CollectionWatcherResponse::new(
                "articles",
                serde_json::from_str(
                    "{\"name\":\"articles\",\"created_at\":\"1\",\"updated_at\":\"2\"}",
                )
                .unwrap(),
                serde_json::from_str(watcher_status).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let watchers = serde_json::to_string(
            &CollectionWatchersStatusResponse::new(vec![
                serde_json::from_str(watcher_status).unwrap()
            ])
            .unwrap(),
        )
        .unwrap();
        let server = TestServer::respond_many(vec![
            json_response("201 Created", &collection),
            json_response("200 OK", collection_root),
            json_response("200 OK", &collection_list),
            json_response("200 OK", &collection),
            "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_string(),
            json_response("200 OK", sync),
            json_response("200 OK", &status),
            json_response("200 OK", watchers.as_str()),
            json_response("200 OK", watcher.as_str()),
            json_response("200 OK", watcher.as_str()),
        ]);
        let client = HttpDaemonClient::with_base_url(server.base_url());

        assert_eq!(
            client
                .create_collection(&CreateCollectionRequest {
                    name: "articles".into(),
                    ignore_patterns: vec!["drafts/".into()],
                })
                .unwrap()
                .collection
                .name,
            "articles"
        );
        assert!(
            client
                .add_collection_root(
                    "articles",
                    &AddCollectionRootRequest {
                        path: "/tmp/articles".into(),
                    },
                )
                .unwrap()
                .added
        );
        assert_eq!(client.list_collections().unwrap()[0].name, "articles");
        assert_eq!(
            client.get_collection("articles").unwrap().collection.name,
            "articles"
        );
        client.delete_collection("articles").unwrap();
        assert_eq!(
            client
                .sync_collection(
                    "articles",
                    &CollectionSyncRequest {
                        paths: Vec::new(),
                        max_depth: Some(7),
                    },
                )
                .unwrap()
                .report
                .member_count,
            1
        );
        assert_eq!(
            client
                .collection_status("articles")
                .unwrap()
                .status
                .member_count,
            1
        );
        assert_eq!(
            client.list_collection_watcher_statuses().unwrap().watchers[0].collection_name,
            "articles"
        );
        let watcher_response = client.collection_watcher_status("articles").unwrap();
        assert!(watcher_response.watcher.active);
        assert_eq!(
            watcher_response.identity.kind.as_str(),
            "collection_watcher_result"
        );
        assert_eq!(watcher_response.identity.schema_version.to_string(), "1.0.0");
        assert_eq!(watcher_response.identity.artifact_id, "articles");
        watcher_response.validate_for_collection("articles").unwrap();
        let updated_watcher_response = client
            .update_collection_watcher(
                "articles",
                &CollectionWatcherUpdateRequest {
                    enabled: true,
                    auto_index_enabled: Some(false),
                },
            )
            .unwrap();
        assert!(updated_watcher_response.watcher.watch_enabled);
        assert_eq!(
            updated_watcher_response.identity.kind.as_str(),
            "collection_watcher_result"
        );
        assert_eq!(
            updated_watcher_response.identity.schema_version.to_string(),
            "1.0.0"
        );
        assert_eq!(updated_watcher_response.identity.artifact_id, "articles");
        updated_watcher_response
            .validate_for_collection("articles")
            .unwrap();

        let requests = server.requests();
        assert_collection_request(&requests[0], CollectionApiEndpoint::CreateCollection, None);
        assert!(requests[0].contains("\"ignore_patterns\":[\"drafts/\"]"));
        assert_collection_request(
            &requests[1],
            CollectionApiEndpoint::AddCollectionRoot,
            Some("articles"),
        );
        assert!(requests[1].contains("\"path\":\"/tmp/articles\""));
        assert_collection_request(&requests[2], CollectionApiEndpoint::ListCollections, None);
        assert_collection_request(
            &requests[3],
            CollectionApiEndpoint::GetCollection,
            Some("articles"),
        );
        assert_collection_request(
            &requests[4],
            CollectionApiEndpoint::DeleteCollection,
            Some("articles"),
        );
        assert_collection_request(
            &requests[5],
            CollectionApiEndpoint::SyncCollection,
            Some("articles"),
        );
        assert!(requests[5].contains("\"max_depth\":7"));
        assert_collection_request(
            &requests[6],
            CollectionApiEndpoint::CollectionStatus,
            Some("articles"),
        );
        assert_collection_request(
            &requests[7],
            CollectionApiEndpoint::ListCollectionWatcherStatuses,
            None,
        );
        assert_collection_request(
            &requests[8],
            CollectionApiEndpoint::CollectionWatcherStatus,
            Some("articles"),
        );
        assert_collection_request(
            &requests[9],
            CollectionApiEndpoint::UpdateCollectionWatcher,
            Some("articles"),
        );
        assert!(requests[9].contains("\"enabled\":true"));
    }

    #[test]
    fn collection_list_result_identity_rejects_invalid_identity() {
        let mut response = serde_json::to_value(CollectionListResponse::new(Vec::new()).unwrap())
            .unwrap();
        response["identity"]["content_hash"] = Value::String("0".repeat(64));
        let body = serde_json::to_string(&response).unwrap();
        let server = TestServer::respond_many(vec![json_response("200 OK", &body)]);

        let error = HttpDaemonClient::with_base_url(server.base_url())
            .list_collections()
            .expect_err("invalid collection-list identity must fail closed");
        assert!(error.to_string().contains("invalid JSON"));
    }
