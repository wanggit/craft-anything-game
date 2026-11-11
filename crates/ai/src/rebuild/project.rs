//! Starting a project from a request: the target, the folder, the stack the person asked for, the
//! asset budget from settings and whether assets may be generated. Shared by the CLI's `rebuild` and the `rebuild_start` tool.

use super::*;
use craft_anything_engine::Settings;
use craft_anything_engine::workspace::{ExistingFiles, Focus, suggest_name};

/// Where a new project goes.
#[derive(Debug, Clone, Copy, Default)]
pub enum Place<'a> {
    /// A new folder in the projects folder from settings.
    #[default]
    Projects,
    /// A new folder inside this one.
    In(&'a Path),
    /// This folder itself ([`Workspace::create_at`]), with what its files are if it has any.
    At(&'a Path, Option<ExistingFiles>),
}

/// A stack picked by name for `--build-with` / `build_with` (an engine id or a stack's full name).
pub fn build_with(name: &str) -> Result<&'static stacks::Stack, ToolError> {
    stacks::resolve(name).ok_or_else(|| {
        let known = stacks::catalogue().iter().map(|s| s.engine.clone().unwrap_or_else(|| s.name.clone())).collect::<Vec<_>>().join(", ");
        ToolError::invalid(format!("Unknown build-with {name:?}.")).with_remediation(format!("Use one of: {known}."))
    })
}

/// The stack a project starts with. One edited in place keeps what its files are built with,
/// unless its request names another; any other goes by its request and target.
pub(super) fn first_stack(kinds: &KindRegistry, ws: &Workspace, deep: bool) -> Option<&'static str> {
    let m = &ws.manifest;
    let own = m.app_in_root && kinds.named(&m.request, &m.target).is_none();
    let found = own.then(|| stacks::found_in(&ws.app_dir())).flatten();
    found.map(|s| s.name.as_str()).or_else(|| kinds.choose(&m.request, &m.target, deep).map(|(_, stack)| stack))
}

/// The part of the target the request says to rebuild ("only the game area, the rest doesn't
/// matter"), as a typed [`Focus`] for the manifest. This is the one place these words are read:
/// the study and the checks follow the typed field. Version 1 recognises an embedded game area
/// only; a request that scopes to anything else stays the whole target (said as no focus).
///
/// Naming the game as the rebuild's own object ("复刻这个游戏", "rebuild this game") is the
/// same ask in fewer words (the person, 2026-11-11: a run rebuilt the portal page from
/// screenshots because only explicit scoping words were read): on a web page it sets the same
/// game-area focus, so the true-rebuild chain runs. A game that is the whole target (its own
/// folder, or no target at all) and a request that names the page ("复刻这个游戏页面") stay
/// the whole target.
pub fn read_focus(request: &str, target: &TargetRef) -> Option<Focus> {
    let r = request.to_lowercase();
    let game = ["游戏", "遊戲", "game"].iter().any(|w| r.contains(w));
    let scoped = [
        "只关注",
        "只關注",
        "只看",
        "只重視",
        "其他区域不需要",
        "其他區域不需要",
        "其他部分不需要",
        "其他区域不用",
        "其他部分不用",
        "忽略其他",
        "忽略其餘",
        "其余部分不",
        "其餘部分不",
        "其他不用",
        "不管其他",
        "only the game",
        "just the game",
        "only focus on",
        "ignore the rest",
        "ignore the page around",
        "don't care about the rest",
        "don't rebuild the rest",
        "don't rebuild the page around",
        "everything else out of scope",
        "not the page around",
    ]
    .iter()
    .any(|p| r.contains(p));
    if game && scoped {
        return Some(Focus { what: "the game area".into(), element_id: None });
    }
    // The game named as the rebuild's object, on a web page only.
    let names_the_game =
        ["这个游戏", "這個遊戲", "此游戏", "此遊戲", "该游戏", "該遊戲", "这款游戏", "這款遊戲", "this game", "that game"].iter().any(|p| r.contains(p));
    let names_the_page =
        ["这个游戏页面", "这个游戏网页", "这个游戏网站", "这个游戏页", "這個遊戲頁面", "this game page", "this game website", "this game site"]
            .iter()
            .any(|p| r.contains(p));
    let rebuilds = [
        "复刻",
        "復刻",
        "重做",
        "克隆",
        "重建",
        "重现",
        "重現",
        "复现",
        "復現",
        "重制",
        "重製",
        "remake",
        "remade",
        "rebuild",
        "rebuilt",
        "clone",
        "cloned",
        "recreate",
        "recreated",
        "replicate",
        "remaster",
        "reproduce",
        "copy",
    ]
    .iter()
    .any(|p| r.contains(p));
    (matches!(target, TargetRef::Website { .. }) && names_the_game && rebuilds && !names_the_page)
        .then(|| Focus { what: "the game area".into(), element_id: None })
}

