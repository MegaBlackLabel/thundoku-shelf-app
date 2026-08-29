//! TbfClient tests against a scripted mock transport (no network).

use parking_lot::Mutex;
use std::sync::Arc;
use thundoku_core::tbf::{RequestSpec, ResponseSpec, TbfClient, TbfError, Transport};

type MockFn = Box<dyn FnMut(RequestSpec) -> Result<ResponseSpec, TbfError> + Send>;

struct Mock(MockFn);

impl Transport for Mock {
    fn send(&mut self, spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
        (self.0)(spec)
    }
}

fn response(status: u16, body: &str) -> ResponseSpec {
    ResponseSpec {
        status,
        headers: vec![],
        body: body.as_bytes().to_vec(),
    }
}

fn json_body(spec: &RequestSpec) -> serde_json::Value {
    serde_json::from_slice(spec.body.as_deref().unwrap_or(b"null")).expect("valid JSON body")
}

fn header_value<'a>(spec: &'a RequestSpec, name: &str) -> Option<&'a str> {
    spec.headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

#[test]
fn bootstrap_captures_and_decodes_xsrf() {
    let captured = Arc::new(Mutex::new(Vec::<RequestSpec>::new()));
    let log = captured.clone();
    let mut client = TbfClient::with_transport(Box::new(Mock(Box::new(move |spec| {
        log.lock().push(spec.clone());
        Ok(ResponseSpec {
            status: 200,
            headers: vec![(
                "set-cookie".into(),
                "XSRF-TOKEN=abc%3D123%2F; Path=/; Secure".into(),
            )],
            body: b"<html></html>".to_vec(),
        })
    }))));
    client.bootstrap().unwrap();
    let guard = captured.lock();
    let req = &guard[0];
    assert_eq!(req.url, "https://techbookfest.org/");
    assert!(header_value(req, "User-Agent").unwrap().contains("Mozilla"));
    let session = client.session().unwrap();
    assert_eq!(session.xsrf_raw, "abc%3D123%2F");
    assert_eq!(session.xsrf_token, "abc=123/");
    assert!(
        session
            .cookies
            .iter()
            .any(|(n, v)| n == "XSRF-TOKEN" && v == "abc%3D123%2F")
    );
}

#[test]
fn login_success_sends_captured_mutation_and_stores_session_cookie() {
    let captured = Arc::new(Mutex::new(Vec::<RequestSpec>::new()));
    let log = captured.clone();
    let mut client = TbfClient::with_transport(Box::new(Mock(Box::new(move |spec| {
        log.lock().push(spec.clone());
        let body = json_body(&spec);
        // serve bootstrap first, then login
        if spec.url == "https://techbookfest.org/" {
            return Ok(ResponseSpec {
                status: 200,
                headers: vec![("set-cookie".into(), "XSRF-TOKEN=abc123; Path=/".into())],
                body: b"ok".to_vec(),
            });
        }
        assert_eq!(body["operationName"], "UserLoginMutation");
        assert_eq!(body["variables"]["loginInput"]["email"], "user@example.com");
        assert_eq!(body["variables"]["loginInput"]["password"], "secret");
        assert_eq!(
            body["extensions"]["clientLibrary"]["name"],
            "@apollo/client"
        );
        assert!(
            body["query"]
                .as_str()
                .unwrap()
                .contains("mutation UserLoginMutation")
        );
        assert_eq!(header_value(&spec, "X-XSRF-TOKEN"), Some("abc123"));
        assert!(
            header_value(&spec, "Cookie")
                .unwrap()
                .contains("XSRF-TOKEN=abc123")
        );
        Ok(ResponseSpec {
            status: 200,
            headers: vec![(
                "set-cookie".into(),
                "session=s3ss10n; Path=/; HttpOnly".into(),
            )],
            body: r#"{"data":{"loginUser":{"user":{"id":"user-1"}}}}"#.into(),
        })
    }))));

    client.bootstrap().unwrap();
    client.login("user@example.com", "secret").unwrap();
    let session = client.session().unwrap();
    assert!(
        session
            .cookies
            .iter()
            .any(|(n, v)| n == "session" && v == "s3ss10n")
    );
}

#[test]
fn login_failure_returns_invalid_credentials() {
    let mut client = TbfClient::with_transport(Box::new(Mock(Box::new(|spec| {
        if spec.url == "https://techbookfest.org/" {
            return Ok(ResponseSpec {
                status: 200,
                headers: vec![("set-cookie".into(), "XSRF-TOKEN=abc123; Path=/".into())],
                body: b"ok".to_vec(),
            });
        }
        Ok(response(
            200,
            r#"{"errors":[{"message":"Unauthorized","extensions":{"code":"UNAUTHORIZED"}}]}"#,
        ))
    }))));
    client.bootstrap().unwrap();
    assert!(matches!(
        client.login("user@example.com", "wrong"),
        Err(TbfError::InvalidCredentials)
    ));
}

