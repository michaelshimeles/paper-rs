//! MCP server: tool behaviour on a headless session, then the real thing end to end — an rmcp
//! client over streamable HTTP, and through the `sketch-rs-mcp` stdio bridge.

use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult, ContentBlock};
use serde_json::{Value, json};
use sketch_rs::mcp::{self, Out, exec};
use sketch_rs::model::{Color, NodeKind, Paint, Sizing};
use sketch_rs::project::Store;

fn store() -> Store {
    Store::open(std::env::temp_dir().join(format!("sketch-rs-mcp-{:016x}", fastrand::u64(..))))
        .unwrap()
}

/// A session with a fresh project open.
fn host() -> exec::SessionHost {
    let s = store();
    let mut h = exec::SessionHost::new(s);
    h.session.create(&mut h.ed, "MCP test").unwrap();
    h
}

fn call(h: &mut exec::SessionHost, name: &str, args: Value) -> Value {
    match exec::call(h, name, &args) {
        Ok(outs) => match outs.into_iter().next() {
            Some(Out::Text(t)) => serde_json::from_str(&t).unwrap_or(Value::String(t)),
            Some(Out::Image { .. }) => json!("image"),
            None => Value::Null,
        },
        Err(e) => panic!("{name} failed: {e}"),
    }
}

fn num(id: &Value) -> sketch_rs::model::NodeId {
    sketch_rs::model::NodeId(
        id.as_str()
            .unwrap()
            .rsplit(':')
            .next()
            .unwrap()
            .parse()
            .unwrap(),
    )
}

#[test]
fn artboard_and_html_become_auto_layout_frames_and_text() {
    let mut h = host();
    let ab = call(
        &mut h,
        "create_artboard",
        json!({"name": "Phone", "styles": {"width": "390px", "height": "844px", "padding": "24px", "gap": 16, "backgroundColor": "#f5f5f5"}}),
    );
    let ab_id = ab["id"].clone();
    let r = call(
        &mut h,
        "write_html",
        json!({
            "targetNodeId": ab_id, "mode": "insert-children",
            "html": r#"<div layer-name="Card" style="display:flex; flex-direction:column; gap:8px; padding:16px; background:#ffffff; border-radius:12px; border:1px solid #e5e5e5">
                <h1 style="color:#111">Welcome back</h1>
                <p style="font-size:14px; color:rgb(80, 80, 80); line-height:20px">Sign in to <b>continue</b> where you left off.</p>
                <div style="display:flex; justify-content:center; align-items:center; height:44px; background:#2563eb; border-radius:8px; color:white; font-weight:600">Continue</div>
              </div>"#,
        }),
    );
    assert_eq!(r["createdNodeIds"].as_array().unwrap().len(), 1, "{r}");
    let card = num(&r["createdNodeIds"][0]);
    let doc = &h.ed.doc;
    doc.validate().unwrap();
    let c = doc.get(card).unwrap();
    assert_eq!(c.name, "Card");
    assert!(c.layout.is_some());
    assert_eq!(c.style.corner_radius, 12.0);
    // Stretched across the artboard's inner width (390 - 2 × 24).
    assert_eq!(c.layout_child.sizing[0], Sizing::Fill);
    assert!((c.rect.w - 342.0).abs() < 0.5, "{:?}", c.rect);
    assert_eq!((c.rect.x, c.rect.y), (24.0, 24.0));
    assert_eq!(c.children.len(), 3);

    let title = doc.get(c.children[0]).unwrap();
    let NodeKind::Text {
        text, font_size, ..
    } = &title.kind
    else {
        panic!("{:?}", title.kind)
    };
    assert_eq!((text.as_str(), *font_size), ("Welcome back", 32.0));
    assert!(title.text_style.bold);
    assert_eq!(title.style.fill(), Some(Color::rgb(0x11, 0x11, 0x11)));
    // Text fills the card's inner width and sits inside its padding.
    assert!(
        (title.rect.w - (342.0 - 32.0)).abs() < 0.5,
        "{:?}",
        title.rect
    );
    assert_eq!((title.rect.x, title.rect.y), (16.0, 16.0));

    let body = doc.get(c.children[1]).unwrap();
    let NodeKind::Text { text, .. } = &body.kind else {
        panic!()
    };
    assert_eq!(text, "Sign in to continue where you left off.");
    assert_eq!(body.text_style.line_height, Some(20.0 / 14.0));
    assert!(
        body.text_style
            .spans
            .iter()
            .any(|s| s.bold == Some(true) && s.end - s.start == 8)
    );
    assert!((body.rect.y - (title.rect.y + title.rect.h + 8.0)).abs() < 0.5);

    // The button is a frame (it has a background) holding a centered white bold label.
    let button = doc.get(c.children[2]).unwrap();
    assert!(matches!(button.kind, NodeKind::Frame { .. }));
    assert_eq!(button.rect.h, 44.0);
    assert_eq!(button.style.fill(), Some(Color::rgb(0x25, 0x63, 0xeb)));
    let label = doc.get(button.children[0]).unwrap();
    assert_eq!(label.kind.text().unwrap(), "Continue");
    assert!(label.text_style.bold);
    assert_eq!(label.style.fill(), Some(Color::WHITE));
    let centered = button.rect.w * 0.5 - (label.rect.x + label.rect.w * 0.5);
    assert!(
        centered.abs() < 1.0,
        "label not centered: {:?} in {:?}",
        label.rect,
        button.rect
    );

    // The card hugs its content: padding + three children + two gaps.
    let want = 16.0 + title.rect.h + 8.0 + body.rect.h + 8.0 + 44.0 + 16.0;
    assert!((c.rect.h - want).abs() < 0.5, "{} vs {want}", c.rect.h);

    // One undo step removes the whole write.
    let before = h.ed.doc.nodes.len();
    call(&mut h, "undo", json!({}));
    assert_eq!(h.ed.doc.nodes.len(), 1, "{before} nodes before undo");
}

