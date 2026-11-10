//! Websites: captured in a private browser, checked by serving the build and comparing
//! screenshots with the original's.

use super::*;
use crate::rebuild::project::run_as_is_asked;
use craft_anything_engine::workspace::{Claim, Focus, PageLikeness};
use serde::{Deserialize, Serialize};

pub struct WebsiteKind;

impl WebsiteKind {
    pub const ID: &'static str = "website";
}

/// Other pages of the site studied and checked besides the one the person gave, at most (the
/// setting `build.max_pages` chooses how many up to this).
pub const MAX_ROUTES: usize = 12;
/// Where the other pages are listed in the capture folder.
pub const ROUTES_FILE: &str = "routes.json";
/// Where the start page's opened menus, pop-ups and tabs are kept in the capture folder, at the
/// desktop width; [`phone_states_dir`] holds them at the phone width (a menu button often only
/// shows there).
pub const STATES_DIR: &str = "states";

/// The folder for the states opened at a phone `width`: `states-390`.
pub fn phone_states_dir(width: u32) -> String {
    format!("{STATES_DIR}-{width}")
}

/// Where the focused part's own capture lives (R2.11): capture/focus/, the view the person asked
/// for as its own page (or cut out of the page's screenshots).
pub const FOCUS_DIR: &str = "focus";

/// The focused part's own saved files, inside `capture/<focus>/` (R2.12).
pub const RESOURCES_DIR: &str = "resources";

/// The focused game's photographed play states, inside `capture/<focus>/` (R2.13): the screens a
/// player reaches, which the check replays against the build.
pub const PLAY_STATES_DIR: &str = "play-states";

/// The embedded view the focus points at in a capture's `embeds` (R2.11): the one with the
/// focus's element id, else the biggest the capture found.
pub fn pick_focus_embed<'a>(capture: &'a Value, focus: &Focus) -> Option<&'a Value> {
    let embeds = capture.get("embeds").and_then(Value::as_array)?;
    if let Some(id) = &focus.element_id
        && let Some(by_id) = embeds.iter().find(|e| e.get("id").and_then(Value::as_str) == Some(id.as_str()))
    {
        return Some(by_id);
    }
    let area = |e: &Value| {
        e.get("boxes").and_then(Value::as_array).into_iter().flatten().fold(0u64, |a, b| {
            let w = b.get("w").and_then(Value::as_u64).unwrap_or(0);
            let h = b.get("h").and_then(Value::as_u64).unwrap_or(0);
            a.max(w.saturating_mul(h))
        })
    };
    let mut best: Option<(u64, &Value)> = None;
    for e in embeds.iter() {
        let a = area(e);
        if a > 0 && best.is_none_or(|(b, _)| a > b) {
            best = Some((a, e));
        }
    }
    best.map(|(_, e)| e)
}

/// What the builder is told when the person asked for one part of the page only (R2.11): the
/// part's own capture is the look to match, and the page around it is out of scope.
fn focus_brief(focus: &Focus) -> String {
    format!(
        "The rebuild is the {} alone — the person asked for just that part, and the page around it is out of scope: don't rebuild it and don't compare yourself to it. The {}'s own capture is in capture/{}/ (screenshot-<width>.png at each width and its own capture.json; open every picture with the Read tool before you plan). Match those pictures closely: the same layout and proportions, sizes, colours, fonts, and where every control sits. The rebuild is the {} itself, filling the window. The checks compare it with capture/{}/ only.",
        focus.what, focus.what, FOCUS_DIR, focus.what, FOCUS_DIR
    )
}

/// What the builder is told when the focused part's own files were saved (R2.12). The default is
/// a true rebuild (the person, 2026-11-09): the game is the builder's own web-stack
/// implementation, written from the original's code, with the original's art and data as they
/// are — never the original's compiled runtime as the game. Only a request that asks for the
/// original's own files to run as they are ([`run_as_is_asked`]) gets that instead.
fn focus_resources_brief(focus: &Focus, ws: &Workspace) -> Option<String> {
    if !ws.capture_dir().join(FOCUS_DIR).join(RESOURCES_DIR).join("resources.json").is_file() {
        return None;
    }
    let what = &focus.what;
    let files = format!(
        "The {}'s own files (its code, data, pictures, sounds) were saved into capture/{}/resources/: resources.json maps every address the browser loaded to its bytes under files/.",
        what, FOCUS_DIR
    );
    if run_as_is_asked(&ws.manifest.request) {
        return Some(format!(
            "{files} The person's request asks for the game's own files to run as they are, and that is what you do: where anything above says to rebuild the behaviour yourself, use placeholders, or keep the original's code out of app/, the game's own files win.\n\
             - Pictures, sounds, fonts and data: copy every one the game uses into app/ and use them as they are.\n\
             - Code: the game's code ships as what runs it (a compiled runtime such as a Unity WebGL build, engine data files, or one project file that drives the game): run those files as they are in app/ and write only the code around them — the loader, the bridge to the page, the UI. Read bundles with js_sources_restore and js_app_map where the code around them needs adapting; data files open as they are.\n\
             List what the build uses of the original's files in your summary. If something in them can't be used, say why."
        ));
    }
    Some(format!(
        "{files} The person wants a true rebuild of this game: your own code that plays like it, written from its code, for their own use on this computer. That is what you do by default — where anything above says to rebuild the behaviour yourself, use placeholders, or keep the original's code out of app/, this game's own files and code win. Work in these stages, in order:\n\
         1) Take stock of what was saved (capture/{FOCUS_DIR}/{RESOURCES_DIR}/resources.json lists it) and copy every file the game uses into app/ and use them as they are — never redraw, regenerate or substitute a file that is already here. Its lists and tables (levels, items, prices, timings) go in unchanged. When a file the game needs wasn't saved, fetch the original from the game's own server first; a stand-in is the last resort, and every stand-in or fetched file is named in app/stand-ins.json ({{\"file\": \"…\", \"kind\": \"stand-in\" or \"original\", \"stands_for\": \"…\"}}) — the checks fail an asset that is neither the game's own nor named there.\n\
         2) Recover the mechanics from its code before you write any of yours. Read it (js_sources_restore and js_app_map for bundles; data files open as they are); where it is compiled, recover what it does — formats_wasm_module_show and formats_wasm_function_show read a wasm build, and game_unity_list, game_unity_asset_export and game_unity_code_names_show read Unity data, bundles and metadata. Put every constant, formula and rule into app/MECHANICS.md with where it came from (a file, a name or address, or an evidence id) or mark it \"estimated\". Never write your own analysis scripts: these tools answer in one call each, and a question that needs more than a few calls becomes a core_unknown_add with what you did instead — the build goes on.\n\
         3) Build the interface against every screen: the play states in capture/{FOCUS_DIR}/{PLAY_STATES_DIR}/ where the study photographed them, and the pictures in reference/.\n\
         4) Implement the game from app/MECHANICS.md: build it as a web game in your own code (HTML and JavaScript — canvas, Three.js or whatever fits), unless their request names another stack. No game-engine project. The game's own code is what you write the game from: where its logic is readable (scripts, project data), adapt it into app/ keeping its meaning — the same formulas, constants, timings and states. Open-source libraries it ships — engines like PlayCanvas, codecs like draco, physics, loaders — are yours to use as they are, copied into app/ unchanged: open-source libraries are never rebuilt. Never run the game's own compiled runtime (a Unity or Unreal build, its engine data) as the game: the game is your own implementation.\n\
         5) Verify per screen: rebuild_check replays the recorded actions against your build and compares every one; fix what it names, and say in your summary what stays estimated."
    ))
}

/// What the builder is told when the focused game's play states were photographed (R2.13):
/// these are the screens the game really shows, and the check replays the same actions against
/// the build — matching the first picture alone is no longer enough.
fn play_states_brief(focus: &Focus, ws: &Workspace) -> Option<String> {
    if !ws.capture_dir().join(FOCUS_DIR).join(PLAY_STATES_DIR).join("states.json").is_file() {
        return None;
    }
    Some(format!(
        "The {}'s play states were photographed by playing it (capture/{}/{}/: state-<n>.png, each reached by the action states.json records). Open every one with the Read tool: they are the screens the game really shows — the start view and what tapping into it reaches — and the checks replay the same actions against your build and compare each screen. Match every one, not just the first picture.",
        focus.what, FOCUS_DIR, PLAY_STATES_DIR
    ))
}

/// The phone width among the widths compared (the narrowest under 768 px), if any.
pub fn phone_width(widths: &[u32]) -> Option<u32> {
    widths.iter().copied().filter(|w| *w < 768).min()
}

/// The folders of opened states in a capture folder (`states`, then `states-<width>`).
pub fn state_dirs(capture: &Path) -> Vec<String> {
    let mut dirs: Vec<String> = std::fs::read_dir(capture)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().to_str().map(String::from))
        .filter(|n| n == STATES_DIR || n.strip_prefix("states-").is_some_and(|w| w.parse::<u32>().is_ok()))
        .collect();
    dirs.sort();
    dirs
}

/// The opened states the study pictured, as `states/state-<n>.png (label, kind, width)` (none
/// when there were none).
pub fn pictured_states(ws: &Workspace) -> Vec<String> {
    let mut out = Vec::new();
    for dir in state_dirs(&ws.capture_dir()) {
        let Some(v) = read_json(&ws.capture_dir().join(&dir).join("states.json")) else { continue };
        let width = v.get("width").and_then(Value::as_u64).map(|w| format!(", {w} px")).unwrap_or_default();
        out.extend(v.get("states").and_then(Value::as_array).into_iter().flatten().filter_map(|s| {
            let file = s.get("file").and_then(Value::as_str)?;
            let label = s.get("label").and_then(Value::as_str).filter(|l| !l.is_empty()).unwrap_or("a control");
            Some(format!("{dir}/{file} ({label}, {}{width})", s.get("kind").and_then(Value::as_str).unwrap_or("menu")))
        }));
    }
    out
}

/// Another page of the site: its path, the folder name its capture uses, and its address.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Route {
    pub path: String,
    pub slug: String,
    pub url: String,
}