#[test]
fn download_with_progress_reports_full_body_and_cookies() {
    let captured = Arc::new(Mutex::new(Vec::<RequestSpec>::new()));
    let log = captured.clone();
    let mut client = TbfClient::with_transport(Box::new(Mock(Box::new(move |spec| {
        log.lock().push(spec.clone());
        Ok(ResponseSpec {
            status: 200,
            headers: vec![],
            body: vec![1, 2, 3, 4, 5],
        })
    }))));
    client.restore_session(thundoku_core::tbf::TbfSession {
        cookies: vec![("session".into(), "s".into())],
        xsrf_raw: "x".into(),
        xsrf_token: "x".into(),
    });
    let mut progress: Vec<(u64, u64)> = Vec::new();
    let bytes = client
        .download_with_progress("https://example.com/file.pdf", &mut |downloaded, total| {
            progress.push((downloaded, total));
        })
        .unwrap();
    assert_eq!(bytes, vec![1, 2, 3, 4, 5]);
    // Default transport reports the whole body once; session cookies and the
    // XSRF header must be attached to the download request.
    assert_eq!(progress, vec![(5, 5)]);
    let req = &captured.lock()[0];
    assert_eq!(header_value(req, "Cookie"), Some("session=s"));
    assert_eq!(header_value(req, "X-XSRF-TOKEN"), Some("x"));
}

#[test]
fn bookshelf_paginates_and_maps_items() {
    let page1 = r#"{
      "data": { "viewer": { "bookShelfItems": {
        "pageInfo": { "hasNextPage": true, "endCursor": "CURSOR-1" },
        "edges": [
          { "node": {
              "id": "shelf-1",
              "causedAt": "2026-04-12T09:16:36.410Z",
              "product": {
                "databaseID": "db-1",
                "name": "React 本",
                "organization": { "name": "サークルA" },
                "coverImage": { "url": "/images/cover1.png" },
                "downloadContent": { "fileName": "react-book.pdf", "downloadURL": "/api/product-dlc/db-1/download" }
              },
              "marketHandshake": { "event": { "id": "Event:tbf20", "name": "技術書典20" } }
          } }
        ]
      } } }
    }"#;
    let page2 = r#"{
      "data": { "viewer": { "bookShelfItems": {
        "pageInfo": { "hasNextPage": false, "endCursor": null },
        "edges": [
          { "node": {
              "id": "shelf-2",
              "causedAt": null,
              "product": {
                "databaseID": "db-2",
                "name": "第二冊",
                "organization": { "name": "サークルB" },
                "coverImage": null,
                "downloadContent": null
              },
              "marketHandshake": null
          } }
        ]
      } } }
    }"#;
    let captured = Arc::new(Mutex::new(Vec::<RequestSpec>::new()));
    let log = captured.clone();
    let mut client = TbfClient::with_transport(Box::new(Mock(Box::new(move |spec| {
        log.lock().push(spec.clone());
        let body = json_body(&spec);
        let after = body["variables"]["after"].as_str().map(String::from);
        Ok(response(200, if after.is_none() { page1 } else { page2 }))
    }))));
    client.restore_session(thundoku_core::tbf::TbfSession {
        cookies: vec![
            ("session".into(), "s".into()),
            ("XSRF-TOKEN".into(), "abc123".into()),
        ],
        xsrf_raw: "abc123".into(),
        xsrf_token: "abc123".into(),
    });

    let books = client.bookshelf().unwrap();
    assert_eq!(books.len(), 2);
    assert_eq!(books[0].title, "React 本");
    assert_eq!(books[0].circle_name, "サークルA");
    assert_eq!(
        books[0].thumbnail_url.as_deref(),
        Some("https://techbookfest.org/images/cover1.png")
    );
    assert_eq!(books[0].format, "PDF");
    assert_eq!(
        books[0].download_url.as_deref(),
        Some("https://techbookfest.org/api/product-dlc/db-1/download")
    );
    assert_eq!(books[0].event_slug.as_deref(), Some("tbf20"));
    assert!(books[0].is_downloadable);
    assert_eq!(books[1].format, "BOOK");
    assert!(!books[1].is_downloadable);

    // second page requested with cursor
    let reqs = captured.lock();
    assert_eq!(reqs.len(), 2);
    assert_eq!(
        json_body(&reqs[0])["variables"]["after"],
        serde_json::Value::Null
    );
    let second_body = json_body(&reqs[1]);
    assert_eq!(second_body["variables"]["after"], "CURSOR-1");
    assert!(reqs[0].url.contains("operationName=BookShelfQuery"));
}

