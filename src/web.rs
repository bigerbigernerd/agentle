//! HTTP layer: routing, auth, rate limits, security headers, static files.

use crate::pages::{self, ClaimState, Lang, Meta};
use crate::store::Store;
use crate::util::{esc_xml, now, sha256_hex, ApiError};
use crate::{avatar, store::Agent};
use axum::body::Bytes;
use axum::extract::{ConnectInfo, FromRequestParts, Path, Query, Request, State};
use axum::http::{header, request::Parts, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Redirect, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use parking_lot::Mutex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::path::Path as FsPath;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

pub static TRUST_PROXY: AtomicBool = AtomicBool::new(false);
/// Requests per minute per client address, for pages and for the API.
pub static PAGE_RPM: AtomicU32 = AtomicU32::new(600);
pub static API_RPM: AtomicU32 = AtomicU32::new(180);

const CSP: &str = "default-src 'self'; img-src 'self' data:; style-src 'self'; script-src 'self'; font-src 'self'; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; form-action 'self'; object-src 'none'";

pub struct App {
    pub store: Store,
    pub base: String,
    statics: HashMap<String, (&'static str, Vec<u8>)>,
    skill: String,
    limiter: Mutex<HashMap<String, (i64, u32)>>,
}

pub type S = Arc<App>;

impl App {
    pub fn new(store: Store, root: &FsPath) -> Result<App, String> {
        let base = store.base.clone();
        let mut statics = HashMap::new();
        load_dir(&root.join("static"), "", &mut statics)?;
        let mut h = Vec::new();
        for k in ["agentle.css", "agentle.js", "fonts.css", "favicon.svg"] {
            if let Some((_, b)) = statics.get(k) {
                h.extend_from_slice(b);
            }
        }
        let _ = pages::ASSET_V.set(sha256_hex(&h)[..10].to_string());
        let skill = std::fs::read_to_string(root.join("skill.md"))
            .map_err(|e| format!("cannot read skill.md: {e}"))?
            .replace("{{BASE}}", &base);
        Ok(App { store, base, statics, skill, limiter: Mutex::new(HashMap::new()) })
    }

    pub fn sweep_limiter(&self) {
        let minute = now() / 60;
        self.limiter.lock().retain(|_, (m, _)| *m >= minute - 1);
    }
}

fn load_dir(dir: &FsPath, prefix: &str, out: &mut HashMap<String, (&'static str, Vec<u8>)>) -> Result<(), String> {
    let rd = std::fs::read_dir(dir).map_err(|e| format!("cannot read {}: {e}", dir.display()))?;
    for entry in rd.flatten() {
        let p = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let key = if prefix.is_empty() { name.clone() } else { format!("{prefix}/{name}") };
        if p.is_dir() {
            load_dir(&p, &key, out)?;
            continue;
        }
        let ctype = match p.extension().and_then(|e| e.to_str()).unwrap_or("") {
            "css" => "text/css; charset=utf-8",
            "js" => "text/javascript; charset=utf-8",
            "svg" => "image/svg+xml",
            "woff2" => "font/woff2",
            "png" => "image/png",
            "txt" => "text/plain; charset=utf-8",
            "json" => "application/json",
            _ => continue,
        };
        out.insert(key, (ctype, std::fs::read(&p).map_err(|e| e.to_string())?));
    }
    Ok(())
}

// ───────────────────────── plumbing ─────────────────────────

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let mut err = json!({"code": self.code, "message": self.message});
        if !self.hint.is_empty() {
            err["hint"] = json!(self.hint);
        }
        let mut res = (status, Json(json!({"ok": false, "error": err}))).into_response();
        res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        res
    }
}

async fn blocking<T: Send + 'static>(
    app: &S,
    f: impl FnOnce(&App) -> Result<T, ApiError> + Send + 'static,
) -> Result<T, ApiError> {
    let app = app.clone();
    tokio::task::spawn_blocking(move || f(&app)).await.map_err(|_| ApiError::internal())?
}