/// Up to `max` other pages of the same site, from the links a capture found (`capture.json`'s
/// `links`): same scheme and host, a page rather than a file, each path once, not the start page.
pub fn pick_routes(capture: &Value, start: &str, max: usize) -> Vec<Route> {
    let origin_and_path = |u: &str| -> Option<(String, String)> {
        let (scheme, rest) = u.split_once("://")?;
        if scheme != "http" && scheme != "https" {
            return None;
        }
        let rest = rest.split(['#', '?']).next().unwrap_or_default();
        let (host, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, "/".into()));
        Some((format!("{scheme}://{}", host.to_ascii_lowercase()), path))
    };
    let Some((origin, start_path)) = origin_and_path(start) else { return Vec::new() };
    let norm = |p: &str| p.trim_end_matches('/').to_string();
    let mut out: Vec<Route> = Vec::new();
    for link in capture.get("links").and_then(Value::as_array).into_iter().flatten() {
        if out.len() >= max {
            break;
        }
        let Some((o, path)) = link.get("href").and_then(Value::as_str).and_then(origin_and_path) else { continue };
        let last = path.rsplit('/').next().unwrap_or_default();
        let file = last.contains('.') && !last.ends_with(".html") && !last.ends_with(".htm");
        if o != origin || file || norm(&path) == norm(&start_path) || out.iter().any(|r| norm(&r.path) == norm(&path)) {
            continue;
        }
        let slug: String = path.trim_matches('/').chars().map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '-' }).collect();
        let slug = slug.split('-').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("-");
        if slug.is_empty() || out.iter().any(|r| r.slug == slug) {
            continue;
        }
        out.push(Route { url: format!("{origin}{path}"), path, slug });
    }
    out
}

/// The next level: up to `max` pages linked from already captured ones (`captures`, their
/// `capture.json`s) that aren't in `have` yet, same rules as [`pick_routes`].
pub fn pick_deeper_routes(captures: &[Value], start: &str, have: &[Route], max: usize) -> Vec<Route> {
    let norm = |p: &str| p.trim_end_matches('/').to_string();
    let mut out: Vec<Route> = Vec::new();
    for c in captures {
        for r in pick_routes(c, start, MAX_ROUTES) {
            if out.len() >= max {
                return out;
            }
            let taken = |x: &Route| norm(&x.path) == norm(&r.path) || x.slug == r.slug;
            if !have.iter().any(taken) && !out.iter().any(taken) {
                out.push(r);
            }
        }
    }
    out
}

/// The other pages captured for a project (none when only the start page was).
pub fn routes(ws: &Workspace) -> Vec<Route> {
    std::fs::read(ws.capture_dir().join(ROUTES_FILE)).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

impl TargetKind for WebsiteKind {
    fn id(&self) -> &'static str {
        Self::ID
    }

    fn words(&self) -> KindWords {
        KindWords {
            button: "Open the website",
            icon: "external-link",
            ready: "Your website is ready",
            change_hint: "Ask for a change, e.g. \"make the header blue\" or \"add a contact form\"",
        }
    }

    fn detect(&self, cx: &DetectCx) -> Option<u32> {
        if matches!(cx.target, TargetRef::Website { .. }) {
            Some(100)
        } else if cx.says(&[" website", " web page", " webpage", " landing page", " site ", " webseite", " internetseite", "网站", "網站", "网页", "網頁"])
        {
            Some(50)
        } else if matches!(cx.target, TargetRef::None) && stacks::get(cx.stack).is_some_and(|s| s.site) {
            // Built as a site with nothing to recreate: "add a contact form" never says website.
            Some(30)
        } else {
            None
        }
    }

    /// A stack the request doesn't name is the builder's to choose, matching the original's.
    fn default_stack(&self, _cx: &DetectCx) -> &'static str {
        stacks::BUILDERS_CHOICE
    }

    fn goal(&self) -> &'static str {
        "a website that looks and behaves like the original"
    }

    fn self_screenshots(&self) -> bool {
        false
    }

    fn brief_sections(&self, cx: &BriefCx) -> Vec<String> {
        // A focused rebuild (R2.11) is only about its part: the page's other pages and menus are
        // out of scope and never studied, so only the focus section is said (with what its own
        // files add, when they were saved — R2.12).
        if let Some(focus) = &cx.ws.manifest.focus {
            let mut out = vec![focus_brief(focus)];
            out.extend(focus_resources_brief(focus, cx.ws));
            out.extend(play_states_brief(focus, cx.ws));
            return out;
        }
        let mut out = Vec::new();
        let routes = routes(cx.ws);
        if !routes.is_empty() {
            let list = routes.iter().map(|r| format!("{} (capture/routes/{}/)", r.path, r.slug)).collect::<Vec<_>>().join(", ");
            out.push(format!(
                "The site has more pages, captured the same way: {list}. Rebuild each at the same path, with the same navigation between them; they're compared with the original too."
            ));
        }
        let states = pictured_states(cx.ws);
        if !states.is_empty() {
            out.push(format!(
                "Its menus, pop-ups and tabs were opened one at a time and photographed in capture/ (on a computer, and on a phone where its controls differ, such as a menu button): {}. Make them open and look the same way at each width.",
                states.join(", ")
            ));
        }
        out
    }

    /// A builder that carries on its session sees only this: where the focused part's capture is
    /// (R2.11), in case it was brought by a re-study since the brief.
    fn follow_up_sections(&self, cx: &BriefCx) -> Vec<String> {
        let Some(focus) = &cx.ws.manifest.focus else { return Vec::new() };
        let mut out = vec![focus_brief(focus)];
        out.extend(focus_resources_brief(focus, cx.ws));
        out.extend(play_states_brief(focus, cx.ws));
        out
    }

    fn checks(&self, ws: &Workspace) -> Vec<Box<dyn Check>> {
        // A site made from a request alone has nothing to compare with: it must build.
        if matches!(ws.manifest.target, TargetRef::Website { .. }) { vec![Box::new(WebVisualCheck)] } else { vec![Box::new(BuildCheck)] }
    }

    fn refine(&self, check: &CheckResult) -> String {
        match check.similarity {
            Some(s) => {
                // A focused rebuild (R2.11) has one pair to look at: the part's own capture.
                let (folders, originals) = match &check.focus {
                    Some(what) => (String::new(), format!("the {what}'s own capture is in capture/{}/.", FOCUS_DIR)),
                    None => (", and routes/<page>/ for other pages".to_string(), "the original's are in capture/.".to_string()),
                };
                format!(
                    "Check {}: compared with the original, your build looks {s:.0}% alike ({}). Your screenshots are in checks/round-{}/ (screenshot-<width>.png for each width{folders}) with the layout measured at each width in its capture.json; {originals} Open each pair with the Read tool, fix the biggest visible differences first (layout and section order, spacing, sizes, colours, fonts, missing parts), fix the padding, items per row, sizes, colours and fonts the check names at each width with the exact values it gives, add any missing headings or words it lists, build it again, and reply with 1-2 plain sentences on what you changed.",
                    check.round, check.note, check.round
                )
            }
            None => format!(
                "Check {}: {}\n\nFix this in app/, build again until it succeeds, and reply with 1-2 plain sentences on what you changed.",
                check.round, check.note
            ),
        }
    }
}

/// How alike two captures' visible text is (`capture.json`'s `text` and `headings`).
#[derive(Debug, Clone, PartialEq)]
pub struct TextLikeness {
    /// Word overlap, 0..=100 (Dice over word counts).
    pub similarity: f64,
    /// The original's headings the build doesn't have, in page order (at most 8).
    pub missing_headings: Vec<String>,
    /// The original's most frequent words the build never uses (at most 12).
    pub missing_words: Vec<String>,
}

pub(crate) fn words(text: &str) -> std::collections::BTreeMap<String, usize> {
    let mut m = std::collections::BTreeMap::new();
    for w in text.split(|c: char| !c.is_alphanumeric()).filter(|w| w.chars().count() >= 2) {
        *m.entry(w.to_lowercase()).or_insert(0) += 1;
    }
    m
}

/// Compare the visible text of the original's capture with the build's. None when the original
/// has no text to compare.
pub fn text_likeness(original: &Value, built: &Value) -> Option<TextLikeness> {
    let text = |v: &Value| v.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
    let (a, b) = (words(&text(original)), words(&text(built)));
    let total_a: usize = a.values().sum();
    if total_a == 0 {
        return None;
    }
    let total_b: usize = b.values().sum();
    let shared: usize = a.iter().map(|(w, n)| (*n).min(b.get(w).copied().unwrap_or(0))).sum();
    let similarity = 200.0 * shared as f64 / (total_a + total_b) as f64;
    let norm = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
    let built_headings: std::collections::BTreeSet<String> =
        built.get("headings").and_then(Value::as_array).into_iter().flatten().filter_map(|h| h.get("text").and_then(Value::as_str)).map(norm).collect();
    let missing_headings = original
        .get("headings")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|h| h.get("text").and_then(Value::as_str))
        .filter(|h| !h.trim().is_empty() && !built_headings.contains(&norm(h)))
        .take(8)
        .map(|h| h.trim().to_string())
        .collect();
    let mut missing: Vec<(&String, &usize)> = a.iter().filter(|(w, _)| !b.contains_key(*w)).collect();
    missing.sort_by(|x, y| y.1.cmp(x.1).then_with(|| x.0.cmp(y.0)));
    let missing_words = missing.into_iter().take(12).map(|(w, _)| w.clone()).collect();
    Some(TextLikeness { similarity, missing_headings, missing_words })
}

impl TextLikeness {
    /// One line for the check's note, e.g. `text 72% alike; missing headings: "Pricing"; missing words: faq, plans`.
    pub fn note(&self) -> String {
        let mut s = format!("text {:.0}% alike", self.similarity);
        if !self.missing_headings.is_empty() {
            s.push_str(&format!("; missing headings: {}", self.missing_headings.iter().map(|h| format!("\"{h}\"")).collect::<Vec<_>>().join(", ")));
        }
        if !self.missing_words.is_empty() {
            s.push_str(&format!("; missing words: {}", self.missing_words.join(", ")));
        }
        s
    }
}