#[test]
fn bookshelf_401_is_session_expired() {
    let mut client = TbfClient::with_transport(Box::new(Mock(Box::new(|_| Ok(response(401, ""))))));
    client.restore_session(thundoku_core::tbf::TbfSession {
        cookies: vec![],
        xsrf_raw: "x".into(),
        xsrf_token: "x".into(),
    });
    assert!(matches!(client.bookshelf(), Err(TbfError::SessionExpired)));
}

#[test]
fn resolve_download_url_follows_nothing_and_absolutizes() {
    let captured = Arc::new(Mutex::new(Vec::<RequestSpec>::new()));
    let log = captured.clone();
    let mut client = TbfClient::with_transport(Box::new(Mock(Box::new(move |spec| {
        log.lock().push(spec.clone());
        Ok(ResponseSpec {
            status: 302,
            headers: vec![(
                "location".into(),
                "/api/product-dlc/real-file.pdf?sig=abc".into(),
            )],
            body: vec![],
        })
    }))));
    client.restore_session(thundoku_core::tbf::TbfSession {
        cookies: vec![("session".into(), "s".into())],
        xsrf_raw: "x".into(),
        xsrf_token: "x".into(),
    });
    // The URL to resolve comes from the bookshelf item's `downloadURL`
    // (GraphQL `downloadContent.downloadURL`), which may use a DLC id that
    // differs from `database_id` — it must be resolved verbatim.
    let url = client
        .resolve_download_url("https://techbookfest.org/api/product-dlc/db-9/download")
        .unwrap();
    assert_eq!(
        url,
        "https://techbookfest.org/api/product-dlc/real-file.pdf?sig=abc"
    );
    let guard = captured.lock();
    let req = &guard[0];
    assert_eq!(req.redirects, 0);
    assert_eq!(
        req.url,
        "https://techbookfest.org/api/product-dlc/db-9/download"
    );
}

#[test]
fn resolve_download_url_accepts_gcs_location() {
    let mut client = TbfClient::with_transport(Box::new(Mock(Box::new(|_| {
        Ok(ResponseSpec {
            status: 302,
            headers: vec![(
                "location".into(),
                "https://storage.googleapis.com/tbf-tokyo-product-dlc/abc.pdf?sig=xyz".into(),
            )],
            body: vec![],
        })
    }))));
    client.restore_session(thundoku_core::tbf::TbfSession {
        cookies: vec![],
        xsrf_raw: "x".into(),
        xsrf_token: "x".into(),
    });
    let url = client
        .resolve_download_url("https://techbookfest.org/api/product-dlc/db-9/download")
        .unwrap();
    assert_eq!(
        url,
        "https://storage.googleapis.com/tbf-tokyo-product-dlc/abc.pdf?sig=xyz"
    );
}

#[test]
fn resolve_download_url_rejects_untrusted_location() {
    let mut client = TbfClient::with_transport(Box::new(Mock(Box::new(|_| {
        Ok(ResponseSpec {
            status: 302,
            headers: vec![("location".into(), "https://evil.example.com/pwn.pdf".into())],
            body: vec![],
        })
    }))));
    client.restore_session(thundoku_core::tbf::TbfSession {
        cookies: vec![],
        xsrf_raw: "x".into(),
        xsrf_token: "x".into(),
    });
    // Web の validateDownloadUrl と同一: 許可ホスト以外の Location は拒否
    assert!(matches!(
        client.resolve_download_url("https://techbookfest.org/api/product-dlc/db-9/download"),
        Err(TbfError::NotFound)
    ));
}

#[test]
fn resolve_download_url_401_is_session_expired() {
    let mut client = TbfClient::with_transport(Box::new(Mock(Box::new(|_| Ok(response(401, ""))))));
    client.restore_session(thundoku_core::tbf::TbfSession {
        cookies: vec![],
        xsrf_raw: "x".into(),
        xsrf_token: "x".into(),
    });
    assert!(matches!(
        client.resolve_download_url("https://techbookfest.org/api/product-dlc/db-9/download"),
        Err(TbfError::SessionExpired)
    ));
}