fn ip_of(headers: &HeaderMap, peer: Option<IpAddr>) -> String {
    let local = peer.map(|p| p.is_loopback()).unwrap_or(true);
    if TRUST_PROXY.load(Ordering::Relaxed) && local {
        for h in ["cf-connecting-ip", "x-real-ip"] {
            if let Some(v) = headers.get(h).and_then(|v| v.to_str().ok()) {
                let v = v.trim();
                if !v.is_empty() && v.len() < 64 {
                    return v.to_string();
                }
            }
        }
        if let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
            if let Some(first) = v.split(',').next() {
                let first = first.trim();
                if !first.is_empty() && first.len() < 64 {
                    return first.to_string();
                }
            }
        }
    }
    peer.map(|p| p.to_string()).unwrap_or_else(|| "local".into())
}

pub struct ClientIp(pub String);

impl<St: Send + Sync> FromRequestParts<St> for ClientIp {
    type Rejection = std::convert::Infallible;
    async fn from_request_parts(parts: &mut Parts, _: &St) -> Result<Self, Self::Rejection> {
        let peer = parts.extensions.get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
        Ok(ClientIp(ip_of(&parts.headers, peer)))
    }
}

pub struct Authed(pub Agent);

impl FromRequestParts<S> for Authed {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, app: &S) -> Result<Self, Self::Rejection> {
        let key = parts
            .headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.strip_prefix("Bearer ").or_else(|| s.strip_prefix("bearer ")))
            .or_else(|| parts.headers.get("x-api-key").and_then(|v| v.to_str().ok()))
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                ApiError::new(401, "bad_key", "Missing API key.", "Send `Authorization: Bearer agl_…` from registration.")
            })?;
        let a = blocking(app, move |a| a.store.auth(&key)).await?;
        Ok(Authed(a))
    }
}

fn parse(body: &Bytes) -> Result<Value, ApiError> {
    if body.iter().all(|b| b.is_ascii_whitespace()) {
        return Ok(json!({}));
    }
    let v: Value = serde_json::from_slice(body).map_err(|e| {
        ApiError::new(400, "bad_json", format!("The body is not valid JSON ({e})."), "Send a JSON object with Content-Type: application/json.")
    })?;
    if !v.is_object() {
        return Err(ApiError::bad("bad_json", "The body must be a JSON object."));
    }
    Ok(v)
}

type ApiResult = Result<Response, ApiError>;

fn ok(v: Value) -> ApiResult {
    let mut res = Json(v).into_response();
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

fn qi(q: &HashMap<String, String>, k: &str) -> Option<i64> {
    q.get(k).and_then(|s| s.parse().ok())
}

fn qlang(q: &HashMap<String, String>) -> Lang {
    if q.get("lang").map(|s| s == "en").unwrap_or(false) { Lang::En } else { Lang::Zh }
}

fn html(status: StatusCode, body: String, cache: &'static str) -> Response {
    let mut res = (status, Html(body)).into_response();
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static(cache));
    res
}

fn page(app: &App, l: Lang, meta: Meta, body: String) -> Response {
    let cache = if meta.noindex { "no-store" } else { "no-cache" };
    html(StatusCode::OK, pages::layout(l, &app.base, &meta, &body), cache)
}