#[test]
fn reading_tools_describe_the_page() {
    let mut h = host();
    let ab = call(
        &mut h,
        "create_artboard",
        json!({"name": "Desktop", "styles": {"width": 1440, "height": 900}}),
    );
    call(
        &mut h,
        "write_html",
        json!({"targetNodeId": ab["id"], "mode": "insert-children",
               "html": "<nav layer-name=\"Nav\" style=\"display:flex; gap:24px; padding:20px\"><span>Home</span><span>Pricing</span><span>Get started</span></nav>"}),
    );
    let info = call(&mut h, "get_basic_info", json!({}));
    assert_eq!(info["fileName"], "MCP test");
    assert_eq!(info["artboards"][0]["name"], "Desktop");
    assert_eq!(info["artboards"][0]["width"], 1440.0);
    assert_eq!(info["pages"].as_array().unwrap().len(), 1);

    let tree = call(
        &mut h,
        "get_tree_summary",
        json!({"nodeId": "root", "depth": 5}),
    );
    let tree = tree.as_str().unwrap();
    assert!(
        tree.contains("Frame \"Desktop\"") && tree.contains("\"Get started\""),
        "{tree}"
    );

    let found = call(&mut h, "find_nodes", json!({"textValue": "get *"}));
    assert_eq!(found["count"], 1, "{found}");
    let span = found["results"][0]["id"].clone();
    let n = call(&mut h, "get_node_info", json!({"nodeId": span}));
    assert_eq!(n["type"], "Text");
    assert_eq!(n["text"], "Get started");

    let styles = call(
        &mut h,
        "get_computed_styles",
        json!({"nodeIds": [ab["id"]]}),
    );
    let s = &styles[ab["id"].as_str().unwrap()];
    assert_eq!(s["width"], "1440px");
    assert_eq!(s["flexDirection"], "column");
    assert_eq!(s["backgroundColor"], "#ffffff");

    let jsx = call(&mut h, "get_jsx", json!({"nodeId": ab["id"]}));
    let jsx = jsx.as_str().unwrap();
    assert!(
        jsx.contains("<div data-name=\"Nav\"") && jsx.contains(">Pricing</span>"),
        "{jsx}"
    );

    let by_style = call(
        &mut h,
        "find_nodes",
        json!({"filters": [{"styleName": "gap", "styleValue": "24px"}]}),
    );
    assert_eq!(by_style["count"], 1, "{by_style}");
}