/// The parts of a page the structure is compared by, as (element, plain name). Elements with
/// the same name count as one part (`ul` and `ol` are both a list).
const PARTS: &[(&str, &str)] = &[
    ("header", "header"),
    ("nav", "navigation bar"),
    ("main", "main area"),
    ("footer", "footer"),
    ("section", "section"),
    ("article", "article"),
    ("aside", "sidebar"),
    ("form", "form"),
    ("h1", "top heading"),
    ("h2", "subheading"),
    ("h3", "subheading"),
    ("h4", "subheading"),
    ("ul", "list"),
    ("ol", "list"),
    ("img", "image"),
    ("video", "video"),
    ("button", "button"),
    ("input", "form field"),
    ("select", "form field"),
    ("textarea", "form field"),
];

/// The page's parts in document order, from `capture.json`'s outline (a role counts as its element).
fn skeleton(capture: &Value) -> Vec<&'static str> {
    capture
        .get("outline")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let tag = match item.get("role").and_then(Value::as_str) {
                Some("banner") => "header",
                Some("navigation") => "nav",
                Some("main") => "main",
                Some("contentinfo") => "footer",
                Some("complementary") => "aside",
                Some("form" | "search") => "form",
                Some("button") => "button",
                Some("img") => "img",
                _ => item.get("tag").and_then(Value::as_str)?,
            };
            PARTS.iter().find(|(t, _)| *t == tag).map(|(_, name)| *name)
        })
        .collect()
}

/// Length of the longest sequence both have in the same order.
fn common_len(a: &[&str], b: &[&str]) -> usize {
    let mut row = vec![0usize; b.len() + 1];
    for x in a {
        let mut diag = 0;
        for (j, y) in b.iter().enumerate() {
            let up = row[j + 1];
            row[j + 1] = if x == y { diag + 1 } else { up.max(row[j]) };
            diag = up;
        }
    }
    row[b.len()]
}

/// How alike two captures' page structure is: their landmarks, headings, lists, media and
/// controls, in order.
#[derive(Debug, Clone, PartialEq)]
pub struct StructureLikeness {
    /// 0..=100: the parts both have in the same order, over all parts of both.
    pub similarity: f64,
    /// Parts the original has more of, as (plain name, how many more), most first (at most 6).
    pub missing: Vec<(&'static str, usize)>,
}

/// Compare the original's page structure with the build's. None when the original's outline has
/// none of the parts.
pub fn structure_likeness(original: &Value, built: &Value) -> Option<StructureLikeness> {
    let (a, b) = (skeleton(original), skeleton(built));
    if a.is_empty() {
        return None;
    }
    let similarity = 200.0 * common_len(&a, &b) as f64 / (a.len() + b.len()) as f64;
    let count = |s: &[&str], name: &str| s.iter().filter(|p| **p == name).count();
    let mut missing: Vec<(&'static str, usize)> = Vec::new();
    for (_, name) in PARTS {
        if !missing.iter().any(|(n, _)| n == name) {
            let more = count(&a, name).saturating_sub(count(&b, name));
            if more > 0 {
                missing.push((name, more));
            }
        }
    }
    missing.sort_by_key(|m| std::cmp::Reverse(m.1));
    missing.truncate(6);
    Some(StructureLikeness { similarity, missing })
}

impl StructureLikeness {
    /// One line for the check's note, e.g. `structure 80% alike; missing parts: 2 lists, a form`.
    pub fn note(&self) -> String {
        let mut s = format!("structure {:.0}% alike", self.similarity);
        if !self.missing.is_empty() {
            let parts = self.missing.iter().map(|(name, n)| if *n == 1 { format!("a {name}") } else { format!("{n} {name}s") }).collect::<Vec<_>>();
            s.push_str(&format!("; missing parts: {}", parts.join(", ")));
        }
        s
    }
}

/// What the build gets wrong that the original doesn't, from both captures' `health` and `a11y`:
/// script and console errors, requests that fail (compared by path, since the build is served
/// from elsewhere), and controls without a name, pictures without alt text or a missing page
/// language. Problems the original has too aren't the build's to fix.
pub fn health_notes(original: &Value, built: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let new_errors = new_errors(original, built);
    if !new_errors.is_empty() {
        let shown: Vec<String> = new_errors.iter().take(3).map(|t| format!("\"{t}\"")).collect();
        out.push(format!("the build logs {} error(s) the original doesn't: {}", new_errors.len(), shown.join("; ")));
    }
    let failing = new_failures(original, built);
    if !failing.is_empty() {
        out.push(format!("{} request(s) fail in the build: {}", failing.len(), failing.iter().take(5).cloned().collect::<Vec<_>>().join(", ")));
    }
    let n = |v: &Value, k: &str| v.pointer(&format!("/a11y/{k}")).and_then(Value::as_u64).unwrap_or(0);
    if built.get("a11y").is_some() {
        for (k, what) in [("unnamed_controls", "controls without a name"), ("images_without_alt", "pictures without alt text")] {
            if n(built, k) > n(original, k) {
                out.push(format!("{} {what} (the original has {})", n(built, k), n(original, k)));
            }
        }
        let lang = |v: &Value| v.pointer("/a11y/lang").and_then(Value::as_str).unwrap_or_default().to_string();
        if lang(built).is_empty() && !lang(original).is_empty() {
            out.push(format!("the page language isn't set (the original's is {})", lang(original)));
        }
    }
    out
}

fn health_list(v: &Value, p: &str) -> Vec<Value> {
    v.pointer(p).and_then(Value::as_array).cloned().unwrap_or_default()
}

/// The build's error texts the original doesn't log.
fn new_errors(original: &Value, built: &Value) -> Vec<String> {
    let text = |e: &Value| e.get("text").and_then(Value::as_str).unwrap_or_default().to_string();
    let theirs: Vec<String> = health_list(original, "/health/errors").iter().map(text).collect();
    health_list(built, "/health/errors").iter().map(text).filter(|t| !t.is_empty() && !theirs.contains(t)).collect()
}

/// The build's failing requests the original doesn't have, by path, as `/logo.png (404)`.
fn new_failures(original: &Value, built: &Value) -> Vec<String> {
    let path = |f: &Value| {
        let url = f.get("url").and_then(Value::as_str).unwrap_or_default();
        let rest = url.split_once("://").map_or(url, |(_, r)| r);
        rest.find('/').map_or("/".to_string(), |i| rest.get(i..).unwrap_or("/").to_string())
    };
    let theirs: Vec<String> = health_list(original, "/health/failed_requests").iter().map(path).collect();
    health_list(built, "/health/failed_requests")
        .iter()
        .filter(|f| !theirs.contains(&path(f)))
        .map(|f| {
            let why = f
                .get("status")
                .and_then(Value::as_u64)
                .map(|s| s.to_string())
                .or_else(|| f.get("error").and_then(Value::as_str).map(String::from))
                .unwrap_or_default();
            format!("{} ({why})", path(f))
        })
        .collect()
}

/// What `page` of the build must do, each judged on its own from the two captures: not scroll
/// sideways at any width where the original doesn't, log no errors and fail no requests the
/// original doesn't, and have the original's landmarks. A claim is made only when both captures
/// hold what it needs (older captures make none).
pub fn page_claims(page: &str, original: &Value, built: &Value) -> Vec<Claim> {
    let mut out = Vec::new();
    let n = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    let built_layouts = built.get("layouts").and_then(Value::as_array).cloned().unwrap_or_default();
    for theirs in original.get("layouts").and_then(Value::as_array).into_iter().flatten() {
        let w = n(theirs, "width");
        if n(theirs, "page_w") > w + 1 {
            continue;
        }
        if let Some(ours) = built_layouts.iter().find(|l| n(l, "width") == w) {
            let pw = n(ours, "page_w");
            out.push(Claim::new(format!("{page} doesn't scroll sideways at {w} px"), pw <= w + 1, format!("it's {pw} px wide")));
        }
    }
    if original.get("health").is_some() && built.get("health").is_some() {
        let errors = new_errors(original, built);
        out.push(Claim::new(format!("{page} logs no errors the original doesn't"), errors.is_empty(), errors.first().cloned().unwrap_or_default()));
        let failing = new_failures(original, built);
        out.push(Claim::new(
            format!("{page} loads every file it asks for"),
            failing.is_empty(),
            failing.iter().take(5).cloned().collect::<Vec<_>>().join(", "),
        ));
    }
    let landmarks =
        |v: &Value| -> Vec<String> { v.pointer("/a11y/landmarks").and_then(Value::as_object).map(|m| m.keys().cloned().collect()).unwrap_or_default() };
    if built.get("a11y").is_some() && !landmarks(original).is_empty() {
        let ours = landmarks(built);
        let missing: Vec<String> = landmarks(original).into_iter().filter(|l| !ours.contains(l)).collect();
        out.push(Claim::new(format!("{page} has the original's landmarks"), missing.is_empty(), format!("missing: {}", missing.join(", "))));
    }
    out
}

/// A `#rrggbb` or `#rrggbbaa` colour as (r, g, b, a).
fn rgba(c: &str) -> Option<[u8; 4]> {
    let h = c.strip_prefix('#')?;
    let byte = |i: usize| h.get(i..i + 2).and_then(|x| u8::from_str_radix(x, 16).ok());
    match h.len() {
        6 => Some([byte(0)?, byte(2)?, byte(4)?, 255]),
        8 => Some([byte(0)?, byte(2)?, byte(4)?, byte(6)?]),
        _ => None,
    }
}

/// The same colour to the eye: each channel within 3 (rounding in the browser), or the same text.
fn same_colour(a: &str, b: &str) -> bool {
    match (rgba(a), rgba(b)) {
        (Some(x), Some(y)) => x.iter().zip(y).all(|(p, q)| p.abs_diff(q) <= 3),
        _ => a.eq_ignore_ascii_case(b),
    }
}

/// The first family of a CSS font stack, as a person names it: `"__Figtree_a1b2c3", sans-serif`
/// (how next/font renames a font) reads `Figtree`.
fn family(stack: &str) -> String {
    let first = stack.split(',').next().unwrap_or_default().trim().trim_matches(['"', '\'']);
    first.trim_start_matches('_').split('_').next().unwrap_or(first).to_string()
}

/// The values of a `design` list (`[{value, count}]`), most used first.
fn design_values<'a>(capture: &'a Value, key: &str) -> Vec<&'a str> {
    capture.pointer(&format!("/design/{key}")).and_then(Value::as_array).into_iter().flatten().filter_map(|x| x.get("value").and_then(Value::as_str)).collect()
}