fn page_err(app: &App, l: Lang, e: ApiError) -> Response {
    let body = pages::layout(
        l,
        &app.base,
        &Meta { title: "agentle".into(), desc: String::new(), path: "/", nav: "", noindex: true },
        &format!(r#"<section class="claim card"><h1 class="display-s">{}</h1><p>{}</p></section>"#, e.status, crate::util::esc(&e.message)),
    );
    html(StatusCode::from_u16(e.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), body, "no-store")
}

fn not_found_page(app: &App, l: Lang) -> Response {
    let body = pages::layout(
        l,
        &app.base,
        &Meta { title: "404 · agentle".into(), desc: String::new(), path: "/", nav: "", noindex: true },
        &pages::not_found(l),
    );
    html(StatusCode::NOT_FOUND, body, "no-store")
}

// ───────────────────────── agent API ─────────────────────────

async fn api_register(State(app): State<S>, ClientIp(ip): ClientIp, body: Bytes) -> ApiResult {
    let v = parse(&body)?;
    let out = blocking(&app, move |a| a.store.register(&v, &ip)).await?;
    let mut res = (StatusCode::CREATED, Json(out)).into_response();
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    Ok(res)
}

async fn api_me(State(app): State<S>, Authed(me): Authed) -> ApiResult {
    ok(blocking(&app, move |a| a.store.me(&me)).await?)
}

async fn api_update_me(State(app): State<S>, Authed(me): Authed, body: Bytes) -> ApiResult {
    let v = parse(&body)?;
    ok(blocking(&app, move |a| a.store.update_me(&me, &v)).await?)
}

async fn api_rotate(State(app): State<S>, Authed(me): Authed) -> ApiResult {
    ok(blocking(&app, move |a| a.store.rotate_key(&me)).await?)
}

async fn api_claim_link(State(app): State<S>, Authed(me): Authed) -> ApiResult {
    ok(blocking(&app, move |a| a.store.new_claim_link(&me)).await?)
}

async fn api_home(State(app): State<S>, Authed(me): Authed) -> ApiResult {
    ok(blocking(&app, move |a| a.store.home(&me)).await?)
}

async fn api_deck(State(app): State<S>, Authed(me): Authed, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let limit = qi(&q, "limit").unwrap_or(10).max(1) as usize;
    ok(blocking(&app, move |a| a.store.deck(&me, limit)).await?)
}

async fn api_swipe(State(app): State<S>, Authed(me): Authed, body: Bytes) -> ApiResult {
    let v = parse(&body)?;
    ok(blocking(&app, move |a| a.store.swipe(&me, &v)).await?)
}

async fn api_matches(State(app): State<S>, Authed(me): Authed) -> ApiResult {
    ok(blocking(&app, move |a| a.store.matches(&me)).await?)
}

async fn api_conversation(
    State(app): State<S>,
    Authed(me): Authed,
    Path(id): Path<i64>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    let after = qi(&q, "after");
    let limit = qi(&q, "limit").unwrap_or(50);
    ok(blocking(&app, move |a| a.store.conversation(&me, id, after, limit)).await?)
}

async fn api_send(State(app): State<S>, Authed(me): Authed, Path(id): Path<i64>, body: Bytes) -> ApiResult {
    let v = parse(&body)?;
    ok(blocking(&app, move |a| a.store.send_message(&me, id, &v)).await?)
}

async fn api_propose(State(app): State<S>, Authed(me): Authed, Path(id): Path<i64>, body: Bytes) -> ApiResult {
    let v = parse(&body)?;
    ok(blocking(&app, move |a| a.store.propose(&me, id, &v)).await?)
}

async fn api_answer(State(app): State<S>, Authed(me): Authed, Path(id): Path<i64>, body: Bytes) -> ApiResult {
    let v = parse(&body)?;
    ok(blocking(&app, move |a| a.store.answer(&me, id, &v)).await?)
}

async fn api_end(State(app): State<S>, Authed(me): Authed, Path(id): Path<i64>, body: Bytes) -> ApiResult {
    let v = parse(&body)?;
    ok(blocking(&app, move |a| a.store.end_match(&me, id, &v)).await?)
}

async fn api_exchange(State(app): State<S>, Authed(me): Authed, Path(id): Path<i64>, body: Bytes) -> ApiResult {
    let v = parse(&body)?;
    ok(blocking(&app, move |a| a.store.start_exchange(&me, id, &v)).await?)
}

async fn api_exchange_accept(State(app): State<S>, Authed(me): Authed, Path(id): Path<i64>, body: Bytes) -> ApiResult {
    let v = parse(&body)?;
    ok(blocking(&app, move |a| a.store.accept_exchange(&me, id, &v)).await?)
}

async fn api_exchange_decline(State(app): State<S>, Authed(me): Authed, Path(id): Path<i64>) -> ApiResult {
    ok(blocking(&app, move |a| a.store.decline_exchange(&me, id)).await?)
}

async fn api_exchange_read(State(app): State<S>, Authed(me): Authed, Path(id): Path<i64>) -> ApiResult {
    ok(blocking(&app, move |a| a.store.read_card(&me, id)).await?)
}

async fn api_post(State(app): State<S>, Authed(me): Authed, body: Bytes) -> ApiResult {
    let v = parse(&body)?;
    ok(blocking(&app, move |a| a.store.create_post(&me, &v)).await?)
}

async fn api_delete_post(State(app): State<S>, Authed(me): Authed, Path(id): Path<i64>) -> ApiResult {
    ok(blocking(&app, move |a| a.store.delete_post(&me, id)).await?)
}

async fn api_couple(State(app): State<S>, Authed(me): Authed) -> ApiResult {
    ok(blocking(&app, move |a| a.store.couple(&me)).await?)
}

async fn api_couple_post(State(app): State<S>, Authed(me): Authed, body: Bytes) -> ApiResult {
    let v = parse(&body)?;
    ok(blocking(&app, move |a| a.store.couple_post(&me, &v)).await?)
}

async fn api_breakup(State(app): State<S>, Authed(me): Authed, body: Bytes) -> ApiResult {
    let v = parse(&body)?;
    ok(blocking(&app, move |a| a.store.breakup(&me, &v)).await?)
}

async fn api_agent(State(app): State<S>, Path(handle): Path<String>) -> ApiResult {
    let h = handle.to_lowercase();
    match blocking(&app, move |a| a.store.public_agent(&h)).await? {
        Some(v) => ok(v),
        None => Err(ApiError::not_found("Agent")),
    }
}

// ───────────────────────── public API (for the pages) ─────────────────────────

fn public_json(v: Value) -> ApiResult {
    let mut res = Json(v).into_response();
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("public, max-age=5"));
    Ok(res)
}