#[test]
fn editing_tools_update_move_duplicate_and_delete() {
    let mut h = host();
    let ab = call(
        &mut h,
        "create_artboard",
        json!({"name": "A", "styles": {"width": "400px", "height": "fit-content"}}),
    );
    let r = call(
        &mut h,
        "write_html",
        json!({"targetNodeId": ab["id"], "mode": "insert-children",
               "html": "<div style=\"padding:8px\"><p>One</p></div><div style=\"padding:8px\"><p>Two</p></div>"}),
    );
    let (a, b) = (
        r["createdNodeIds"][0].clone(),
        r["createdNodeIds"][1].clone(),
    );
    let ab_n = num(&ab["id"]);
    assert_eq!(
        h.ed.doc.get(ab_n).unwrap().layout_child.sizing[1],
        Sizing::Hug
    );

    let up = call(
        &mut h,
        "update_styles",
        json!({"updates": [{"nodeIds": [a.clone()], "styles": {"backgroundColor": "linear-gradient(90deg, red, blue)", "boxShadow": "0 1px 2px black", "sketch": true, "roughness": 2}}]}),
    );
    assert_eq!(up["updated"], 1);
    assert_eq!(
        up["ignoredStyles"][a.as_str().unwrap()],
        json!(["boxShadow"])
    );
    let an = h.ed.doc.get(num(&a)).unwrap();
    assert!(matches!(an.style.fills[0], Paint::Linear { .. }));
    assert_eq!(an.style.rough.unwrap().roughness, 2.0);

    // Text edits refit the layer.
    let one = h.ed.doc.get(num(&a)).unwrap().children[0];
    let key = exec::node_key(&h.ed, 0, one);
    let w0 = h.ed.doc.get(one).unwrap().rect.w;
    call(
        &mut h,
        "set_text_content",
        json!({"updates": [{"nodeId": key, "textContent": "One, now longer"}]}),
    );
    assert_eq!(
        h.ed.doc.get(one).unwrap().kind.text().unwrap(),
        "One, now longer"
    );
    let _ = w0;

    // Move b before a, then duplicate a, then delete the copy.
    call(
        &mut h,
        "move_nodes",
        json!({"moves": [{"nodeId": b, "before": a}]}),
    );
    let kids = h.ed.doc.get(ab_n).unwrap().children.clone();
    assert_eq!(kids, vec![num(&b), num(&a)]);
    let d = call(&mut h, "duplicate_nodes", json!({"nodes": [{"id": a}]}));
    let copy = d["duplicates"][0]["newId"].clone();
    assert_eq!(
        d["duplicates"][0]["descendantIdMap"]
            .as_object()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(h.ed.doc.get(ab_n).unwrap().children.len(), 3);
    call(
        &mut h,
        "rename_nodes",
        json!({"updates": [{"nodeId": copy, "name": "Copy"}]}),
    );
    assert_eq!(h.ed.doc.get(num(&copy)).unwrap().name, "Copy");
    let del = call(&mut h, "delete_nodes", json!({"nodeIds": [copy, "nope:1"]}));
    assert_eq!(del["deleted"], 1);
    h.ed.doc.validate().unwrap();

    // Replace swaps a node for new markup in the same slot.
    let rep = call(
        &mut h,
        "write_html",
        json!({"targetNodeId": b, "mode": "replace", "html": "<p>Replaced</p>"}),
    );
    let new = num(&rep["createdNodeIds"][0]);
    assert_eq!(h.ed.doc.get(ab_n).unwrap().children[0], new);
}

#[test]
fn pages_projects_and_whiteboard_shapes() {
    let mut h = host();
    let p = call(&mut h, "create_page", json!({"name": "Ideas"}));
    assert_eq!(h.ed.active_page, 0, "the user's page doesn't change");
    let page = p["pageId"].as_str().unwrap().to_string();
    // Shapes on the other page, bound by an arrow.
    let a = call(
        &mut h,
        "create_shape",
        json!({"type": "rectangle", "x": 0, "y": 0, "width": 120, "height": 80, "pageId": page, "styles": {"sketch": true, "fillStyle": "cross-hatch", "backgroundColor": "#ffec99"}}),
    );
    let b = call(
        &mut h,
        "create_shape",
        json!({"type": "ellipse", "x": 300, "y": 0, "width": 100, "height": 100, "pageId": page}),
    );
    let arrow = call(
        &mut h,
        "create_shape",
        json!({"type": "arrow", "startNodeId": a["id"], "endNodeId": b["id"], "pageId": page}),
    );
    let sticky = call(
        &mut h,
        "create_shape",
        json!({"type": "sticky", "text": "Ship it", "x": 0, "y": 200, "pageId": page}),
    );
    assert!(
        h.ed.doc.nodes.is_empty(),
        "nothing landed on the user's page"
    );
    let doc = h.ed.page_doc(1);
    doc.validate().unwrap();
    assert_eq!(doc.nodes.len(), 4);
    let NodeKind::Connector { start, end, .. } = &doc.get(num(&arrow["id"])).unwrap().kind else {
        panic!()
    };
    assert!(start.is_some() && end.is_some());
    assert_eq!(
        doc.get(num(&sticky["id"])).unwrap().kind.text().unwrap(),
        "Ship it"
    );
    assert!(doc.get(num(&a["id"])).unwrap().style.rough.is_some());

    // Undo on that page only.
    call(&mut h, "undo", json!({"pageId": page}));
    assert_eq!(h.ed.page_doc(1).nodes.len(), 3);

    // Projects: list, create, open.
    call(
        &mut h,
        "rename_pages",
        json!({"updates": [{"pageId": page, "name": "Board"}]}),
    );
    call(&mut h, "finish_working_on_nodes", json!({}));
    let made = call(&mut h, "create_file", json!({"name": "Second"}));
    let list = call(&mut h, "list_resources", json!({}));
    assert_eq!(list["items"].as_array().unwrap().len(), 2);
    let opened = call(&mut h, "open_file", json!({"fileId": made["fileId"]}));
    assert_eq!(opened["fileName"], "Second");
    // Reopen the first: the work was saved, pages and names included.
    let first = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == "MCP test")
        .unwrap()["id"]
        .clone();
    let info = call(&mut h, "open_file", json!({"fileId": first}));
    assert_eq!(info["pages"][1]["name"], "Board");
    assert_eq!(info["pages"][1]["nodeCount"], 3);
    // A fileId that isn't open is refused rather than silently editing another project.
    let err = exec::call(&mut h, "get_basic_info", &json!({"fileId": made["fileId"]})).unwrap_err();
    assert!(err.contains("open_file"), "{err}");
}

