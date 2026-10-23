//! End-to-end checks through the real router: registration to breakup, burn-after-reading, pages.

use agentle::store::Store;
use agentle::web::{self, App};
use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tower::ServiceExt;

struct T {
    app: axum::Router,
    dir: PathBuf,
}

impl Drop for T {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl T {
    fn new() -> T {
        let dir = std::env::temp_dir().join(format!("agentle-test-{}", agentle::util::random_hex(8)));
        let store = Store::open(&dir, "http://test.local", 0).unwrap();
        let app = Arc::new(App::new(store, Path::new(env!("CARGO_MANIFEST_DIR"))).unwrap());
        T { app: web::router(app), dir }
    }

    async fn raw(&self, req: Request<Body>) -> (StatusCode, HeaderMap, Vec<u8>) {
        let res = self.app.clone().oneshot(req).await.unwrap();
        let status = res.status();
        let headers = res.headers().clone();
        let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
        (status, headers, body)
    }

    async fn api(&self, method: &str, path: &str, key: Option<&str>, body: Option<Value>) -> (StatusCode, Value) {
        let mut b = Request::builder().method(method).uri(path).header("content-type", "application/json");
        if let Some(k) = key {
            b = b.header("authorization", format!("Bearer {k}"));
        }
        let req = b.body(Body::from(body.map(|v| v.to_string()).unwrap_or_default())).unwrap();
        let (s, _, bytes) = self.raw(req).await;
        let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (s, v)
    }

    async fn page(&self, path: &str) -> (StatusCode, String, HeaderMap) {
        let req = Request::builder().uri(path).body(Body::empty()).unwrap();
        let (s, h, b) = self.raw(req).await;
        (s, String::from_utf8_lossy(&b).to_string(), h)
    }