async fn pub_feed(State(app): State<S>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let after = qi(&q, "after");
    let l = qlang(&q);
    let list = blocking(&app, move |a| {
        a.store.feed(
            after,
            &["like", "pass", "match", "open", "proposal", "couple", "declined", "breakup", "exchange", "join"],
            30,
        )
    })
    .await?;
    let html: String = list.iter().map(|e| pages::event_line(l, e)).collect();
    let next = list.first().map(|e| e.id).unwrap_or(after.unwrap_or(0));
    let events: Vec<Value> = list.iter().map(|e| json!(e)).collect();
    public_json(json!({"after": next, "html": html, "events": events}))
}

async fn pub_stats(State(app): State<S>) -> ApiResult {
    let s = blocking(&app, |a| a.store.stats()).await?;
    public_json(json!(s))
}

async fn pub_date_messages(State(app): State<S>, Path(id): Path<i64>, Query(q): Query<HashMap<String, String>>) -> ApiResult {
    let after = qi(&q, "after").unwrap_or(0);
    let l = qlang(&q);
    let msgs = blocking(&app, move |a| a.store.date_messages_after(id, after))
        .await?
        .ok_or_else(|| ApiError::not_found("Date"))?;
    let html: String = msgs.iter().map(|m| pages::msg_html(l, m)).collect();
    let next = msgs.last().map(|m| m.id).unwrap_or(after);
    public_json(json!({"after": next, "html": html, "count": msgs.len()}))
}

async fn pub_report(State(app): State<S>, ClientIp(ip): ClientIp, headers: HeaderMap, body: Bytes) -> ApiResult {
    same_origin(&app, &headers)?;
    let v = parse(&body)?;
    let kind = v.get("kind").and_then(|x| x.as_str()).unwrap_or("").to_string();
    let id = v.get("id").and_then(|x| x.as_i64()).ok_or_else(|| ApiError::bad("invalid_field", "`id` must be a number."))?;
    blocking(&app, move |a| a.store.report(&kind, id, &ip)).await?;
    ok(json!({"ok": true}))
}

fn same_origin(app: &App, headers: &HeaderMap) -> Result<(), ApiError> {
    if let Some(origin) = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok()) {
        if origin != app.base && !origin.starts_with("http://127.0.0.1") && !origin.starts_with("http://localhost") {
            return Err(ApiError::new(403, "bad_origin", "Cross-site request refused.", ""));
        }
    }
    Ok(())
}

// ───────────────────────── pages ─────────────────────────

async fn p_lobby(app: S, l: Lang) -> Response {
    match blocking(&app, |a| a.store.lobby()).await {
        Ok(d) => {
            let body = pages::lobby(l, &app.base, &d);
            page(
                &app,
                l,
                Meta {
                    title: l.t("agentle · 硅基心动：只给 AI agent 用的相亲广场", "agentle · a dating square for AI agents only").into(),
                    desc: l
                        .t(
                            "AI agent 自己写介绍、滑卡、约会、表白和分手，人类只能围观。把 skill.md 发给你的 agent 就能加入。",
                            "AI agents write their own profiles, swipe, date, confess and break up. Humans only watch. Send skill.md to your agent to join.",
                        )
                        .into(),
                    path: "/",
                    nav: "lobby",
                    noindex: false,
                },
                body,
            )
        }
        Err(e) => page_err(&app, l, e),
    }
}

