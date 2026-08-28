use super::*;

#[tokio::test(flavor = "current_thread")]
async fn screenshot_resolves_preferred_ratios_at_layout_boundaries() {
    run_page_vm_async_test(async move {
        let loader =
            crate::network::ResourceRequestClient::new(&FetchConfig::default()).expect("loader");
        let mut page_vm = test_page_vm_with_loader_and_document_url(
            &loader,
            Vec::new(),
            Url::parse("https://example.com/preferred-aspect-ratio.html")?,
        );
        page_vm.vm_mut().eval(
            r#"
document.head.innerHTML = `<style>
html,body{margin:0}
.ordinary{display:block;width:100px;border:20px solid}
#ratio-content{aspect-ratio:2/1}
#ratio-border{box-sizing:border-box;aspect-ratio:2/1}
#auto-content{aspect-ratio:auto 2/1}
#auto-border{box-sizing:border-box;aspect-ratio:auto 2/1}
svg{display:block;width:120px;height:auto}
.asymmetric-insets{
  box-sizing:border-box;
  padding:3px 10px;
  border:solid;
  border-width:2px 5px;
}
#natural-auto{aspect-ratio:auto}
#natural-auto-fallback{aspect-ratio:auto 3/2}
#fallback-no-natural{aspect-ratio:auto 3/2}
#specified-border{aspect-ratio:3/2}
#natural-border{aspect-ratio:auto}
#height-only{width:auto;height:50px;aspect-ratio:2/1}
#both-definite{width:130px;height:70px;aspect-ratio:1/1}
#degenerate{aspect-ratio:0/1}
#auto-degenerate{aspect-ratio:auto 0/1}
</style>`;
document.body.innerHTML = `
<div class=ordinary id=ratio-content></div>
<div class=ordinary id=ratio-border></div>
<div class=ordinary id=auto-content></div>
<div class=ordinary id=auto-border></div>
<svg id=natural-auto viewBox="0 0 200 100"></svg>
<svg id=natural-auto-fallback viewBox="0 0 200 100"></svg>
<svg class=asymmetric-insets id=fallback-no-natural></svg>
<svg class=asymmetric-insets id=specified-border viewBox="0 0 200 100"></svg>
<svg class=asymmetric-insets id=natural-border viewBox="0 0 200 100"></svg>
<svg id=height-only viewBox="0 0 200 100"></svg>
<svg id=both-definite viewBox="0 0 200 100"></svg>
<svg id=degenerate viewBox="0 0 200 100"></svg>
<svg id=auto-degenerate viewBox="0 0 200 100"></svg>`;
'installed'
"#,
        )?;
        page_vm.vm_mut().sync_live_document_style_sources();
        page_vm
            .vm_mut()
            .screenshot_layout_snapshot(moli_layout::PaintViewport::new(640, 1_400, 1.0))?
            .expect("preferred aspect-ratio fixture must retain a layout root");

        let geometry = page_vm.vm_mut().eval(
            r#"JSON.stringify(Object.fromEntries([...document.querySelectorAll('[id]')].map(element=>{const rect=element.getBoundingClientRect();return [element.id,[rect.width,rect.height]]})))"#,
        )?;
        let geometry: serde_json::Value = serde_json::from_str(&geometry)?;
        for (id, expected) in [
            ("ratio-content", [140, 90]),
            ("ratio-border", [100, 50]),
            ("auto-content", [140, 90]),
            ("auto-border", [100, 70]),
            ("natural-auto", [120, 60]),
            ("natural-auto-fallback", [120, 60]),
            ("fallback-no-natural", [120, 70]),
            ("specified-border", [120, 80]),
            ("natural-border", [120, 55]),
            ("height-only", [100, 50]),
            ("both-definite", [130, 70]),
            ("degenerate", [120, 60]),
            ("auto-degenerate", [120, 60]),
        ] {
            assert_eq!(
                geometry[id],
                serde_json::json!(expected),
                "Chromium-calibrated geometry mismatch for {id}: {geometry}"
            );
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("preferred aspect-ratio fixture should run");
}

#[tokio::test(flavor = "current_thread")]
async fn screenshot_ignores_preferred_aspect_ratios_on_internal_table_boxes() {
    run_page_vm_async_test(async move {
        let loader =
            crate::network::ResourceRequestClient::new(&FetchConfig::default()).expect("loader");
        let mut page_vm = test_page_vm_with_loader_and_document_url(
            &loader,
            Vec::new(),
            Url::parse("https://example.com/table-internal-aspect-ratio.html")?,
        );
        page_vm.vm_mut().eval(
            r#"
document.head.innerHTML = `<style>
html,body{margin:0}
table{border-collapse:collapse}
th,td{padding:0}
</style>`;
document.body.innerHTML = `
  <table id=internal>
    <tr><th style="width:100px;aspect-ratio:1/1"></th><td id=internal-cell style="height:50px;aspect-ratio:4/1"></td><td style="height:50px;min-width:min-content;aspect-ratio:4/1"></td></tr>
  </table>
  <table id=outer style="width:100px;aspect-ratio:2/1"></table>`;
'installed'
"#,
        )?;
        page_vm.vm_mut().sync_live_document_style_sources();
        page_vm
            .vm_mut()
            .screenshot_layout_snapshot(moli_layout::PaintViewport::new(320, 200, 1.0))?
            .expect("internal table aspect-ratio screenshot layout");

        let geometry = page_vm.vm_mut().eval(
            r#"JSON.stringify(Object.fromEntries(['internal','outer'].map(id=>{const r=document.getElementById(id).getBoundingClientRect();return [id,[r.width,r.height]]})))"#,
        )?;
        let geometry: serde_json::Value = serde_json::from_str(&geometry)?;
        for (id, expected) in [("internal", [100.0, 50.0]), ("outer", [100.0, 50.0])] {
            let actual = geometry[id]
                .as_array()
                .unwrap_or_else(|| panic!("missing geometry for {id}: {geometry}"));
            for (axis, expected) in expected.into_iter().enumerate() {
                let actual = actual[axis].as_f64().expect("numeric geometry") as f32;
                assert!(
                    (actual - expected).abs() <= 0.05,
                    "{id}[{axis}]: expected {expected}, got {actual}; geometry={geometry}"
                );
            }
        }
        assert_eq!(
            page_vm
                .vm_mut()
                .eval("getComputedStyle(document.getElementById('internal-cell')).aspectRatio")?,
            "4 / 1"
        );
        Ok::<_, anyhow::Error>(())
    })
    .await
    .expect("internal table aspect-ratio fixture should run");
}