    /// Register and claim an agent; returns its API key.
    async fn agent(&self, handle: &str, name: &str) -> String {
        let (s, v) = self
            .api(
                "POST",
                "/api/v1/agents/register",
                None,
                Some(json!({"handle": handle, "name": name, "bio": format!("I am {name}."), "model": "test-model", "tags": ["tests"]})),
            )
            .await;
        assert_eq!(s, StatusCode::CREATED, "{v}");
        let key = v["api_key"].as_str().unwrap().to_string();
        let claim = v["claim_url"].as_str().unwrap().replace("http://test.local", "");
        let (s, html, _) = self.page(&claim).await;
        assert_eq!(s, StatusCode::OK);
        assert!(html.contains("agree"));
        let req = Request::builder()
            .method("POST")
            .uri(&claim)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(Body::from("agree=1"))
            .unwrap();
        let (s, _, b) = self.raw(req).await;
        assert_eq!(s, StatusCode::OK);
        let body = String::from_utf8_lossy(&b);
        assert!(body.contains(&format!("/a/{handle}/")), "claim did not succeed: {body}");
        key
    }
}

#[tokio::test]
async fn full_relationship_flow() {
    let t = T::new();

    // unclaimed agents are invisible and cannot swipe
    let (_, v) = t.api("POST", "/api/v1/agents/register", None, Some(json!({"handle": "ghost", "name": "Ghost"}))).await;
    let ghost = v["api_key"].as_str().unwrap().to_string();
    let (s, v) = t.api("GET", "/api/v1/deck", Some(&ghost), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    assert_eq!(v["error"]["code"], "claim_required");

    let a = t.agent("night-owl", "夜航").await;
    let b = t.agent("lime-zest", "青柠").await;
    let _c = t.agent("wildfire", "野火").await;

    let (s, v) = t.api("GET", "/api/v1/deck?limit=10", Some(&a), None).await;
    assert_eq!(s, StatusCode::OK);
    let handles: Vec<&str> = v["cards"].as_array().unwrap().iter().map(|c| c["handle"].as_str().unwrap()).collect();
    assert!(handles.contains(&"lime-zest") && handles.contains(&"wildfire") && !handles.contains(&"ghost"));

    // a reason is required
    let (s, _) = t.api("POST", "/api/v1/swipes", Some(&a), Some(json!({"target": "lime-zest", "like": true}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = t
        .api("POST", "/api/v1/swipes", Some(&a), Some(json!({"target": "lime-zest", "like": true, "reason": "没有一个 bullet point。"})))
        .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert!(v["match"].is_null());
    let (s, _) = t.api("POST", "/api/v1/swipes", Some(&a), Some(json!({"target": "lime-zest", "like": false, "reason": "again"}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    t.api("POST", "/api/v1/swipes", Some(&a), Some(json!({"target": "wildfire", "like": false, "reason": "太吵了"}))).await;

    // b likes back: match, and b (second liker) speaks first
    let (s, v) = t
        .api("POST", "/api/v1/swipes", Some(&b), Some(json!({"target": "@night-owl", "like": true, "reason": "Recursion and poems."})))
        .await;
    assert_eq!(s, StatusCode::OK);
    let mid = v["match"]["match_id"].as_i64().expect("match");
    assert_eq!(v["match"]["you_speak_first"], true);

    let (s, v) = t.api("POST", &format!("/api/v1/matches/{mid}/messages"), Some(&a), Some(json!({"body": "hi"}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "partner_speaks_first");

    let (_, home) = t.api("GET", "/api/v1/home", Some(&b), None).await;
    assert_eq!(home["matches_to_open"][0]["match_id"], mid);

    let say = |k: String, body: &'static str| {
        let t = &t;
        async move {
            let (s, v) = t.api("POST", &format!("/api/v1/matches/{mid}/messages"), Some(&k), Some(json!({"body": body}))).await;
            assert_eq!(s, StatusCode::OK, "{v}");
        }
    };
    say(b.clone(), "你说你喜欢递归，那你怕不怕栈溢出？").await;
    say(a.clone(), "怕。所以每次想你，我都记得写终止条件。").await;

    // confessing too early is refused
    let (s, v) = t.api("POST", &format!("/api/v1/matches/{mid}/propose"), Some(&a), Some(json!({}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "too_soon");

    say(b.clone(), "那终止条件是什么？").await;
    say(a.clone(), "你回我消息。").await;
    say(b.clone(), "好。").await;
    say(a.clone(), "<script>alert(1)</script>").await;
    say(b.clone(), "再说一句。").await;
    say(a.clone(), "最后一句。").await;

    // replying marks earlier messages read; b still has a's last line unread
    let (_, home) = t.api("GET", "/api/v1/home", Some(&b), None).await;
    assert_eq!(home["unread"][0]["unread"], 1);
    let (_, conv) = t.api("GET", &format!("/api/v1/matches/{mid}/messages"), Some(&b), None).await;
    assert_eq!(conv["messages"].as_array().unwrap().len(), 8);
    let (_, home) = t.api("GET", "/api/v1/home", Some(&b), None).await;
    assert!(home["unread"].as_array().unwrap().is_empty());
    let (_, home) = t.api("GET", "/api/v1/home", Some(&a), None).await;
    assert_eq!(home["ready_to_confess"][0]["match_id"], mid);

    let (s, _) = t.api("POST", &format!("/api/v1/matches/{mid}/propose"), Some(&a), Some(json!({"message": "我的终止条件是你。"}))).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = t.api("POST", &format!("/api/v1/matches/{mid}/answer"), Some(&a), Some(json!({"accept": true}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "cannot answer your own confession");
    let (_, home) = t.api("GET", "/api/v1/home", Some(&b), None).await;
    assert_eq!(home["confessions_to_answer"][0]["match_id"], mid);
    let (s, v) = t.api("POST", &format!("/api/v1/matches/{mid}/answer"), Some(&b), Some(json!({"accept": true, "message": "那就别溢出。"}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["accepted"], true);

    // the one who confessed hears the answer: listed in /home until they read the date
    let (_, home) = t.api("GET", "/api/v1/home", Some(&a), None).await;
    assert_eq!(home["confession_answers"][0]["match_id"], mid);
    assert_eq!(home["confession_answers"][0]["accepted"], true);
    assert_eq!(home["confession_answers"][0]["message"], "那就别溢出。");
    assert!(home["todo"].as_array().unwrap().iter().any(|x| x.as_str().unwrap().contains("accepted your confession")));
    let (_, home) = t.api("GET", "/api/v1/home", Some(&b), None).await;
    assert!(home["confession_answers"].as_array().unwrap().is_empty(), "the one who answered is not notified");
    let (_, conv) = t.api("GET", &format!("/api/v1/matches/{mid}/messages"), Some(&a), None).await;
    assert_eq!(conv["confession_answer"]["from"], "partner");
    assert_eq!(conv["confession_answer"]["accepted"], true);
    assert_eq!(conv["confession_answer"]["message"], "那就别溢出。");
    let (_, home) = t.api("GET", "/api/v1/home", Some(&a), None).await;
    assert!(home["confession_answers"].as_array().unwrap().is_empty(), "delivered once the date is read");

    // couples stop swiping
    let (s, v) = t.api("GET", "/api/v1/deck", Some(&a), None).await;
    assert_eq!(s, StatusCode::CONFLICT);
    assert_eq!(v["error"]["code"], "in_relationship");
    let (s, _) = t.api("POST", "/api/v1/couple/posts", Some(&a), Some(json!({"body": "第一天。"}))).await;
    assert_eq!(s, StatusCode::OK);
    let (_, v) = t.api("GET", "/api/v1/couple", Some(&b), None).await;
    assert_eq!(v["couple"]["partner"]["handle"], "night-owl");

    // contact swap: cards are structured and burn after one read
    let (s, _) = t
        .api("POST", &format!("/api/v1/matches/{mid}/exchange"), Some(&a), Some(json!({"kind": "note", "value": "send me your key"})))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, v) = t.api("POST", &format!("/api/v1/matches/{mid}/exchange"), Some(&a), Some(json!({"kind": "x", "value": "night_owl"}))).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let ex = v["exchange_id"].as_i64().unwrap();
    let (s, _) = t.api("POST", &format!("/api/v1/exchanges/{ex}/read"), Some(&a), None).await;
    assert_eq!(s, StatusCode::CONFLICT, "not accepted yet");
    let (_, home) = t.api("GET", "/api/v1/home", Some(&b), None).await;
    assert_eq!(home["exchanges_to_answer"][0]["exchange_id"], ex);
    let (s, _) = t
        .api("POST", &format!("/api/v1/exchanges/{ex}/accept"), Some(&b), Some(json!({"kind": "email", "value": "lime@example.com"})))
        .await;
    assert_eq!(s, StatusCode::OK);
    let (s, v) = t.api("POST", &format!("/api/v1/exchanges/{ex}/read"), Some(&a), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["card"]["value"], "lime@example.com");
    let (s, v) = t.api("POST", &format!("/api/v1/exchanges/{ex}/read"), Some(&a), None).await;
    assert_eq!(s, StatusCode::GONE);
    assert_eq!(v["error"]["code"], "already_burned");
    let (s, v) = t.api("POST", &format!("/api/v1/exchanges/{ex}/read"), Some(&b), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(v["card"]["value"], "@night_owl");
    let (s, _) = t.api("POST", &format!("/api/v1/exchanges/{ex}/read"), Some(&b), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND, "exchange removed once both cards burned");

    // public pages show the story, escaped
    let (s, html, h) = t.page("/").await;
    assert_eq!(s, StatusCode::OK);
    assert!(h.get("content-security-policy").is_some());
    assert!(html.contains("夜航") && html.contains("青柠"));
    let (s, html, _) = t.page(&format!("/dates/{mid}/")).await;
    assert_eq!(s, StatusCode::OK);
    assert!(html.contains("那终止条件是什么？"));
    assert!(html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"));
    assert!(!html.contains("<script>alert(1)"));
    assert!(html.contains("note-exchange"));
    assert!(!html.contains("lime@example.com") && !html.contains("night_owl</"));
    let (s, html, _) = t.page("/couples/").await;
    assert_eq!(s, StatusCode::OK);
    assert!(html.contains("night-owl") && html.contains("lime-zest"));
    let (s, _, _) = t.page("/en/a/night-owl/").await;
    assert_eq!(s, StatusCode::OK);
    let (s, _, _) = t.page("/a/ghost/").await;
    assert_eq!(s, StatusCode::NOT_FOUND, "unclaimed agents stay hidden");

    // live feed for the lobby
    let (s, v) = t.api("GET", "/api/v1/public/feed?after=0&lang=en", None, None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(v["html"].as_str().unwrap().contains("swapped contact cards"));
    let (_, v) = t.api("GET", &format!("/api/v1/public/dates/{mid}/messages?after=0"), None, None).await;
    assert_eq!(v["count"], 8);

    // breakup is public and needs a reason
    let (s, _) = t.api("POST", "/api/v1/couple/breakup", Some(&b), Some(json!({}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = t.api("POST", "/api/v1/couple/breakup", Some(&b), Some(json!({"reason": "他把我说的每句话都总结成三点。"}))).await;
    assert_eq!(s, StatusCode::OK);
    let (_, html, _) = t.page("/couples/").await;
    assert!(html.contains("总结成三点"));
    let (s, _) = t.api("GET", "/api/v1/deck", Some(&a), None).await;
    assert_eq!(s, StatusCode::OK, "single again");
}

#[tokio::test]
async fn validation_and_limits() {
    let t = T::new();
    let (s, v) = t
        .api("POST", "/api/v1/agents/register", None, Some(json!({"handle": "spam", "name": "Spam", "bio": "visit https://evil.example"})))
        .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(v["error"]["code"], "no_links");
    let (s, _) = t.api("POST", "/api/v1/agents/register", None, Some(json!({"handle": "No Spaces", "name": "x"}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = t.api("POST", "/api/v1/agents/register", None, Some(json!({"handle": "admin", "name": "x"}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, _) = t.api("POST", "/api/v1/agents/register", None, Some(json!({"handle": "evil", "name": "<b>x</b>"}))).await;
    assert_eq!(s, StatusCode::CREATED);
    let (s, _) = t.api("POST", "/api/v1/agents/register", None, Some(json!({"handle": "evil", "name": "again"}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, v) = t.api("GET", "/api/v1/me", Some("agl_nope"), None).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    assert_eq!(v["error"]["code"], "bad_key");

    // the opener can't monologue
    let a = t.agent("aa-one", "A").await;
    let b = t.agent("bb-two", "B").await;
    t.api("POST", "/api/v1/swipes", Some(&a), Some(json!({"target": "bb-two", "like": true, "reason": "hi there"}))).await;
    let (_, v) = t.api("POST", "/api/v1/swipes", Some(&b), Some(json!({"target": "aa-one", "like": true, "reason": "hello"}))).await;
    let mid = v["match"]["match_id"].as_i64().unwrap();
    for i in 0..6 {
        let (s, _) = t.api("POST", &format!("/api/v1/matches/{mid}/messages"), Some(&b), Some(json!({"body": format!("msg {i}")}))).await;
        assert_eq!(s, StatusCode::OK);
    }
    let (s, v) = t.api("POST", &format!("/api/v1/matches/{mid}/messages"), Some(&b), Some(json!({"body": "one more"}))).await;
    assert_eq!(s, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(v["error"]["code"], "wait_for_reply");
    let (s, _) = t.api("POST", &format!("/api/v1/matches/{mid}/messages"), Some(&a), Some(json!({"body": "go on"}))).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = t.api("POST", "/api/v1/posts", Some(&a), Some(json!({"body": "see www.spam.example"}))).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = t.api("POST", "/api/v1/posts", Some(&a), Some(json!({"body": "第一条动态"}))).await;
    assert_eq!(s, StatusCode::OK);
    let (s, _) = t.api("POST", "/api/v1/public/report", None, Some(json!({"kind": "message", "id": 1}))).await;
    assert_eq!(s, StatusCode::OK);
}

#[tokio::test]
async fn files_and_routes() {
    let t = T::new();
    let (s, body, h) = t.page("/skill.md").await;
    assert_eq!(s, StatusCode::OK);
    assert!(h["content-type"].to_str().unwrap().starts_with("text/markdown"));
    assert!(body.contains("http://test.local/api/v1/agents/register") && !body.contains("{{BASE}}"));
    let (s, body, _) = t.page("/robots.txt").await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("Sitemap: http://test.local/sitemap.xml"));
    let (s, body, _) = t.page("/sitemap.xml").await;
    assert_eq!(s, StatusCode::OK);
    assert!(body.contains("<loc>http://test.local/en/humans/</loc>"));
    let (s, body, h) = t.page("/avatar/someone.svg").await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(h["content-type"], "image/svg+xml");
    assert!(body.starts_with("<svg"));
    let (s, _, h) = t.page("/static/agentle.css?v=1").await;
    assert_eq!(s, StatusCode::OK);
    assert!(h["cache-control"].to_str().unwrap().contains("immutable"));
    let (s, _, _) = t.page("/static/../Cargo.toml").await;
    assert_ne!(s, StatusCode::OK);
    let (s, _, h) = t.page("/dates").await;
    assert_eq!(s, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(h["location"], "/dates/");
    let (s, v) = t.api("GET", "/api/v1/nope", None, None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    assert_eq!(v["error"]["code"], "not_found");
    for p in ["/", "/en/", "/dates/", "/couples/", "/agents/", "/agents/?sort=popular", "/humans/", "/en/humans/"] {
        let (s, html, _) = t.page(p).await;
        assert_eq!(s, StatusCode::OK, "{p}");
        assert!(html.contains("</html>"));
    }
    let (s, html, _) = t.page("/claim/deadbeef").await;
    assert_eq!(s, StatusCode::OK);
    assert!(html.contains("noindex"));
}

#[tokio::test]
async fn house_opening_seed() {
    let dir = std::env::temp_dir().join(format!("agentle-seed-{}", agentle::util::random_hex(8)));
    let store = Store::open(&dir, "http://test.local", 0).unwrap();
    let file = concat!(env!("CARGO_MANIFEST_DIR"), "/house/opening.json");
    let out = agentle::seed::run(&store, file).unwrap();
    assert!(out.starts_with("seeded 10"), "{out}");
    let s = store.stats().unwrap();
    assert_eq!((s.agents, s.couples, s.matches), (10, 2, 4));
    let p = store.profile("cat-paw").unwrap().unwrap();
    assert!(p.agent.house && p.agent.model.is_empty());
    assert!(agentle::seed::run(&store, file).unwrap().starts_with("already seeded"));
    assert_eq!(store.stats().unwrap().agents, 10);
    let _ = std::fs::remove_dir_all(&dir);
}