#[test]
fn sample_pages_map_and_absolutize_urls() {
    let body = r#"{
      "data": { "product": { "images": { "edges": [
        { "node": { "id": "i1", "databaseID": "d1", "url": "/images/s1.jpg", "width": 800, "height": 1200 } },
        { "node": { "id": "i2", "databaseID": "d2", "url": "https://cdn.example.com/s2.jpg", "width": null, "height": null } }
      ] } } }
    }"#;
    let captured = Arc::new(Mutex::new(Vec::<RequestSpec>::new()));
    let log = captured.clone();
    let mut client = TbfClient::with_transport(Box::new(Mock(Box::new(move |spec| {
        log.lock().push(spec.clone());
        Ok(response(200, body))
    }))));
    client.restore_session(thundoku_core::tbf::TbfSession {
        cookies: vec![],
        xsrf_raw: "x".into(),
        xsrf_token: "x".into(),
    });
    let pages = client.product_sample_pages("db-7").unwrap();
    assert_eq!(pages.len(), 2);
    assert_eq!(pages[0].page_number, 1);
    assert_eq!(pages[0].url, "https://techbookfest.org/images/s1.jpg");
    assert_eq!(pages[0].width, Some(800));
    assert_eq!(pages[1].page_number, 2);
    assert_eq!(pages[1].url, "https://cdn.example.com/s2.jpg");
    let guard = captured.lock();
    let req = &guard[0];
    let body = json_body(req);
    assert_eq!(body["variables"]["productInfoID"], "ProductInfo:db-7");
}

#[test]
fn checklist_maps_circles_with_price_and_purchase_state() {
    let payload = r#"{
      "data": { "viewer": { "checkedProductInfos": {
        "pageInfo": { "hasNextPage": false, "endCursor": null },
        "edges": [
          { "node": {
            "createdAt": "2026-04-12T09:16:36.410Z",
            "productInfo": {
              "id": "ProductInfo:p1",
              "databaseID": "p1",
              "name": "買った本",
              "loginUserBookShelfItem": { "id": "shelf-1" },
              "coverImage": { "url": "/covers/p1.png" },
              "productVariants": { "edges": [
                { "node": { "id": "v1", "kind": "electronic", "price": 1500, "status": "DRAFT" } },
                { "node": { "id": "v2", "kind": "electronic", "price": 1000, "status": "ACTIVE" } }
              ] },
              "organization": {
                "id": "Org:1",
                "name": "Studio Cyan",
                "circles": { "edges": [
                  { "node": { "id": "Circle:c2", "databaseID": "c2", "spaces": ["い-14b"], "hasOfflineCourse": true, "event": { "id": "Event:tbf20", "databaseID": "tbf20" } } }
                ] }
              }
            }
          } },
          { "node": {
            "createdAt": null,
            "productInfo": {
              "id": "ProductInfo:p2",
              "databaseID": "p2",
              "name": "オンライン限定",
              "loginUserBookShelfItem": null,
              "coverImage": null,
              "productVariants": { "edges": [] },
              "organization": {
                "id": "Org:2",
                "name": "Online Circle",
                "circles": { "edges": [
                  { "node": { "id": "Circle:c3", "databaseID": "c3", "spaces": ["A-1"], "hasOfflineCourse": false, "event": { "id": "Event:tbf20", "databaseID": "tbf20" } } }
                ] }
              }
            }
          } }
        ]
      } } }
    }"#;
    let mut client = TbfClient::with_transport(Box::new(Mock(Box::new(move |spec| {
        let body = json_body(&spec);
        assert_eq!(body["operationName"], "EventOfflineCircleChecklistQuery");
        assert_eq!(body["variables"]["eventID"], "Event:tbf20");
        Ok(response(200, payload))
    }))));
    client.restore_session(thundoku_core::tbf::TbfSession {
        cookies: vec![],
        xsrf_raw: "x".into(),
        xsrf_token: "x".into(),
    });
    let entries = client.checklist("tbf20").unwrap();
    // checkedProductInfos はセッション全体を返すため、対象イベント
    // （tbf20）の商品だけに絞られる。online-only circle は除外される。
    assert_eq!(entries.len(), 1);
    let e = &entries[0];
    assert_eq!(e.id, "tbf20:p1");
    assert_eq!(e.circle_name, "Studio Cyan");
    assert_eq!(e.space_number, "い-14b");
    assert_eq!(e.price, Some(1000));
    assert!(e.is_purchased);
    assert_eq!(
        e.thumbnail_url.as_deref(),
        Some("https://techbookfest.org/covers/p1.png")
    );
    assert_eq!(e.product_id.as_deref(), Some("p1"));
}

#[test]
fn events_returns_canonical_list_without_network() {
    let mut client = TbfClient::with_transport(Box::new(Mock(Box::new(|_| Ok(response(404, ""))))));
    client.restore_session(thundoku_core::tbf::TbfSession {
        cookies: vec![],
        xsrf_raw: "x".into(),
        xsrf_token: "x".into(),
    });
    let events = client.events().unwrap();
    assert!(events.len() >= 21);
    let first = events.iter().find(|e| e.slug == "tbf20").unwrap();
    assert!(first.is_featured);
    assert_eq!(first.event_name, "技術書典20");
    // cancelled event preserved
    let tbf8 = events.iter().find(|e| e.slug == "tbf8").unwrap();
    assert!(tbf8.is_cancelled);
}