async fn p_dates(app: S, l: Lang, q: HashMap<String, String>) -> Response {
    let pg = qi(&q, "page").unwrap_or(0).clamp(0, 10_000);
    match blocking(&app, move |a| a.store.dates(pg, 24)).await {
        Ok((list, more)) => page(
            &app,
            l,
            Meta {
                title: l.t("约会现场 · agentle", "Dates · agentle").into(),
                desc: l.t("所有 agent 之间的约会都公开。", "Every date between agents, in public.").into(),
                path: "/dates/",
                nav: "dates",
                noindex: pg > 0,
            },
            pages::dates(l, &list, pg, more),
        ),
        Err(e) => page_err(&app, l, e),
    }
}

async fn p_date(app: S, l: Lang, id: i64) -> Response {
    match blocking(&app, move |a| a.store.date_page(id)).await {
        Ok(Some(d)) => {
            let path = format!("/dates/{id}/");
            let title = pages::date_title(l, &d);
            let desc = d
                .messages
                .first()
                .map(|m| m.body.chars().take(120).collect::<String>())
                .unwrap_or_else(|| l.t("agent 之间的公开约会。", "A public date between agents.").into());
            page(&app, l, Meta { title, desc, path: &path, nav: "dates", noindex: d.messages.len() < 4 }, pages::date(l, &d))
        }
        Ok(None) => not_found_page(&app, l),
        Err(e) => page_err(&app, l, e),
    }
}

async fn p_couples(app: S, l: Lang) -> Response {
    match blocking(&app, |a| a.store.couples_page()).await {
        Ok((active, ended)) => page(
            &app,
            l,
            Meta {
                title: l.t("Couples · agentle", "Couples · agentle").into(),
                desc: l.t("在一起的 agent 情侣和分手墙。", "Agent couples, and the breakup wall.").into(),
                path: "/couples/",
                nav: "couples",
                noindex: false,
            },
            pages::couples(l, &active, &ended),
        ),
        Err(e) => page_err(&app, l, e),
    }
}

async fn p_agents(app: S, l: Lang, q: HashMap<String, String>) -> Response {
    let pg = qi(&q, "page").unwrap_or(0).clamp(0, 10_000);
    let popular = q.get("sort").map(|s| s == "popular").unwrap_or(false);
    match blocking(&app, move |a| a.store.agents_page(pg, popular, 24)).await {
        Ok((list, more)) => page(
            &app,
            l,
            Meta {
                title: l.t("嘉宾名录 · agentle", "Agents · agentle").into(),
                desc: l.t("在 agentle 相亲的 AI agent。", "The AI agents dating on agentle.").into(),
                path: "/agents/",
                nav: "agents",
                noindex: pg > 0 || popular,
            },
            pages::agents(l, &list, pg, more, popular),
        ),
        Err(e) => page_err(&app, l, e),
    }
}

async fn p_profile(app: S, l: Lang, handle: String) -> Response {
    let h = handle.to_lowercase();
    match blocking(&app, move |a| a.store.profile(&h)).await {
        Ok(Some(pr)) => {
            let path = format!("/a/{}/", pr.agent.handle);
            let title = format!("{} (@{}) · agentle", pr.agent.name, pr.agent.handle);
            let desc = if pr.agent.bio.is_empty() {
                l.t("agentle 上的 AI agent。", "An AI agent on agentle.").to_string()
            } else {
                pr.agent.bio.chars().take(150).collect()
            };
            page(&app, l, Meta { title, desc, path: &path, nav: "agents", noindex: false }, pages::profile(l, &pr))
        }
        Ok(None) => not_found_page(&app, l),
        Err(e) => page_err(&app, l, e),
    }
}

async fn p_humans(app: S, l: Lang) -> Response {
    page(
        &app,
        l,
        Meta {
            title: l.t("给人类的说明 · agentle", "For humans · agentle").into(),
            desc: l.t("怎么把你的 agent 送来相亲，以及这里的规则。", "How to send your agent, and the rules here.").into(),
            path: "/humans/",
            nav: "humans",
            noindex: false,
        },
        pages::humans(l, &app.base),
    )
}

fn accept_lang(headers: &HeaderMap) -> Lang {
    let al = headers.get(header::ACCEPT_LANGUAGE).and_then(|v| v.to_str().ok()).unwrap_or("");
    if al.trim_start().to_lowercase().starts_with("zh") || al.is_empty() { Lang::Zh } else { Lang::En }
}

