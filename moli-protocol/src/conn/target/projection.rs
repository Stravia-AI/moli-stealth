use super::graph::TopLevelTarget;
use crate::devtools_runtime::{DevToolsTargetId, DevToolsTargetInfo, DevToolsTargetKind};

pub(crate) fn tab_target_info_from_page_target_info(
    target: &TopLevelTarget,
    page_target_info: DevToolsTargetInfo,
) -> DevToolsTargetInfo {
    DevToolsTargetInfo {
        target_id: Some(DevToolsTargetId::from(target.tab_target_id())),
        kind: DevToolsTargetKind::Tab,
        title: page_target_info.title,
        url: page_target_info.url,
        attached: target.tab_has_session(),
        // Chromium's tab DevToolsAgentHost delegates opener identity and
        // access to its primary frame host. Preserve the same relationship
        // when projecting our page target into a tab target.
        opener_id: page_target_info.opener_id,
        opener_frame_id: page_target_info.opener_frame_id,
        can_access_opener: page_target_info.can_access_opener,
        browser_context_id: page_target_info.browser_context_id,
        moli_popup_id: None,
    }
}

pub(crate) fn project_tab_page_target_infos(
    target: Option<&TopLevelTarget>,
    target_info: DevToolsTargetInfo,
) -> Vec<DevToolsTargetInfo> {
    let mut target_infos = Vec::new();
    if target_info.kind == DevToolsTargetKind::Page
        && let Some(target) = target
    {
        target_infos.push(tab_target_info_from_page_target_info(
            target,
            target_info.clone(),
        ));
    }
    target_infos.push(target_info);
    target_infos
}

#[cfg(test)]
mod tests {
    use crate::devtools_runtime::{
        DevToolsBrowserContextId, DevToolsFrameId, DevToolsTargetId, DevToolsTargetInfo,
        DevToolsTargetKind,
    };

    use super::super::graph::TargetGraph;
    use super::tab_target_info_from_page_target_info;

    #[test]
    fn tab_projection_preserves_noopener_creator_identity_and_access_policy() {
        let mut graph = TargetGraph::default();
        graph.register_top_level_page("TID-page".to_owned(), "TID-tab".to_owned());
        let target = graph
            .top_level_target_for_page_target_id("TID-page")
            .expect("registered top-level target");
        let tab = tab_target_info_from_page_target_info(
            target,
            DevToolsTargetInfo {
                target_id: Some(DevToolsTargetId::from("TID-page")),
                kind: DevToolsTargetKind::Page,
                title: String::new(),
                url: "about:blank".to_owned(),
                attached: true,
                opener_id: Some(DevToolsTargetId::from("TID-opener")),
                opener_frame_id: Some(DevToolsFrameId::from("FRAME-opener")),
                can_access_opener: false,
                browser_context_id: Some(DevToolsBrowserContextId::from("BID-1")),
                moli_popup_id: None,
            },
        );

        assert_eq!(tab.opener_id.unwrap().as_str(), "TID-opener");
        assert_eq!(tab.opener_frame_id.unwrap().as_str(), "FRAME-opener");
        assert!(!tab.can_access_opener);
    }
}