/// Whether the request asks for the original's own files to run as they are (the only way a
/// rebuild runs the original's compiled runtime instead of a true rebuild — R2.12). This is the
/// one place these words are read: the focused brief and the claims follow this answer. Default
/// false: a rebuild is a true rebuild (the builder's own code) unless the person asks for this.
pub fn run_as_is_asked(request: &str) -> bool {
    let r = request.to_lowercase();
    [
        "原样跑",
        "原样运行",
        "原封不动",
        "直接运行原版",
        "直接运行原游戏",
        "用原版文件运行",
        "运行原版文件",
        "运行原游戏文件",
        "把原版跑起来",
        "把原游戏跑起来",
        "run the original as it is",
        "run the original as they are",
        "run the original files",
        "run the original's own files",
        "run the original unmodified",
        "run the game's own files",
        "run it unmodified",
    ]
    .iter()
    .any(|p| r.contains(p))
}

/// A new project for `request` about `target` (a web address, an absolute path, or `-` for none),
/// at `place`, with the stack the person
/// picked (`build_with`), the asset budget from settings, and `generate_assets`: the person lets the
/// builder generate pictures and 3D models.
pub fn new_project(
    settings: &Settings,
    target: &str,
    request: &str,
    place: Place,
    build_with: Option<&str>,
    generate_assets: bool,
    builder: Option<&str>,
) -> Result<Workspace, ToolError> {
    let stack = build_with.map(self::build_with).transpose()?;
    let target_ref = if target.trim() == "-" { TargetRef::None } else { TargetRef::parse(target) };
    if target_ref == TargetRef::None && target.trim() != "-" {
        return Err(ToolError::invalid(format!("{target:?} is neither a web address nor an existing absolute path"))
            .with_remediation("Pass https://… for a website, a full path for a program, or - for none."));
    }
    let name = suggest_name(request, &target_ref);
    let mut ws = match place {
        Place::Projects => Workspace::create(&settings.projects_dir(), &name, request, target_ref)?,
        Place::In(dir) => Workspace::create(dir, &name, request, target_ref)?,
        Place::At(folder, existing) => Workspace::create_at(folder, &name, request, target_ref, existing)?,
    };
    if let Some(s) = stack {
        ws.manifest.stack = s.name.clone();
    }
    let budget = settings.assets.budget();
    ws.set_person(|m| {
        m.asset_budget_usd = budget;
        m.generate_assets = generate_assets;
        m.builder = builder.map(str::to_string);
    })?;
    Ok(ws)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// R2.11: the one place the person's scoping words are read. A game with the rest of the page
    /// out of scope becomes a typed focus; a game on its own ("复刻一个游戏") is the whole target,
    /// as is anything that scopes to something that isn't an embedded game. Naming the game as
    /// the rebuild's object on a web page ("复刻这个游戏", the person 2026-11-11) is the same
    /// ask in fewer words and sets the same focus; on its own folder or with no target, and when
    /// the request names the page, it is the whole target.
    #[test]
    fn the_request_says_whether_just_the_game_is_wanted() {
        let web = TargetRef::Website { url: "https://poki.com/zh/g/snake-vs-human".into() };
        for request in [
            "你参考 https://x/game 复刻一个游戏，只关注游戏区域，其他区域不需要关注。",
            "Rebuild the game at https://x; only the game area, ignore the rest of the page",
            "clone this game, just the game, not the page around it",
        ] {
            let f = read_focus(request, &web).unwrap_or_else(|| panic!("{request} sets a focus"));
            assert_eq!(f.what, "the game area");
            assert_eq!(f.element_id, None);
        }
        for request in ["https://poki.com/zh/g/snake-vs-human  复刻这个游戏", "复刻这个游戏", "Rebuild this game 1:1", "帮我重制一下这个游戏"]
        {
            let f = read_focus(request, &web).unwrap_or_else(|| panic!("{request} names the game on a page"));
            assert_eq!(f.what, "the game area");
            assert_eq!(f.element_id, None);
        }
        for request in [
            "复刻一个游戏",
            "只关注表单区域，其他不需要",
            "make me a game like that one",
            "复刻这个网站",
            "only the pricing section, ignore the rest",
            "复刻这个游戏页面",
        ] {
            assert!(read_focus(request, &web).is_none(), "{request:?} is the whole target");
        }
        // A game that is the whole target has no page around it to ignore.
        for target in [TargetRef::None, TargetRef::Local { path: "/games/x".into() }] {
            for request in ["复刻这个游戏", "Rebuild this game 1:1"] {
                assert!(read_focus(request, &target).is_none(), "{request:?} on {target:?} is the whole target");
            }
        }
    }
}