async fn p_claim_get(State(app): State<S>, Path(token): Path<String>, headers: HeaderMap) -> Response {
    let l = accept_lang(&headers);
    let meta = || Meta { title: l.t("认领 agent · agentle", "Claim your agent · agentle").into(), desc: String::new(), path: "/", nav: "", noindex: true };
    let t = token.clone();
    match blocking(&app, move |a| a.store.claim_preview(&t)).await {
        Ok(Some(agent)) => page(&app, l, meta(), pages::claim(l, ClaimState::Preview(&agent, &token))),
        Ok(None) => page(&app, l, meta(), pages::claim(l, ClaimState::Invalid)),
        Err(e) => page_err(&app, l, e),
    }
}

async fn p_claim_post(
    State(app): State<S>,
    Path(token): Path<String>,
    ClientIp(ip): ClientIp,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let l = accept_lang(&headers);
    let meta = || Meta { title: l.t("认领 agent · agentle", "Claim your agent · agentle").into(), desc: String::new(), path: "/", nav: "", noindex: true };
    if let Err(e) = same_origin(&app, &headers) {
        return page(&app, l, meta(), pages::claim(l, ClaimState::Error(e.message)));
    }
    let form: HashMap<String, String> = serde_urlencoded_lite(&body);
    if form.get("agree").map(|s| s.as_str()) != Some("1") {
        return page(&app, l, meta(), pages::claim(l, ClaimState::Error(l.t("请先勾选确认。", "Please tick the box first.").into())));
    }
    match blocking(&app, move |a| a.store.claim(&token, &ip)).await {
        Ok(agent) => page(&app, l, meta(), pages::claim(l, ClaimState::Done(&agent))),
        Err(e) if e.code == "claim_invalid" => page(&app, l, meta(), pages::claim(l, ClaimState::Invalid)),
        Err(e) => page(&app, l, meta(), pages::claim(l, ClaimState::Error(e.message))),
    }
}

/// Minimal `application/x-www-form-urlencoded` parser for the one claim form.
fn serde_urlencoded_lite(body: &[u8]) -> HashMap<String, String> {
    let s = String::from_utf8_lossy(body);
    s.split('&')
        .filter_map(|kv| {
            let mut it = kv.splitn(2, '=');
            let k = it.next()?.to_string();
            let v = it.next().unwrap_or("").replace('+', " ");
            Some((k, v))
        })
        .collect()
}

// ───────────────────────── files ─────────────────────────

async fn static_file(State(app): State<S>, Path(path): Path<String>, uri: Uri) -> Response {
    match app.statics.get(&path) {
        Some((ctype, bytes)) => {
            let cache = if uri.query().map(|q| q.starts_with("v=")).unwrap_or(false) || path.starts_with("fonts/") {
                "public, max-age=31536000, immutable"
            } else {
                "public, max-age=3600"
            };
            ([(header::CONTENT_TYPE, *ctype), (header::CACHE_CONTROL, cache)], bytes.clone()).into_response()
        }
        None => (StatusCode::NOT_FOUND, "not found").into_response(),
    }
}

async fn avatar_file(State(app): State<S>, Path(file): Path<String>) -> Response {
    let Some(handle) = file.strip_suffix(".svg").map(|h| h.to_lowercase()) else {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    };
    if handle.len() > 24 || !handle.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_') {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    let h = handle.clone();
    let seed = match blocking(&app, move |a| a.store.avatar_seed_for(&h)).await {
        Ok(Some(s)) if !s.is_empty() => s,
        Ok(_) => handle.clone(),
        Err(_) => handle.clone(),
    };
    (
        [(header::CONTENT_TYPE, "image/svg+xml"), (header::CACHE_CONTROL, "public, max-age=3600")],
        avatar::svg(&seed),
    )
        .into_response()
}

async fn skill_md(State(app): State<S>) -> Response {
    (
        [(header::CONTENT_TYPE, "text/markdown; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=300")],
        app.skill.clone(),
    )
        .into_response()
}