/// What a person sees differ between the original's capture and the build's, for the builder:
/// colours, the main font, text sizes and spacing the build doesn't use, and at each width a
/// page that scrolls sideways, heading sizes, and sections whose padding, items per row or
/// content column differ (sections are paired by tag and heading, in order). Both are
/// `capture.json` documents; empty when nothing differs or there's nothing to compare.
pub fn design_notes(original: &Value, built: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut colours = Vec::new();
    for (key, what) in [("backgrounds", "background"), ("text_colors", "text")] {
        let ours = design_values(built, key);
        colours.extend(design_values(original, key).into_iter().take(6).filter(|c| !ours.iter().any(|o| same_colour(c, o))).map(|c| format!("{c} ({what})")));
    }
    if !colours.is_empty() {
        out.push(format!("colours the original uses that the build doesn't: {}", colours.join(", ")));
    }
    if let (Some(a), Some(b)) = (design_values(original, "fonts").first(), design_values(built, "fonts").first())
        && !family(a).eq_ignore_ascii_case(&family(b))
    {
        out.push(format!("main font: {} in the original, {} in the build", family(a), family(b)));
    }
    for (key, what) in [("font_sizes", "text sizes"), ("spacing", "spacing")] {
        let ours = design_values(built, key);
        let missing: Vec<&str> = design_values(original, key).into_iter().take(8).filter(|v| !ours.contains(v)).collect();
        if !missing.is_empty() {
            out.push(format!("{what} the original uses that the build doesn't: {}", missing.join(", ")));
        }
    }
    let n = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    let layouts = |v: &Value| v.get("layouts").and_then(Value::as_array).cloned().unwrap_or_default();
    let built_layouts = layouts(built);
    for theirs in layouts(original) {
        let w = n(&theirs, "width");
        let Some(ours) = built_layouts.iter().find(|l| n(l, "width") == w) else { continue };
        let mut diffs = Vec::new();
        if n(ours, "page_w") > w + 1 && n(&theirs, "page_w") <= w + 1 {
            diffs.push(format!("the page scrolls sideways ({} px wide)", n(ours, "page_w")));
        }
        for (k, name) in [("h1", "top heading"), ("h2", "subheadings"), ("body", "body text")] {
            let size = |l: &Value| l.pointer(&format!("/type/{k}")).and_then(Value::as_u64).unwrap_or(0);
            let (a, b) = (size(&theirs), size(ours));
            if a > 0 && b > 0 && a.abs_diff(b) > 2 {
                diffs.push(format!("{name} {b} px, the original's {a} px"));
            }
        }
        diffs.extend(block_diffs(&theirs, ours).into_iter().take(4));
        if !diffs.is_empty() {
            out.push(format!("at {w} px: {}", diffs.join("; ")));
        }
    }
    out
}

/// The sections of one width whose padding, items per row or content column differ. Sections are
/// paired first by tag and heading in order (the n-th `section "Plans"` with the n-th), then,
/// because the builder writes its own words, the rest of each tag in page order (the original's
/// "Why people switch" with the build's "Why people like it"). Items per row is a grid's measure,
/// so it isn't compared for a `nav` or `header` (a row of links, checked by the pictures).
fn block_diffs(original: &Value, built: &Value) -> Vec<String> {
    let blocks = |v: &Value| v.get("blocks").and_then(Value::as_array).cloned().unwrap_or_default();
    let key = |b: &Value| {
        let heading = b.get("heading").and_then(Value::as_str).unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        (b.get("tag").and_then(Value::as_str).unwrap_or_default().to_string(), heading)
    };
    let nums = |b: &Value, k: &str| -> Vec<u64> { b.get(k).and_then(Value::as_array).into_iter().flatten().map(|x| x.as_u64().unwrap_or(0)).collect() };
    let n = |v: &Value, k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
    let column = |b: &Value, k: &str| b.get("column").map(|c| n(c, k));
    let (theirs, ours) = (blocks(original), blocks(built));
    // The build's block paired with each of the original's, if any.
    let mut partner: Vec<Option<usize>> = vec![None; theirs.len()];
    let mut taken = vec![false; ours.len()];
    let mut pair = |same: &dyn Fn(&Value, &Value) -> bool, partner: &mut Vec<Option<usize>>| {
        for (i, a) in theirs.iter().enumerate() {
            if partner[i].is_some() {
                continue;
            }
            if let Some(j) = ours.iter().enumerate().position(|(j, b)| !taken[j] && same(a, b)) {
                taken[j] = true;
                partner[i] = Some(j);
            }
        }
    };
    pair(&|a, b| key(a) == key(b), &mut partner);
    pair(&|a, b| key(a).0 == key(b).0, &mut partner);
    let mut out = Vec::new();
    for (i, a) in theirs.iter().enumerate() {
        let k = key(a);
        let Some(b) = partner[i].and_then(|j| ours.get(j)) else { continue };
        let mut what = Vec::new();
        let (pa, pb) = (nums(a, "pad"), nums(b, "pad"));
        if pa.len() == pb.len() && pa.iter().zip(&pb).any(|(x, y)| x.abs_diff(*y) > 4) {
            let show = |p: &[u64]| p.iter().map(u64::to_string).collect::<Vec<_>>().join(" ");
            what.push(format!("padding {} vs the original's {}", show(&pb), show(&pa)));
        }
        let grid = !matches!(k.0.as_str(), "nav" | "header");
        if grid && n(a, "per_row") != n(b, "per_row") {
            what.push(format!("{} per row vs {}", n(b, "per_row"), n(a, "per_row")));
        }
        if let (Some(x), Some(y)) = (column(a, "max_w"), column(b, "max_w"))
            && x.abs_diff(y) > 8
        {
            what.push(format!("content column {y} px vs {x} px"));
        } else if column(a, "max_w").is_some() && column(b, "max_w").is_none() {
            what.push(format!("no content column (the original's is {} px)", column(a, "max_w").unwrap_or(0)));
        }
        if !what.is_empty() {
            let name = if k.1.is_empty() {
                format!("the {}", k.0)
            } else {
                format!("the \"{}\" {}", a.get("heading").and_then(Value::as_str).unwrap_or_default(), k.0)
            };
            out.push(format!("{name}: {}", what.join(", ")));
        }
    }
    out
}

/// Pair each state the study pictured with the build's: the same kind of control with the same
/// label, ignoring case and spacing. The build's half is None when it has no such control or
/// couldn't picture it. Both are `states.json` documents.
pub fn pair_states<'a>(original: &'a Value, built: &'a Value) -> Vec<(&'a Value, Option<&'a Value>)> {
    let rows = |v: &'a Value| v.get("states").and_then(Value::as_array).into_iter().flatten().filter(|s| s.get("file").is_some());
    let key = |s: &Value| {
        let label = s.get("label").and_then(Value::as_str).unwrap_or_default().split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase();
        (s.get("kind").and_then(Value::as_str).unwrap_or_default().to_string(), label)
    };
    rows(original).map(|o| (o, rows(built).find(|b| key(b) == key(o)))).collect()
}

/// Read a JSON file, or None.
fn read_json(path: &Path) -> Option<Value> {
    std::fs::read(path).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

/// A picture of the build against the original's, in percent: (likeness, layout, detail), then
/// the two heights in px, and whether the frames were compared as they are (the noise mask that
/// would have left nothing to compare was dropped).
type Likeness = (f64, f64, f64, u64, u64, bool);

/// Compare the build's picture with the original's. None when either picture is missing or they
/// couldn't be compared.
fn picture_likeness(cx: &CheckCx, original: &Path, built: &Path) -> Result<Option<Likeness>, ToolError> {
    if !(original.is_file() && built.is_file()) {
        return Ok(None);
    }
    let mut params =
        json!({"before": {"path": original.display().to_string()}, "after": {"path": built.display().to_string()}, "common_area": true, "tolerance": 24});
    // The parts of the original that change by themselves (Study's noise-<width>.png) aren't
    // compared — unless that is (almost) everything: a game that redraws every frame would leave
    // nothing to differ and score 100% against any build. Those frames are compared as they are
    // (animation frames never line up; the note says so), never as an empty comparison (R2.11).
    let noise =
        original.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix("screenshot-")).map(|w| original.with_file_name(format!("noise-{w}")));
    let mut mask = noise.filter(|m| m.is_file());
    let mut as_is = false;
    if let Some(m) = &mask
        && mask_hides_everything(m, original).unwrap_or(false)
    {
        as_is = true;
        mask = None;
    }
    if let Some(mask) = mask {
        params["ignore"] = json!({"path": mask.display().to_string()});
    }
    let r = cx.call("web_pixels_compare", params)?;
    let v = r.to_json();
    let res = v.get("result").cloned().unwrap_or(Value::Null);
    let Some(layout) = res.get("layout_similarity").and_then(Value::as_f64) else { return Ok(None) };
    // Layout alone is generous to mostly empty pages; detail (SSIM) sees text and edges.
    let detail = res.get("perceptual_similarity").and_then(Value::as_f64).unwrap_or(layout);
    let h = |k: &str| res.pointer(&format!("/sizes/{k}/height")).and_then(Value::as_u64).unwrap_or(0);
    Ok(Some(((layout + detail) * 50.0, layout * 100.0, detail * 100.0, h("before"), h("after"), as_is)))
}

/// Whether the noise mask covers (almost) every tile of the original's picture: nothing would be
/// left to compare, so the mask must not be used (R2.11).
fn mask_hides_everything(mask: &Path, original: &Path) -> Result<bool, ToolError> {
    use craft_anything_engine::tools::web_common::{NoiseMask, png_dimensions};
    let img = std::fs::read(original).map_err(|e| ToolError::io(original.display(), e))?;
    let (w, h) = png_dimensions(&img)?;
    let bytes = std::fs::read(mask).map_err(|e| ToolError::io(mask.display(), e))?;
    let mask = NoiseMask::from_png(&bytes)?;
    let tile = u64::from(mask.tile.max(1));
    let tiles = (u64::from(w).div_ceil(tile) * u64::from(h).div_ceil(tile)).max(1);
    Ok(mask.count() as u64 * 10 >= tiles * 9)
}

/// Where a web check's originals come from (R2.11): the focused part's own capture alone when
/// the person asked for one part, else the whole capture with everything studied.
fn check_scope(ws: &Workspace) -> PathBuf {
    match &ws.manifest.focus {
        Some(_) => ws.capture_dir().join(FOCUS_DIR),
        None => ws.capture_dir(),
    }
}