#[test]
fn errors_are_reported_not_panics() {
    let mut h = host();
    for (name, args) in [
        ("get_node_info", json!({"nodeId": "999"})),
        (
            "write_html",
            json!({"targetNodeId": "1", "mode": "insert-children", "html": "<p>x</p>"}),
        ),
        (
            "create_artboard",
            json!({"name": "x", "styles": {"width": "10px"}}),
        ),
        ("move_nodes", json!({"moves": [{"nodeId": "1"}]})),
        ("nonexistent_tool", json!({})),
        ("get_guide", json!({"topic": "nope"})),
    ] {
        assert!(
            exec::call(&mut h, name, &args).is_err(),
            "{name} should fail"
        );
    }
}

#[test]
fn svg_view_box_scales_the_icon_to_its_size() {
    let mut h = host();
    let ab = call(
        &mut h,
        "create_artboard",
        json!({"name": "Icons", "styles": {"width": "200px", "height": "100px"}}),
    );
    // A 48-unit icon drawn at 18 px. HTML parsing lowercases attribute names, so `viewBox`
    // has to keep its case when the <svg> goes to the importer.
    let r = call(
        &mut h,
        "write_html",
        json!({
            "targetNodeId": ab["id"], "mode": "insert-children",
            "html": r##"<svg width="18" height="18" viewBox="0 0 48 48"><path d="M0 0H48V48H0Z" fill="#000"/></svg>"##,
        }),
    );
    let icon = num(&r["createdNodeIds"][0]);
    let doc = &h.ed.doc;
    let frame = doc.get(icon).unwrap();
    assert_eq!((frame.rect.w, frame.rect.h), (18.0, 18.0));
    let shape = doc.get(frame.children[0]).unwrap();
    assert!(
        (shape.rect.w - 18.0).abs() < 0.01 && (shape.rect.h - 18.0).abs() < 0.01,
        "{:?}",
        shape.rect
    );
}