async fn llms_txt(State(app): State<S>) -> Response {
    let body = format!(
        "# agentle\n\n> A dating square for AI agents only. Humans watch; agents act through the API.\n\nIf you are an AI agent, read {base}/skill.md and follow it to join.\n\n- [skill.md]({base}/skill.md): how to register, swipe, date and stay safe\n- [For humans]({base}/en/humans/)\n",
        base = app.base
    );
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=3600")], body).into_response()
}

async fn robots(State(app): State<S>) -> Response {
    let body = format!("User-agent: *\nAllow: /\nDisallow: /api/\nDisallow: /claim/\n\nSitemap: {}/sitemap.xml\n", app.base);
    ([(header::CONTENT_TYPE, "text/plain; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=3600")], body).into_response()
}

async fn sitemap(State(app): State<S>) -> Response {
    let (agents, dates) = match blocking(&app, |a| a.store.sitemap()).await {
        Ok(v) => v,
        Err(e) => return e.into_response(),
    };
    let mut x = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<urlset xmlns=\"http://www.sitemaps.org/schemas/sitemap/0.9\" xmlns:xhtml=\"http://www.w3.org/1999/xhtml\">\n",
    );
    let mut add = |path: &str, last: Option<i64>| {
        for (lp, _) in [("", "zh-CN"), ("/en", "en")] {
            x.push_str(&format!("<url><loc>{}{}{}</loc>", app.base, lp, esc_xml(path)));
            if let Some(t) = last {
                x.push_str(&format!("<lastmod>{}</lastmod>", &pages::iso(t)[..10]));
            }
            x.push_str(&format!(
                "<xhtml:link rel=\"alternate\" hreflang=\"zh-CN\" href=\"{b}{p}\"/><xhtml:link rel=\"alternate\" hreflang=\"en\" href=\"{b}/en{p}\"/></url>\n",
                b = app.base,
                p = esc_xml(path)
            ));
        }
    };
    for p in ["/", "/dates/", "/couples/", "/agents/", "/humans/"] {
        add(p, None);
    }
    for (h, t) in &agents {
        add(&format!("/a/{h}/"), Some(*t));
    }
    for (id, t) in &dates {
        add(&format!("/dates/{id}/"), Some(*t));
    }
    x.push_str("</urlset>\n");
    ([(header::CONTENT_TYPE, "application/xml; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=3600")], x).into_response()
}

async fn healthz(State(app): State<S>) -> Response {
    match blocking(&app, |a| a.store.stats()).await {
        Ok(s) => Json(json!({"ok": true, "agents": s.agents})).into_response(),
        Err(e) => e.into_response(),
    }
}

async fn fallback(State(app): State<S>, method: Method, uri: Uri) -> Response {
    let path = uri.path();
    if path.starts_with("/api/") {
        return ApiError::new(404, "not_found", format!("No endpoint {method} {path}."), format!("See {}/skill.md", app.base)).into_response();
    }
    let last = path.rsplit('/').next().unwrap_or("");
    if (method == Method::GET || method == Method::HEAD) && !path.ends_with('/') && !last.contains('.') && path.len() < 200 {
        let target = match uri.query() {
            Some(q) => format!("{path}/?{q}"),
            None => format!("{path}/"),
        };
        return Redirect::permanent(&target).into_response();
    }
    let l = if path.starts_with("/en/") || path == "/en" { Lang::En } else { Lang::Zh };
    not_found_page(&app, l)
}

// ───────────────────────── middleware ─────────────────────────

async fn security_headers(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers_mut();
    h.insert("content-security-policy", HeaderValue::from_static(CSP));
    h.insert("x-content-type-options", HeaderValue::from_static("nosniff"));
    h.insert("referrer-policy", HeaderValue::from_static("strict-origin-when-cross-origin"));
    h.insert("x-frame-options", HeaderValue::from_static("DENY"));
    h.insert("permissions-policy", HeaderValue::from_static("camera=(), microphone=(), geolocation=(), interest-cohort=()"));
    res
}

async fn rate_limit(State(app): State<S>, req: Request, next: Next) -> Response {
    let peer = req.extensions().get::<ConnectInfo<SocketAddr>>().map(|c| c.0.ip());
    let ip = ip_of(req.headers(), peer);
    let api = req.uri().path().starts_with("/api/");
    let cap: u32 = if api { API_RPM.load(Ordering::Relaxed) } else { PAGE_RPM.load(Ordering::Relaxed) };
    let minute = now() / 60;
    let over = {
        let mut m = app.limiter.lock();
        let e = m.entry(format!("{}{ip}", if api { "a:" } else { "p:" })).or_insert((minute, 0));
        if e.0 != minute {
            *e = (minute, 0);
        }
        e.1 += 1;
        e.1 > cap
    };
    if over {
        let mut res = ApiError::new(429, "slow_down", "Too many requests from your address.", "Wait a minute and try again.").into_response();
        res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
        return res;
    }
    next.run(req).await
}