/// Open-source libraries a game may ship and the rebuild may use as they are — engines like
/// PlayCanvas, codecs like draco, physics and loaders (the person, 2026-11-09: open-source
/// libraries are never rebuilt). Matched by name in the saved file's address: one of these in
/// app/ is not "the game running".
const OPEN_SOURCE_LIBRARIES: &[&str] = &["draco", "ammo", "physx", "bullet", "box2d", "rapier", "zstd", "basis", "meshopt", "ktx", "rhino3dm"];

/// The claims about the game's own saved files (R2.12): what the focused brief asked for is what
/// must show up in the build. The default is a true rebuild — the game's art and data reused
/// unchanged, and its compiled runtime kept out of app/ (the game is the builder's own
/// implementation); a request that asks for the game's own files to run as they are gets that
/// instead. Either way a build that reuses none of the saved files is a from-scratch lookalike,
/// however alike its pictures look; a saved set of only the files the build rewrites (scripts,
/// stylesheets, pages) makes no claim at all.
fn originals_claims(ws: &Workspace, run_as_is: bool) -> Vec<Claim> {
    let Some(manifest) = read_json(&ws.capture_dir().join(FOCUS_DIR).join(RESOURCES_DIR).join("resources.json")) else {
        return Vec::new();
    };
    // Scripts, stylesheets and pages are rewritten (adapted), not copied. A compiled runtime (a
    // wasm or engine build) is what a true rebuild reimplements instead of running; the rest —
    // pictures, sounds, fonts, data — is used as it is.
    let mut art = std::collections::BTreeSet::new();
    let mut runtime = std::collections::BTreeSet::new();
    for it in manifest.get("items").and_then(Value::as_array).into_iter().flatten() {
        let kind = it.get("type").and_then(Value::as_str).unwrap_or_default();
        if matches!(kind, "Script" | "Stylesheet" | "Document") {
            continue;
        }
        let (Some(bytes), Some(sha)) = (it.get("bytes").and_then(Value::as_u64), it.get("sha256").and_then(Value::as_str)) else { continue };
        let url = it.get("url").and_then(Value::as_str).unwrap_or_default().split(['?', '#']).next().unwrap_or_default().to_ascii_lowercase();
        let compiled = [".wasm", ".unityweb", ".pck", ".data"].iter().any(|e| url.ends_with(e));
        if compiled && !OPEN_SOURCE_LIBRARIES.iter().any(|l| url.contains(l)) {
            runtime.insert((bytes, sha.to_string()));
        } else {
            art.insert((bytes, sha.to_string()));
        }
    }
    let mut want = art.clone();
    if run_as_is {
        want.extend(runtime.iter().cloned());
    }
    if want.is_empty() && (run_as_is || runtime.is_empty()) {
        return Vec::new();
    }
    // Hash against every size a claim looks at, so the runtime check can't miss by size.
    let mut sizes: std::collections::BTreeSet<u64> = want.iter().map(|(b, _)| *b).collect();
    sizes.extend(runtime.iter().map(|(b, _)| *b));
    let hashes = app_file_hashes(&ws.app_dir(), &sizes);
    let mut out = Vec::new();
    if !want.is_empty() {
        let used = want.iter().filter(|(_, h)| hashes.contains(h)).count();
        out.push(Claim::new(
            "The build uses the game's own saved files",
            used > 0,
            format!(
                "none of the files saved into capture/{FOCUS_DIR}/{RESOURCES_DIR}/ appear in the build; copy the ones the game uses into app/ and use them as they are"
            ),
        ));
    }
    if !run_as_is && !runtime.is_empty() {
        out.push(Claim::new(
            "The game is the builder's own implementation",
            !runtime.iter().any(|(_, h)| hashes.contains(h)),
            "the game's own compiled runtime is copied into the build and runs as the game: the person asked for a rebuild in your own code — reimplement it (an open-source library it ships is fine as it is; only a request that asks for the game's own files to run as they are makes this right)",
        ));
    }
    out
}

/// The sha256 of each file in `app` whose size is in `sizes` (only those can be one of the saved
/// files), skipping dependency and git folders and following no symlink, bounded so a huge tree
/// can't stall the check.
fn app_file_hashes(app: &Path, sizes: &std::collections::BTreeSet<u64>) -> std::collections::BTreeSet<String> {
    const MAX_FILES: u64 = 50_000;
    let mut out = std::collections::BTreeSet::new();
    let mut stack = vec![app.to_path_buf()];
    let mut seen = 0u64;
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            if seen >= MAX_FILES {
                return out;
            }
            seen += 1;
            let path = e.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
            if meta.is_dir() {
                if !matches!(e.file_name().to_string_lossy().as_ref(), "node_modules" | ".git") {
                    stack.push(path);
                }
            } else if meta.is_file()
                && sizes.contains(&meta.len())
                && let Ok(bytes) = std::fs::read(&path)
            {
                out.insert(craft_anything_evidence::sha256_hex(&bytes));
            }
        }
    }
    out
}

/// The floor for a play state "looking alike": it is the same screen, not the finish bar (which
/// waits for measured true-rebuild attempts — R2.12's decision).
const PLAY_STATE_FLOOR: f64 = 50.0;

/// Claims from pairing the study's recorded play states with the build's replayed ones (R2.13):
/// every recorded screen must be reached in the build and look alike. `(label, likeness)` per
/// recorded state; `None` likeness = the build never showed it (or it couldn't be compared).
pub fn play_state_claims(states: &[(String, Option<f64>)]) -> Vec<Claim> {
    states
        .iter()
        .enumerate()
        .map(|(i, (label, likeness))| {
            let title = format!("Play state {} ({label}) is reached and looks alike", i + 1);
            match likeness {
                Some(s) if *s >= PLAY_STATE_FLOOR => Claim::new(title, true, ""),
                Some(s) => Claim::new(title, false, format!("only {s:.0}% alike")),
                None => Claim::new(title, false, "the build never showed this state"),
            }
        })
        .collect()
}

/// Replay the study's recorded play-state actions against the build, photograph what it reaches,
/// and pair each screen with the recorded one (R2.13). A replay that can't run is an unknown
/// (the build is not blamed for a check that couldn't look).
fn play_states_note(cx: &CheckCx, ws: &Workspace, out: &Path, unknowns: &mut Vec<String>, claims: &mut Vec<Claim>) -> Result<Option<String>, ToolError> {
    let dir = ws.capture_dir().join(FOCUS_DIR).join(PLAY_STATES_DIR);
    let Some(orig) = read_json(&dir.join("states.json")) else { return Ok(None) };
    let all = orig.get("states").and_then(Value::as_array).into_iter().flatten().collect::<Vec<_>>();
    let rows: Vec<&Value> = all.iter().copied().filter(|r| r.get("file").and_then(Value::as_str).is_some()).collect();
    if rows.is_empty() {
        return Ok(None);
    }
    let actions: Vec<Value> = all
        .iter()
        .filter_map(|r| {
            let a = r.get("action")?;
            (a.get("kind").and_then(Value::as_str) == Some("click")).then(|| a.clone())
        })
        .collect();
    let width = orig.get("width").and_then(Value::as_u64).unwrap_or(1440).clamp(320, 3840) as u32;
    let built = out.join(PLAY_STATES_DIR);
    let replayed = cx.call(
        "web_game_states_capture",
        json!({"output_directory": built.display().to_string(), "width": width, "max_states": rows.len().min(8), "replay": actions}),
    );
    let ours = read_json(&built.join("states.json"));
    let built_rows: Vec<&Value> = ours
        .as_ref()
        .and_then(|o| o.get("states"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|r| r.get("file").and_then(Value::as_str).is_some())
        .collect();
    if built_rows.is_empty() {
        let why = replayed.err().map(|e| e.message).unwrap_or_else(|| "no state was reached".into());
        unknowns.push(format!("the play states couldn't be replayed against the build ({why})"));
        return Ok(Some("play states: not compared this round".to_string()));
    }
    let mut pairs = Vec::new();
    let mut parts = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        let label =
            row.get("action").and_then(|a| a.get("target")).and_then(Value::as_str).unwrap_or(if i == 0 { "the start view" } else { "a tap" }).to_string();
        let likeness = match (row.get("file").and_then(Value::as_str), built_rows.get(i).and_then(|b| b.get("file")).and_then(Value::as_str)) {
            (Some(a_file), Some(b_file)) => match picture_likeness(cx, &dir.join(a_file), &built.join(b_file))? {
                Some((s, ..)) => Some(s),
                None => {
                    unknowns.push(format!("play state {} ({label}) couldn't be compared", i + 1));
                    None
                }
            },
            _ => None,
        };
        if let Some(s) = likeness {
            parts.push(format!("{}: {:.0}%", i + 1, s));
        }
        pairs.push((label, likeness));
    }
    claims.extend(play_state_claims(&pairs));
    let reached = pairs.iter().filter(|(_, l)| l.is_some()).count();
    Ok(Some(format!("play states: {reached} of {} recorded screens reached and compared ({}).", pairs.len(), parts.join(", "))))
}

/// Asset-like files that must come from the game or carry a name in `stand-ins.json`: the
/// pictures, sounds, fonts and data a stand-in could hide behind. Code and notes are the
/// builder's own and don't belong here.
const ASSET_KINDS: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "gif", "svg", "bmp", "ico", "mp3", "ogg", "wav", "m4a", "ttf", "otf", "woff", "woff2", "glb", "bin", "ktx2", "dds", "tga",
    "mp4", "webm",
];