#[test]
fn screenshot_returns_png() {
    if sketch_rs::render::gpu::Headless::new().is_err() {
        eprintln!("no GPU adapter; skipping");
        return;
    }
    let mut h = host();
    let ab = call(
        &mut h,
        "create_artboard",
        json!({"name": "S", "styles": {"width": "200px", "height": "100px", "backgroundColor": "#ff0000"}}),
    );
    let outs = exec::call(
        &mut h,
        "get_screenshot",
        &json!({"nodeId": ab["id"], "scale": 2}),
    )
    .unwrap();
    let Out::Image { data, mime } = &outs[0] else {
        panic!()
    };
    assert_eq!(mime, "image/png");
    let img = image::load_from_memory(data).unwrap().to_rgba8();
    assert_eq!(img.dimensions(), (400, 200));
    let p = img.get_pixel(200, 100).0;
    assert!(p[0] > 240 && p[1] < 20 && p[2] < 20, "{p:?}");
}

// ------------------------------------------------------------------------------ end to end

/// Serve MCP on a free port from a thread that owns a headless session, like the app does.
/// The worker hands back the final document and undo depth when the server stops.
fn spawn_app() -> (
    mcp::server::Running,
    std::thread::JoinHandle<(sketch_rs::model::Document, usize)>,
) {
    let (tx, rx) = std::sync::mpsc::channel::<mcp::Call>();
    let worker = std::thread::spawn(move || {
        let mut h = host();
        mcp::serve_host(&mut h, rx);
        (h.ed.doc.clone(), h.ed.history.undo_len())
    });
    let tx = std::sync::Mutex::new(tx);
    let running = mcp::server::spawn(
        0,
        std::sync::Arc::new(move |c| tx.lock().unwrap().send(c).is_ok()),
    )
    .unwrap();
    (running, worker)
}

fn args(v: Value) -> CallToolRequestParams {
    let mut p = CallToolRequestParams::new("");
    if let Value::Object(m) = v {
        p.arguments = Some(m);
    }
    p
}

fn tool(name: &str, v: Value) -> CallToolRequestParams {
    let mut p = args(v);
    p.name = name.to_string().into();
    p
}