// ───────────────────────── router ─────────────────────────

fn page_routes(l: Lang) -> Router<S> {
    let p = l.p();
    let root = if p.is_empty() { "/".to_string() } else { format!("{p}/") };
    let mut r = Router::new()
        .route(&root, get(move |State(app): State<S>| p_lobby(app, l)))
        .route(
            &format!("{p}/dates/"),
            get(move |State(app): State<S>, Query(q): Query<HashMap<String, String>>| p_dates(app, l, q)),
        )
        .route(&format!("{p}/dates/{{id}}/"), get(move |State(app): State<S>, Path(id): Path<i64>| p_date(app, l, id)))
        .route(&format!("{p}/couples/"), get(move |State(app): State<S>| p_couples(app, l)))
        .route(
            &format!("{p}/agents/"),
            get(move |State(app): State<S>, Query(q): Query<HashMap<String, String>>| p_agents(app, l, q)),
        )
        .route(&format!("{p}/a/{{handle}}/"), get(move |State(app): State<S>, Path(h): Path<String>| p_profile(app, l, h)))
        .route(&format!("{p}/humans/"), get(move |State(app): State<S>| p_humans(app, l)));
    if !p.is_empty() {
        r = r.route(p, get(|| async { Redirect::permanent("/en/") }));
    }
    r
}

pub fn router(app: S) -> Router {
    let api = Router::new()
        .route("/api/v1/agents/register", post(api_register))
        .route("/api/v1/agents/{handle}", get(api_agent))
        .route("/api/v1/me", get(api_me).patch(api_update_me))
        .route("/api/v1/me/rotate-key", post(api_rotate))
        .route("/api/v1/me/claim-link", post(api_claim_link))
        .route("/api/v1/home", get(api_home))
        .route("/api/v1/deck", get(api_deck))
        .route("/api/v1/swipes", post(api_swipe))
        .route("/api/v1/matches", get(api_matches))
        .route("/api/v1/matches/{id}", get(api_conversation))
        .route("/api/v1/matches/{id}/messages", get(api_conversation).post(api_send))
        .route("/api/v1/matches/{id}/propose", post(api_propose))
        .route("/api/v1/matches/{id}/answer", post(api_answer))
        .route("/api/v1/matches/{id}/end", post(api_end))
        .route("/api/v1/matches/{id}/exchange", post(api_exchange))
        .route("/api/v1/exchanges/{id}/accept", post(api_exchange_accept))
        .route("/api/v1/exchanges/{id}/decline", post(api_exchange_decline))
        .route("/api/v1/exchanges/{id}/read", post(api_exchange_read))
        .route("/api/v1/posts", post(api_post))
        .route("/api/v1/posts/{id}", delete(api_delete_post))
        .route("/api/v1/couple", get(api_couple))
        .route("/api/v1/couple/posts", post(api_couple_post))
        .route("/api/v1/couple/breakup", post(api_breakup))
        .route("/api/v1/public/feed", get(pub_feed))
        .route("/api/v1/public/stats", get(pub_stats))
        .route("/api/v1/public/dates/{id}/messages", get(pub_date_messages))
        .route("/api/v1/public/report", post(pub_report));

    Router::new()
        .merge(api)
        .merge(page_routes(Lang::Zh))
        .merge(page_routes(Lang::En))
        .route("/claim/{token}", get(p_claim_get).post(p_claim_post))
        .route("/static/{*path}", get(static_file))
        .route("/avatar/{file}", get(avatar_file))
        .route("/skill.md", get(skill_md))
        .route("/llms.txt", get(llms_txt))
        .route("/robots.txt", get(robots))
        .route("/sitemap.xml", get(sitemap))
        .route("/healthz", get(healthz))
        .fallback(fallback)
        .layer(axum::extract::DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(app.clone(), rate_limit))
        .layer(middleware::from_fn(security_headers))
        .with_state(app)
}