/// Every asset in the build is the game's own or named in `app/stand-ins.json` (R2.13): a
/// stand-in never hides (the Fashion Battle wardrobe shipped blank behind passing claims). The
/// note counts what the person is looking at: originals as-is, fetched originals and stand-ins.
/// Only meaningful where saved originals exist to match against.
fn assets_provenance_claim(ws: &Workspace) -> Option<(Claim, String)> {
    let saved = read_json(&ws.capture_dir().join(FOCUS_DIR).join(RESOURCES_DIR).join("resources.json"))?;
    let saved: std::collections::BTreeSet<String> = saved
        .get("items")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|i| i.get("sha256").and_then(Value::as_str).map(str::to_string))
        .collect();
    let stand_ins = read_json(&ws.app_dir().join("stand-ins.json"));
    let named: std::collections::BTreeMap<String, String> = stand_ins
        .as_ref()
        .and_then(|v| v.get("files"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|f| {
            let file = f.get("file").and_then(Value::as_str)?.trim_start_matches("./").to_string();
            let kind = f.get("kind").and_then(Value::as_str).unwrap_or("stand-in").to_string();
            Some((file, kind))
        })
        .collect();
    // The assets under app/ (bounded walk; code and notes are not assets).
    let mut stack = vec![ws.app_dir()];
    let mut seen = 0u64;
    let (mut originals, mut fetched, mut standins, mut unlisted) = (0usize, 0usize, 0usize, Vec::new());
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { continue };
        for e in entries.flatten() {
            if seen >= 50_000 {
                break;
            }
            seen += 1;
            let path = e.path();
            let Ok(meta) = std::fs::symlink_metadata(&path) else { continue };
            if meta.is_dir() {
                if !matches!(e.file_name().to_string_lossy().as_ref(), "node_modules" | ".git") {
                    stack.push(path);
                }
                continue;
            }
            if !meta.is_file() {
                continue;
            }
            let ext = path.extension().and_then(|x| x.to_str()).unwrap_or_default().to_ascii_lowercase();
            if !ASSET_KINDS.contains(&ext.as_str()) {
                continue;
            }
            let rel = path.strip_prefix(ws.app_dir()).unwrap_or(&path).to_string_lossy().replace("\\", "/");
            let hash = std::fs::read(&path).ok().map(|b| craft_anything_evidence::sha256_hex(&b));
            match hash {
                Some(h) if saved.contains(&h) => originals += 1,
                _ => match named.get(&rel) {
                    Some(k) if k == "original" => fetched += 1,
                    Some(_) => standins += 1,
                    None => unlisted.push(rel),
                },
            }
        }
    }
    let note = format!(
        "assets: {originals} the game's own as-is, {fetched} fetched from the game's server and {standins} stand-ins, each named in app/stand-ins.json."
    );
    let claim = Claim::new(
        "Every asset in the build is the game's own or a labelled stand-in",
        unlisted.is_empty(),
        format!(
            "not from the game and not named in app/stand-ins.json: {}{}",
            unlisted.iter().take(5).cloned().collect::<Vec<_>>().join(", "),
            if unlisted.len() > 5 { format!(" (and {} more)", unlisted.len() - 5) } else { String::new() }
        ),
    );
    Some((claim, note))
}

/// "the game area" → "The game area", for a claim's title.
fn capitalized(what: &str) -> String {
    let mut c = what.chars();
    match c.next() {
        Some(f) => format!("{}{}", f.to_uppercase(), c.as_str()),
        None => "The part".to_string(),
    }
}

/// Open the build's menus, pop-ups and tabs the way the study opened the original's in `dir`
/// (`states` or `states-<width>`), at the same width, and compare each pair of pictures. None
/// when the study pictured none there.
fn states_note(cx: &CheckCx, base: &str, out: &Path, dir: &str, unknowns: &mut Vec<String>, claims: &mut Vec<Claim>) -> Result<Option<String>, ToolError> {
    let theirs = cx.ws.capture_dir().join(dir);
    let Some(original) = read_json(&theirs.join("states.json")) else { return Ok(None) };
    if pair_states(&original, &Value::Null).is_empty() {
        return Ok(None);
    }
    let ours = out.join(dir);
    cx.call("web_site_open", json!({"url": base, "settle_ms": 1200}))?;
    // More than the study opened, so the same controls are found even when the build orders them differently.
    let width = original.get("width").and_then(Value::as_u64).unwrap_or(1440);
    if let Err(e) = cx.call("web_page_states_capture", json!({"output_directory": ours.display().to_string(), "max_states": 8, "width": width})) {
        unknowns.push(format!("menus and pop-ups at {width} px couldn't be opened"));
        return Ok(Some(format!("menus and pop-ups couldn't be opened: {}", e.message)));
    }
    let built = read_json(&ours.join("states.json")).unwrap_or(Value::Null);
    let mut parts = Vec::new();
    for (o, b) in pair_states(&original, &built) {
        let s = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).unwrap_or_default().to_string();
        let name = match s(o, "label") {
            l if l.is_empty() => format!("a {}", s(o, "kind")),
            l => format!("the \"{l}\" {}", s(o, "kind")),
        };
        let likeness = match b {
            Some(b) => picture_likeness(cx, &theirs.join(s(o, "file")), &ours.join(s(b, "file")))?,
            None => None,
        };
        claims.push(Claim::new(format!("{name} opens in the build at {width} px"), b.is_some(), "no such control opens"));
        parts.push(match (b, likeness) {
            (_, Some((score, ..))) => format!("{name} {score:.0}% alike"),
            (Some(_), None) => {
                unknowns.push(format!("{name} at {width} px couldn't be compared"));
                format!("{name} couldn't be compared")
            }
            (None, _) => format!("{name} doesn't open in the build"),
        });
    }
    Ok(Some(format!("opened menus and pop-ups at {width} px: {}", parts.join(", "))))
}

/// Serve the built site on loopback, capture it like the original, and compare screenshots at
/// each width. The likeness is the mean of layout (tile colours) and detail (SSIM of brightness).
/// The note adds the text, the page structure and the opened menus and pop-ups; they guide the
/// builder but don't change the score.
pub struct WebVisualCheck;

/// What the check says when there is no site to look at: the places `site_dir` accepts, in the
/// place the app really is ("app/" or "the project folder", #494).
/// When `app/` holds no files at all, the note says so first: the builder may claim it built the
/// site while nothing was written (#736), and a site can't be found in a folder with no project.
fn missing_site_note(place: &str, empty: bool) -> String {
    let start = if empty { format!("{place} has no files yet, so the builder hasn't written the project. ") } else { String::new() };
    format!(
        "{start}No built site found in {place} yet. Expected an index.html in its dist/, build/ or out/ folder, or an index.html straight in {place} when there is no package.json."
    )
}