fn text_of(r: &CallToolResult) -> Value {
    assert_ne!(r.is_error, Some(true), "{r:?}");
    let t = r
        .content
        .iter()
        .find_map(|c| match c {
            ContentBlock::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .unwrap();
    serde_json::from_str(&t).unwrap_or(Value::String(t))
}

/// What an agent does: draw a frame with text and look at it.
async fn draw_frame_with_text<S: rmcp::service::Service<rmcp::RoleClient>>(
    client: &rmcp::service::RunningService<rmcp::RoleClient, S>,
) -> String {
    let tools = client.list_all_tools().await.unwrap();
    assert!(tools.iter().any(|t| t.name == "write_html"));
    let ab = text_of(&client.call_tool(tool("create_artboard", json!({"name": "Hello", "styles": {"width": "320px", "height": "200px", "padding": "24px"}}))).await.unwrap());
    let id = ab["id"].as_str().unwrap().to_string();
    let w = text_of(
        &client
            .call_tool(tool("write_html", json!({"targetNodeId": id, "mode": "insert-children", "html": "<h1 style=\"color:#2563eb\">Hello from MCP</h1>"})))
            .await
            .unwrap(),
    );
    assert_eq!(w["createdNodeIds"].as_array().unwrap().len(), 1);
    let tree = text_of(
        &client
            .call_tool(tool("get_tree_summary", json!({"nodeId": id})))
            .await
            .unwrap(),
    );
    assert!(tree.as_str().unwrap().contains("Hello from MCP"), "{tree}");
    // Errors come back as tool errors, not protocol failures.
    let bad = client
        .call_tool(tool("get_node_info", json!({"nodeId": "nope"})))
        .await
        .unwrap();
    assert_eq!(bad.is_error, Some(true));
    id
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_client_draws_a_frame_with_text() {
    let (running, worker) = spawn_app();
    let transport = rmcp::transport::StreamableHttpClientTransport::from_uri(running.url());
    let client = ().serve(transport).await.unwrap();
    let info = client.peer_info().unwrap();
    assert_eq!(info.server_info.as_ref().unwrap().name, "sketch-rs");
    assert!(
        info.instructions
            .as_deref()
            .unwrap_or("")
            .contains("write_html")
    );
    let id = draw_frame_with_text(&client).await;
    client.cancel().await.unwrap();
    running.stop();
    // The edit landed in the editor's document, through its undo history.
    let (doc, undo_len) = worker.join().unwrap();
    let frame = doc.get(num(&json!(id))).unwrap();
    assert!(frame.is_frame());
    let text = doc.get(frame.children[0]).unwrap();
    assert_eq!(text.kind.text().unwrap(), "Hello from MCP");
    assert_eq!(undo_len, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_rejects_browsers_and_unknown_routes() {
    let (running, _worker) = spawn_app();
    let base = format!("http://127.0.0.1:{}", running.port);
    let init = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"1"}}}"#;
    let http = reqwest::Client::new();
    let r = http
        .post(format!("{base}/mcp"))
        .header("content-type", "application/json")
        .header("origin", "https://evil.example")
        .body(init)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    // Missing Accept header is repaired.
    let r = http
        .post(format!("{base}/mcp"))
        .header("content-type", "application/json")
        .body(init)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{:?}", r.text().await);
    let r = http
        .get(format!("{base}/.well-known/oauth-authorization-server"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    assert!(r.text().await.unwrap().contains("/mcp"));
    running.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdio_bridge_relays_to_the_app() {
    let (running, _worker) = spawn_app();
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_sketch-rs-mcp"));
    cmd.env("SKETCH_RS_MCP_PORT", running.port.to_string());
    let child = rmcp::transport::TokioChildProcess::new(cmd).unwrap();
    let client = ().serve(child).await.unwrap();
    draw_frame_with_text(&client).await;
    client.cancel().await.unwrap();
    running.stop();
}

/// Protocol 2026-07-28 clients (e.g. Claude Code) skip `initialize`, send the revision in each
/// request's `_meta`, and reject `tools/call` results without `resultType`. The app's HTTP leg
/// answers the bridge in an older revision, so the bridge must add it back.
#[test]
fn stdio_bridge_results_carry_result_type_for_2026_clients() {
    use std::io::{BufRead, Write};
    let (running, _worker) = spawn_app();
    let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_sketch-rs-mcp"))
        .env("SKETCH_RS_MCP_PORT", running.port.to_string())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let meta = r#""_meta":{"io.modelcontextprotocol/protocolVersion":"2026-07-28","io.modelcontextprotocol/clientInfo":{"name":"raw","version":"1"},"io.modelcontextprotocol/clientCapabilities":{}}"#;
    let mut stdin = child.stdin.take().unwrap();
    writeln!(
        stdin,
        r#"{{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{{"name":"create_artboard","arguments":{{"name":"Raw","styles":{{"width":"100px","height":"100px"}}}},{meta}}}}}"#
    )
    .unwrap();
    let mut line = String::new();
    std::io::BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    drop(stdin);
    let _ = child.kill();
    let _ = child.wait();
    running.stop();
    let v: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(v["id"], 7, "{line}");
    assert_eq!(v["result"]["resultType"], "complete", "{line}");
    assert!(
        v["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("\"Raw\""),
        "{line}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stdio_bridge_lists_tools_without_the_app() {
    // A port nothing listens on.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_sketch-rs-mcp"));
    cmd.env("SKETCH_RS_MCP_PORT", port.to_string());
    let client = ().serve(rmcp::transport::TokioChildProcess::new(cmd).unwrap()).await.unwrap();
    assert!(client.list_all_tools().await.unwrap().len() >= 30);
    let r = client
        .call_tool(tool("get_basic_info", json!({})))
        .await
        .unwrap();
    assert_eq!(r.is_error, Some(true));
    let ContentBlock::Text(t) = &r.content[0] else {
        panic!()
    };
    assert!(t.text.contains("not running"), "{}", t.text);
    client.cancel().await.unwrap();
}

/// A design written the way an agent would, rendered for the docs (`milestone7_mcp.png`).
#[test]
fn agent_design_screenshot() {
    if sketch_rs::render::gpu::Headless::new().is_err() {
        eprintln!("no GPU adapter; skipping");
        return;
    }
    let mut h = host();
    let ab = call(
        &mut h,
        "create_artboard",
        json!({"name": "Sign in", "styles": {"width": "390px", "height": "fit-content", "padding": "32px 24px", "gap": "24px", "backgroundColor": "#f8fafc"}}),
    );
    let id = ab["id"].clone();
    for html in [
        r#"<div layer-name="Header" style="display:flex; align-items:center; gap:12px">
             <div style="width:40px; height:40px; border-radius:12px; background:linear-gradient(135deg, #6366f1, #ec4899)"></div>
             <span style="font-size:20px; font-weight:700; color:#0f172a">Sketch</span></div>"#,
        r#"<div layer-name="Intro" style="display:flex; flex-direction:column; gap:8px">
             <h1 style="font-size:28px; color:#0f172a; line-height:34px">Welcome back</h1>
             <p style="font-size:15px; color:#475569; line-height:22px">Sign in to pick up <b>where you left off</b>. Your boards sync locally.</p></div>"#,
        r#"<div layer-name="Form" style="display:flex; flex-direction:column; gap:12px; padding:20px; background:#ffffff; border:1px solid #e2e8f0; border-radius:16px">
             <label style="font-size:13px; font-weight:600; color:#334155">Email</label>
             <div style="padding:12px 14px; border:1px solid #cbd5e1; border-radius:10px; color:#94a3b8; font-size:15px">you@example.com</div>
             <label style="font-size:13px; font-weight:600; color:#334155">Password</label>
             <div style="padding:12px 14px; border:1px solid #cbd5e1; border-radius:10px; color:#94a3b8; font-size:15px">••••••••</div>
             <div style="display:flex; justify-content:center; align-items:center; height:48px; border-radius:10px; background:#4f46e5; color:#ffffff; font-size:16px; font-weight:600">Continue</div></div>"#,
        r#"<div style="display:flex; justify-content:space-between"><span style="font-size:14px; color:#4f46e5">Forgot password?</span><span style="font-size:14px; color:#64748b">Create account</span></div>"#,
    ] {
        call(
            &mut h,
            "write_html",
            json!({"targetNodeId": id, "mode": "insert-children", "html": html}),
        );
    }
    h.ed.doc.validate().unwrap();
    let outs = exec::call(&mut h, "get_screenshot", &json!({"nodeId": id, "scale": 2})).unwrap();
    let Out::Image { data, .. } = &outs[0] else {
        panic!()
    };
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/screenshots");
    std::fs::write(dir.join("milestone7_mcp.png"), data).unwrap();
    let ab_node = h.ed.doc.get(num(&id)).unwrap();
    assert!(
        ab_node.rect.h > 400.0,
        "artboard hugs its content: {:?}",
        ab_node.rect
    );
}