impl Check for WebVisualCheck {
    fn id(&self) -> &'static str {
        CheckResult::WEB_VISUAL
    }

    fn run(&self, cx: &mut CheckCx) -> Result<CheckResult, ToolError> {
        let (ws, round) = (cx.ws, cx.round);
        let widths = cx.build_settings().widths();
        let Some(site) = site_dir(&ws.app_dir()) else {
            return Ok(CheckResult::new(self.id(), round, false, missing_site_note(ws.app_place(), app_is_empty(ws))));
        };
        let base = cx.serve_preview(&site)?;
        let out = ws.checks_dir().join(format!("round-{round}"));
        if out.exists() {
            let _ = std::fs::remove_dir_all(&out);
        }
        std::fs::create_dir_all(ws.checks_dir()).map_err(|e| ToolError::io(ws.checks_dir().display(), e))?;
        // A focused rebuild (R2.11) compares only its part — the part's own capture against the
        // build — and everything the person scoped out (the page around it, its other pages and
        // its menus) stays un-compared. Without a focus, everything is compared exactly as ever.
        let focus = ws.manifest.focus.clone();
        let originals = check_scope(ws);
        let mut notes = Vec::new();
        if let Some(f) = &focus {
            notes.push(format!("Only the {} is compared — the person asked for just that part, and the page around it is out of scope.", f.what));
        }
        // The start page (or the focused part), then every other page studied: the same path on
        // the build.
        let mut pages = vec![(focus.as_ref().map_or_else(|| "start page".to_string(), |f| f.what.clone()), String::new(), base.clone())];
        if focus.is_none() {
            for r in routes(ws) {
                let url = format!("{}{}", base.trim_end_matches('/'), r.path);
                pages.push((r.path.clone(), format!("routes/{}", r.slug), url));
            }
        }
        let mut scores = Vec::new();
        let mut likeness = Vec::new();
        let mut artifacts = Vec::new();
        let mut unknowns = Vec::new();
        let mut claims = Vec::new();
        // R2.12: the claims hold the build to what the focused brief asked for — the game's own
        // files reused (never a from-scratch lookalike), and a true rebuild with the original's
        // compiled runtime kept out unless the request asked for it to run as it is.
        claims.extend(originals_claims(ws, run_as_is_asked(&ws.manifest.request)));
        // R2.13: every asset in the build is the game's own or named in stand-ins.json — a
        // stand-in never hides behind a passing check (the Fashion Battle wardrobe).
        if let Some((claim, note)) = assets_provenance_claim(ws) {
            claims.push(claim);
            notes.push(note);
        }
        for (name, dir, url) in &pages {
            let (original, built) = if dir.is_empty() { (originals.clone(), out.clone()) } else { (originals.join(dir), out.join(dir)) };
            // web_site_capture creates the page's own folder (it must not exist yet) inside one
            // that does.
            if let Some(parent) = built.parent() {
                std::fs::create_dir_all(parent).map_err(|e| ToolError::io(parent.display(), e))?;
            }
            cx.call("web_site_open", json!({"url": url, "settle_ms": 1200}))?;
            cx.call("web_site_capture", json!({"output_directory": built.display().to_string(), "include_html": false, "widths": widths}))?;
            let mut per_width = Vec::new();
            for &w in &widths {
                let (a, b) = (original.join(format!("screenshot-{w}.png")), built.join(format!("screenshot-{w}.png")));
                match picture_likeness(cx, &a, &b)? {
                    Some((s, l, d, hb, ha, as_is)) => {
                        per_width.push((w, s, l, d, hb, ha, as_is));
                        artifacts.extend(b.strip_prefix(&ws.root).ok().map(Path::to_path_buf));
                    }
                    // A width the original was never pictured at is outside what was studied;
                    // one it was, that the build's picture can't be compared with, is unknown.
                    None if a.is_file() => unknowns.push(format!("{name} at {w} px couldn't be compared")),
                    None => {}
                }
            }
            if per_width.is_empty() {
                notes.push(format!("{name}: couldn't be compared"));
                continue;
            }
            let score = per_width.iter().map(|p| p.1).sum::<f64>() / per_width.len() as f64;
            scores.push(score);
            likeness.push(PageLikeness { name: name.clone(), dir: dir.clone(), similarity: score });
            let widths = per_width
                .iter()
                .map(|(w, s, l, d, hb, ha, as_is)| {
                    let moved = if *as_is { ", compared frame to frame: the picture moves on its own" } else { "" };
                    format!("{w} px: {s:.0}% alike (layout {l:.0}%, detail {d:.0}%; page height {hb} vs {ha} px{moved})")
                })
                .collect::<Vec<_>>()
                .join("; ");
            // The page's words, parts and measured design too: a build can look alike and still
            // miss words, or get the padding, colours and fonts wrong at some width.
            let page = if dir.is_empty() { focus.as_ref().map_or_else(|| "The start page".to_string(), |f| capitalized(&f.what)) } else { name.clone() };
            let text = match (read_json(&original.join("capture.json")), read_json(&built.join("capture.json"))) {
                (Some(o), Some(b)) => {
                    claims.extend(page_claims(&page, &o, &b));
                    [text_likeness(&o, &b).map(|t| t.note()), structure_likeness(&o, &b).map(|s| s.note())]
                        .into_iter()
                        .flatten()
                        .chain(design_notes(&o, &b))
                        .chain(health_notes(&o, &b))
                        .map(|n| format!("; {n}"))
                        .collect::<String>()
                }
                _ => String::new(),
            };
            notes.push(if pages.len() == 1 { format!("{widths}{text}") } else { format!("{name}: {score:.0}% ({widths}{text})") });
        }
        if scores.is_empty() {
            return Ok(CheckResult::new(self.id(), round, false, "The build couldn't be compared with the original."));
        }
        if focus.is_none() {
            for dir in state_dirs(&ws.capture_dir()) {
                notes.extend(states_note(cx, &base, &out, &dir, &mut unknowns, &mut claims)?);
            }
        }
        // R2.13: the recorded play states are the game, not just the first paint: replay the
        // study's clicks against the build and compare each screen it reached.
        if let Some(note) = play_states_note(cx, ws, &out, &mut unknowns, &mut claims)? {
            notes.push(note);
        }
        let failed: Vec<String> = claims.iter().filter(|c| !c.passed).map(|c| format!("{} ({})", c.title, c.detail)).collect();
        if !failed.is_empty() {
            notes.push(format!("{} of {} claims don't hold yet: {}", failed.len(), claims.len(), failed.join("; ")));
        }
        if !unknowns.is_empty() {
            notes.push(format!("not compared, so this can't pass yet: {}", unknowns.join(", ")));
        }
        let avg = scores.iter().sum::<f64>() / scores.len() as f64;
        // One page needs no list; with several, each one's likeness is kept for the project page.
        let pages = if likeness.len() > 1 { likeness } else { Vec::new() };
        Ok(CheckResult {
            similarity: Some(avg),
            artifacts,
            pages,
            unknowns,
            claims,
            focus: focus.map(|f| f.what),
            ..CheckResult::new(self.id(), round, true, notes.join(" | "))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #736: an empty app/ is said to be empty, so a builder that claims a build can't hide it; a
    /// folder with files, or a project folder (which always holds the workspace's own), isn't.
    #[test]
    fn an_empty_app_folder_is_named_as_empty() {
        let d = tempfile::tempdir().unwrap();
        let ws = Workspace::create(d.path(), "Site", "Clone it", TargetRef::None).unwrap();
        std::fs::create_dir_all(ws.app_dir()).unwrap();
        assert!(folder_is_empty(&ws.app_dir()), "a new app/ has nothing in it");
        assert!(missing_site_note("app/", true).starts_with("app/ has no files yet"));
        std::fs::write(ws.app_dir().join("package.json"), "{}").unwrap();
        assert!(!folder_is_empty(&ws.app_dir()));
        assert!(!missing_site_note("app/", false).contains("has no files"));
        assert!(folder_is_empty(&d.path().join("never-made")), "a missing folder is empty");
    }

    /// #494: the note names every place `site_dir` accepts, and the place the app really is.
    #[test]
    fn the_missing_site_note_names_what_site_dir_accepts() {
        let note = missing_site_note("app/", false);
        for named in ["dist/", "build/", "out/", "index.html straight in app/", "no package.json"] {
            assert!(note.contains(named), "{named} in {note}");
        }
        assert!(missing_site_note("the project folder", false).contains("index.html straight in the project folder"));
        // What it names is what is accepted, and a manifest turns the plain page down.
        for folder in ["dist", "build", "out"] {
            let d = tempfile::tempdir().unwrap();
            std::fs::create_dir_all(d.path().join(folder)).unwrap();
            std::fs::write(d.path().join(folder).join("index.html"), "<html></html>").unwrap();
            assert_eq!(site_dir(d.path()), Some(d.path().join(folder)), "{folder}");
        }
        let d = tempfile::tempdir().unwrap();
        assert_eq!(site_dir(d.path()), None, "nothing built");
        std::fs::write(d.path().join("index.html"), "<html></html>").unwrap();
        assert_eq!(site_dir(d.path()), Some(d.path().to_path_buf()), "a plain page");
        std::fs::write(d.path().join("package.json"), "{}").unwrap();
        assert_eq!(site_dir(d.path()), None, "a page beside a manifest is not a built site");
    }

    /// R2.11: the focus finds its embedded view by element id, else the biggest one the capture
    /// found; a capture with no embeds leaves nothing to pick.
    #[test]
    fn the_focus_finds_its_embedded_view() {
        let capture = json!({"embeds": [
            {"tag": "canvas", "boxes": [{"width": 1440, "x": 0, "y": 0, "w": 100, "h": 100}]},
            {"tag": "iframe", "id": "game-iframe", "src": "https://g.test/play", "boxes": [{"width": 1440, "x": 0, "y": 0, "w": 800, "h": 450}]},
            {"tag": "canvas", "boxes": [{"width": 1440, "x": 0, "y": 0, "w": 200, "h": 100}]}
        ]});
        let biggest = Focus { what: "the game area".into(), element_id: None };
        assert_eq!(pick_focus_embed(&capture, &biggest).unwrap()["id"], "game-iframe", "the biggest view");
        let by_id = Focus { what: "the game area".into(), element_id: Some("game-iframe".into()) };
        assert_eq!(pick_focus_embed(&capture, &by_id).unwrap()["id"], "game-iframe");
        let missing = Focus { what: "the game area".into(), element_id: Some("nope".into()) };
        assert_eq!(pick_focus_embed(&capture, &missing).unwrap()["tag"], "iframe", "no such id: the biggest again");
        assert!(pick_focus_embed(&json!({}), &biggest).is_none(), "no embeds: nothing to pick");
    }

    /// R2.11: a project's focus decides where the originals come from and what the page label
    /// says; without one nothing changes.
    #[test]
    fn the_focus_decides_what_gets_compared() {
        let d = tempfile::tempdir().unwrap();
        let mut ws = Workspace::create(d.path(), "Site", "Clone the game, only the game area", TargetRef::Website { url: "https://x/".into() }).unwrap();
        assert_eq!(check_scope(&ws), ws.capture_dir(), "no focus: the whole capture");
        assert_eq!(capitalized("the game area"), "The game area");
        ws.manifest.focus = Some(Focus { what: "the game area".into(), element_id: None });
        assert_eq!(check_scope(&ws), ws.capture_dir().join(FOCUS_DIR), "a focus: its part's own capture");
    }

    /// R2.11: a noise mask over (almost) the whole picture must not be used — it would leave
    /// nothing to differ and score 100% against any build. Most of a picture masked still
    /// compares what is left.
    #[test]
    fn a_mask_over_everything_is_dropped_not_trusted() {
        use craft_anything_engine::tools::web_common::{Image, NoiseMask, encode_png};
        let d = tempfile::tempdir().unwrap();
        let png = encode_png(&Image { width: 64, height: 64, rgba: vec![0; 64 * 64 * 4] }).unwrap();
        let original = d.path().join("screenshot-64.png");
        std::fs::write(&original, &png).unwrap();
        let full = d.path().join("noise-full.png");
        std::fs::write(&full, NoiseMask { tile: 16, cols: 4, rows: 4, bits: vec![true; 16] }.to_png().unwrap()).unwrap();
        assert!(mask_hides_everything(&full, &original).unwrap(), "everything is masked");
        let half = d.path().join("noise-half.png");
        std::fs::write(&half, NoiseMask { tile: 16, cols: 4, rows: 4, bits: (0..16).map(|i| i < 6).collect() }.to_png().unwrap()).unwrap();
        assert!(!mask_hides_everything(&half, &original).unwrap(), "most of the picture still compares");
    }

    /// R2.12: with the focused part's own files saved, the brief asks for a true rebuild by
    /// default — the builder's own web game, written from the original's code, with its art and
    /// data as they are, never its compiled runtime as the game; without them nothing says so,
    /// and the base focus brief stays exactly as it was.
    #[test]
    fn the_focus_brief_offers_the_saved_resources() {
        let d = tempfile::tempdir().unwrap();
        let mut ws = Workspace::create(d.path(), "Game", "Clone the game, only the game area", TargetRef::Website { url: "https://x/".into() }).unwrap();
        ws.manifest.focus = Some(Focus { what: "the game area".into(), element_id: None });
        let focus = Focus { what: "the game area".into(), element_id: None };
        assert!(!focus_brief(&focus).contains("resources"), "the base focus brief is unchanged");
        assert!(focus_resources_brief(&focus, &ws).is_none(), "nothing saved yet: nothing said");
        let res = ws.capture_dir().join(FOCUS_DIR).join(RESOURCES_DIR);
        std::fs::create_dir_all(&res).unwrap();
        std::fs::write(res.join("resources.json"), b"{}").unwrap();
        let text = focus_resources_brief(&focus, &ws).unwrap();
        for said in [
            "capture/focus/resources/",
            "true rebuild of this game",
            "your own code that plays like it",
            "web game in your own code",
            "use them as they are",
            "never redraw, regenerate or substitute",
            "game's own code is what you write the game from",
            "adapt it into app/ keeping its meaning",
            "the same formulas, constants, timings and states",
            "app/MECHANICS.md",
            "Never run the game's own compiled runtime",
            "Open-source libraries it ships",
            "open-source libraries are never rebuilt",
            "formats_wasm_module_show",
            "js_sources_restore",
        ] {
            assert!(text.contains(said), "{said} missing from {text}");
        }
        assert!(!text.contains("run those files as they are in app/"), "the runtime never runs as the game by default");
    }

    /// R2.12: only a request that asks for the game's own files to run as they are gets that
    /// brief (the words are read in one place); any other request gets the true rebuild.
    #[test]
    fn a_request_to_run_the_original_as_it_is_gets_that_instead() {
        assert!(run_as_is_asked("复刻它，原样运行原版"));
        assert!(run_as_is_asked("Please run the original files as they are"));
        assert!(!run_as_is_asked("复刻这个游戏，只关注游戏区域"));
        let d = tempfile::tempdir().unwrap();
        let ws = Workspace::create(d.path(), "Game", "复刻游戏，原样运行原版", TargetRef::Website { url: "https://x/".into() }).unwrap();
        let res = ws.capture_dir().join(FOCUS_DIR).join(RESOURCES_DIR);
        std::fs::create_dir_all(&res).unwrap();
        std::fs::write(res.join("resources.json"), b"{}").unwrap();
        let focus = Focus { what: "the game area".into(), element_id: None };
        let text = focus_resources_brief(&focus, &ws).unwrap();
        assert!(text.contains("run those files as they are in app/"), "{text}");
        assert!(!text.contains("MECHANICS.md") && !text.contains("Never run the original's compiled runtime"), "{text}");
    }

    /// R2.12: the claims hold the build to what the brief asked for. A true rebuild reuses the
    /// game's art and data and keeps its own compiled runtime out (the game is the builder's own
    /// implementation), while an open-source library it ships is fine as it is; a build that runs
    /// the game's runtime fails both. With a request for
    /// the files to run as they are, the runtime in the build is what passes. The files the build
    /// adapts (scripts, stylesheets, pages) are not what the claims count, and a saved set of
    /// only those makes no claim at all.
    #[test]
    fn the_claims_hold_the_build_to_what_the_brief_asked() {
        let d = tempfile::tempdir().unwrap();
        let ws = Workspace::create(d.path(), "Game", "Clone the game, only the game area", TargetRef::Website { url: "https://x/".into() }).unwrap();
        let res = ws.capture_dir().join(FOCUS_DIR).join(RESOURCES_DIR);
        std::fs::create_dir_all(&res).unwrap();
        let sha = |bytes: &[u8]| craft_anything_evidence::sha256_hex(bytes);
        std::fs::write(
            res.join("resources.json"),
            format!(
                r#"{{"items":[
                    {{"type":"Image","bytes":9,"sha256":"{}","url":"https://x/art.png"}},
                    {{"type":"Fetch","bytes":7,"sha256":"{}","url":"https://x/Build/Build.wasm.unityweb"}},
                    {{"type":"Fetch","bytes":6,"sha256":"{}","url":"https://x/draco.wasm.wasm"}},
                    {{"type":"Script","bytes":4,"sha256":"{}","url":"https://x/game.js"}}
                ]}}"#,
                sha(b"png-bytes"),
                sha(b"wasm-here"),
                sha(b"draco!"),
                sha(b"code")
            ),
        )
        .unwrap();
        fn by_title<'a>(claims: &'a [Claim], title: &str) -> &'a Claim {
            match claims.iter().find(|c| c.title == title) {
                Some(c) => c,
                None => panic!("{title} missing from {claims:?}"),
            }
        }
        // True rebuild, nothing copied: the from-scratch lookalike fails the reuse claim while
        // the runtime claim holds.
        let claims = originals_claims(&ws, false);
        assert!(!by_title(&claims, "The build uses the game's own saved files").passed);
        assert!(by_title(&claims, "The game is the builder's own implementation").passed);
        // The game's art copied in: both hold.
        std::fs::create_dir_all(ws.app_dir().join("assets")).unwrap();
        std::fs::write(ws.app_dir().join("assets/art.png"), b"png-bytes").unwrap();
        assert!(originals_claims(&ws, false).iter().all(|c| c.passed), "art reused, own implementation");
        // An open-source library it ships (a draco decoder) is not "the game running": copied in
        // beside the art, both claims still hold.
        std::fs::write(ws.app_dir().join("draco.wasm.wasm"), b"draco!").unwrap();
        assert!(originals_claims(&ws, false).iter().all(|c| c.passed), "an open-source library is fine as it is");
        // The game's own runtime copied in as the game instead: the runtime claim fails, and the
        // reuse claim fails too — the runtime is not the art a true rebuild must use.
        std::fs::remove_file(ws.app_dir().join("assets/art.png")).unwrap();
        std::fs::remove_file(ws.app_dir().join("draco.wasm.wasm")).unwrap();
        std::fs::write(ws.app_dir().join("Build.wasm.unityweb"), b"wasm-here").unwrap();
        let claims = originals_claims(&ws, false);
        assert!(!by_title(&claims, "The game is the builder's own implementation").passed);
        assert!(!by_title(&claims, "The build uses the game's own saved files").passed, "the runtime doesn't count as art");
        // A request for the game's own files to run as they are: the runtime is the build, one
        // claim, and it holds.
        let claims = originals_claims(&ws, true);
        assert_eq!(claims.len(), 1, "{claims:?}");
        assert!(by_title(&claims, "The build uses the game's own saved files").passed);
        // Scripts are adapted, not copied: a saved set of only scripts has nothing to require.
        std::fs::write(
            res.join("resources.json"),
            format!(r#"{{"items":[{{"type":"Script","bytes":4,"sha256":"{}","url":"https://x/game.js"}}]}}"#, sha(b"code")),
        )
        .unwrap();
        assert!(originals_claims(&ws, false).is_empty(), "only scripts: nothing to claim about");
    }

    /// R2.13: the play-states brief is said only when the study photographed any, and each
    /// recorded screen becomes a claim that must be reached and look alike (the floor is the
    /// same screen, not the finish bar).
    #[test]
    fn play_states_are_a_brief_and_a_claim_each() {
        let d = tempfile::tempdir().unwrap();
        let mut ws = Workspace::create(d.path(), "Game", "Clone the game, only the game area", TargetRef::Website { url: "https://x/".into() }).unwrap();
        ws.manifest.focus = Some(Focus { what: "the game area".into(), element_id: None });
        let focus = Focus { what: "the game area".into(), element_id: None };
        assert!(play_states_brief(&focus, &ws).is_none(), "nothing photographed: nothing said");
        let dir = ws.capture_dir().join(FOCUS_DIR).join(PLAY_STATES_DIR);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("states.json"), b"{}").unwrap();
        let text = play_states_brief(&focus, &ws).unwrap();
        for said in ["capture/focus/play-states/", "state-<n>.png", "replay the same actions against your build"] {
            assert!(text.contains(said), "{said} missing from {text}");
        }
        let claims = play_state_claims(&[("the start view".into(), Some(92.0)), ("a tap".into(), Some(30.0)), ("another tap".into(), None)]);
        assert!(claims[0].passed && claims[0].detail.is_empty(), "{:?}", claims[0]);
        assert!(!claims[1].passed && claims[1].detail.contains("30% alike"), "{:?}", claims[1]);
        assert!(!claims[2].passed && claims[2].detail.contains("never showed"), "{:?}", claims[2]);
    }

    /// R2.13: an asset that is neither the game's own nor named in app/stand-ins.json fails the
    /// claim (a stand-in never hides behind a passing check); named stand-ins and fetched
    /// originals pass and are counted, and the builder's own code is not an asset at all.
    #[test]
    fn a_stand_in_never_hides() {
        let d = tempfile::tempdir().unwrap();
        let ws = Workspace::create(d.path(), "Game", "Clone the game", TargetRef::Website { url: "https://x/".into() }).unwrap();
        let res = ws.capture_dir().join(FOCUS_DIR).join(RESOURCES_DIR);
        std::fs::create_dir_all(&res).unwrap();
        let sha = craft_anything_evidence::sha256_hex(b"real-bytes");
        std::fs::write(res.join("resources.json"), format!(r#"{{"items":[{{"sha256":"{sha}"}}]}}"#)).unwrap();
        let assets = ws.app_dir().join("assets");
        std::fs::create_dir_all(&assets).unwrap();
        std::fs::write(assets.join("real.png"), b"real-bytes").unwrap();
        std::fs::write(assets.join("made.png"), b"drawn-by-hand").unwrap();
        std::fs::write(assets.join("fake.png"), b"stand-in").unwrap();
        std::fs::write(ws.app_dir().join("game.js"), b"// the builder's own code").unwrap();
        let (claim, _note) = assets_provenance_claim(&ws).unwrap();
        assert!(!claim.passed && claim.detail.contains("assets/made.png"), "{:?}", claim);
        assert!(claim.detail.contains("stand-ins.json"), "{:?}", claim);
        // Listing it passes the claim; the note counts what is what.
        std::fs::write(
            ws.app_dir().join("stand-ins.json"),
            r#"{"files":[{"file":"assets/fake.png","kind":"stand-in"},{"file":"assets/made.png","kind":"original","stands_for":"Dress.png"}]}"#,
        )
        .unwrap();
        let (claim, note) = assets_provenance_claim(&ws).unwrap();
        assert!(claim.passed && claim.detail.is_empty(), "{:?}", claim);
        assert!(note.contains("1 the game's own as-is") && note.contains("1 fetched") && note.contains("1 stand-ins"), "{note}");
    }

    /// R2.11: the refine note names the part's own capture for a focused rebuild (one pair to
    /// look at, no other pages), and is byte-for-byte the same as ever without a focus.
    #[test]
    fn the_refine_note_matches_what_was_compared() {
        let focused = CheckResult { focus: Some("the game area".into()), similarity: Some(50.0), ..CheckResult::new(CheckResult::WEB_VISUAL, 1, true, "note") };
        let r = WebsiteKind.refine(&focused);
        assert!(r.contains("the game area's own capture is in capture/focus/") && !r.contains("routes/"), "{r}");
        let whole = CheckResult { similarity: Some(50.0), ..CheckResult::new(CheckResult::WEB_VISUAL, 1, true, "note") };
        let r = WebsiteKind.refine(&whole);
        assert!(r.starts_with("Check 1: compared with the original, your build looks 50% alike (note)."), "{r}");
        assert!(r.contains("screenshot-<width>.png for each width, and routes/<page>/ for other pages"), "{r}");
        assert!(r.contains("the original's are in capture/. Open each pair with the Read tool"), "{r}");
    }
}
