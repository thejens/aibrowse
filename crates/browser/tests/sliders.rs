//! Setting range sliders from the keyboard: a native range input and an ARIA slider
//! that handles its own keys, as shops' price filters do. Ignored by default; run with
//! `make test-live`.

mod common;

use aibrowse_browser::{Browser, LaunchOptions};
use common::{backend_id, page_text};

const PAGE: &str = r##"<!doctype html>
<title>sliders</title>
<label>Max price <input id="native" type="range" min="0" max="10000" step="100" value="8000"></label>
<div id="aria" role="slider" tabindex="0" aria-label="Maximum price" aria-valuemin="0"
     aria-valuemax="10000" aria-valuenow="7999" style="width:300px;height:24px;background:#ddd"></div>
<span id="mui" style="display:inline-block;position:relative;width:20px;height:20px;background:#555">
  <input id="hidden" type="range" min="0" max="10000" step="1" value="8000"
         style="border:0;clip-path:inset(50%);height:100%;margin:-1px;overflow:hidden;padding:0;position:absolute;white-space:nowrap;width:100%">
</span>
<div id="redraw"></div>
<div id="track" style="position:relative;width:400px;height:24px;margin:40px 20px;background:#eee">
  <div id="drag" role="slider" aria-label="Maximum price" aria-valuemin="0" aria-valuemax="10000"
       aria-valuenow="8000" style="position:absolute;left:304px;top:0;width:24px;height:24px;background:#333"></div>
</div>
<script>
  // Follows the pointer only, as many shops' price sliders do.
  const track = document.getElementById('track');
  const thumb = document.getElementById('drag');
  let dragging = false;
  thumb.addEventListener('pointerdown', (e) => { dragging = true; thumb.setPointerCapture(e.pointerId); });
  thumb.addEventListener('pointerup', () => { dragging = false; });
  thumb.addEventListener('pointermove', (e) => {
    if (!dragging) return;
    const r = track.getBoundingClientRect();
    const x = Math.min(r.width - 24, Math.max(0, e.clientX - r.left - 12));
    thumb.style.left = x + 'px';
    thumb.setAttribute('aria-valuenow', String(Math.round(x / (r.width - 24) * 10000)));
  });
  // Applies the filter on every change and draws the slider anew, as shops do.
  const box = document.getElementById('redraw');
  const draw = (v) => {
    box.innerHTML = `<div role="slider" tabindex="0" aria-label="Upper" aria-valuemin="0"
      aria-valuemax="10000" aria-valuenow="${v}" style="width:300px;height:24px;background:#ccc"></div>`;
  };
  draw(10000);
  box.addEventListener('keydown', (e) => {
    const steps = { ArrowLeft: -100, ArrowRight: 100, PageDown: -1000, PageUp: 1000 };
    if (!(e.key in steps)) return;
    e.preventDefault();
    const now = Number(box.firstElementChild.getAttribute('aria-valuenow'));
    const v = Math.min(10000, Math.max(0, now + steps[e.key]));
    box.firstElementChild.setAttribute('aria-valuenow', String(v));
    setTimeout(() => draw(v), 100);
  });
  const s = document.getElementById('aria');
  s.addEventListener('keydown', (e) => {
    const steps = { ArrowLeft: -250, ArrowRight: 250, PageDown: -1000, PageUp: 1000 };
    if (!(e.key in steps)) return;
    e.preventDefault();
    const v = Math.min(10000, Math.max(0, Number(s.getAttribute('aria-valuenow')) + steps[e.key]));
    s.setAttribute('aria-valuenow', String(v));
  });
</script>"##;

#[tokio::test]
#[ignore = "needs Chrome; run with `make test-live`"]
async fn sliders_are_set_from_the_keyboard() {
    let port = common::serve(PAGE).await;
    let browser = Browser::launch(LaunchOptions {
        profile: "test-sliders".into(),
        headless: true,
        mute: true,
        ..LaunchOptions::default()
    })
    .await
    .expect("launch");
    let mut tab = browser.open_tab().await.expect("tab");
    tab.navigate(&format!("http://localhost:{port}/"))
        .await
        .expect("navigate");

    let native = backend_id(&tab, "#native").await;
    tab.set_range(native, 5000.0).await.expect("set native");
    assert_eq!(page_text(&tab, "#native", "value").await, "5000");

    // Steps of 250 can't land on 5000 from 7999: the closest it can reach.
    let aria = backend_id(&tab, "#aria").await;
    let done = tab.set_range(aria, 5000.0).await.expect("set aria");
    let now: f64 = page_text(&tab, "#aria", "getAttribute('aria-valuenow')")
        .await
        .parse()
        .unwrap();
    assert!((now - 5000.0).abs() <= 125.0, "{now} {:?}", done.notes);

    // A visually hidden range input under a drawn thumb (MUI's) is focused, not clicked.
    let hidden = backend_id(&tab, "#hidden").await;
    tab.set_range(hidden, 5000.0).await.expect("set hidden");
    let now: f64 = page_text(&tab, "#hidden", "value").await.parse().unwrap();
    assert!((now - 5000.0).abs() <= 200.0, "{now}");

    // One drawn anew after every key is found again and set all the same.
    let redraw = backend_id(&tab, "#redraw [role=slider]").await;
    let done = tab.set_range(redraw, 5000.0).await.expect("set redrawn");
    let now: f64 = page_text(
        &tab,
        "#redraw [role=slider]",
        "getAttribute('aria-valuenow')",
    )
    .await
    .parse()
    .unwrap();
    assert!((now - 5000.0).abs() <= 100.0, "{now} {:?}", done.notes);

    // One that follows only the pointer is dragged there.
    let drag = backend_id(&tab, "#drag").await;
    let done = tab.set_range(drag, 5000.0).await.expect("set drag");
    let now: f64 = page_text(&tab, "#drag", "getAttribute('aria-valuenow')")
        .await
        .parse()
        .unwrap();
    assert!((now - 5000.0).abs() <= 200.0, "{now} {:?}", done.notes);

    browser.close().await.expect("close");
}

// A price popover that opens with an animation, applies the filter on the first change
// and closes, as XXL's and Intersport's do; another slider stays on the page.
const POPOVER: &str = r##"<!doctype html>
<title>popover</title>
<div id="popover" style="position:relative;height:24px;margin:40px 20px;width:0;overflow:hidden;transition:width 600ms">
  <input id="once" type="range" min="0" max="10000" step="100" value="10000" style="width:100%;margin:0">
</div>
<p id="applied">none</p>
<div id="mui-pop" style="margin:40px 20px">
  <span id="root" style="position:relative;display:block;width:400px;height:22px;background:#ddd">
    <span id="thumb" style="position:absolute;left:100%;top:0;width:22px;height:22px;margin-left:-11px;background:#333">
      <input id="hidden-once" type="range" min="0" max="10000" step="1" value="10000" aria-label="Maximum price"
             style="border:0;clip-path:inset(50%);height:100%;margin:-1px;overflow:hidden;padding:0;position:absolute;white-space:nowrap;width:100%">
    </span>
  </span>
</div>
<p id="mui-applied">none</p>
<input id="other" type="range" min="0" max="10000" step="100" value="8000" style="width:400px">
<script>
  const popover = document.getElementById('popover');
  requestAnimationFrame(() => requestAnimationFrame(() => { popover.style.width = '400px'; }));
  document.getElementById('once').addEventListener('change', (e) => {
    document.getElementById('applied').textContent = e.target.value;
    popover.remove();
  });
  // MUI's way: a visually hidden range input inside the drawn thumb, the thumb dragged
  // along the slider root; the filter applies on release and the panel closes.
  const root = document.getElementById('root');
  const thumb = document.getElementById('thumb');
  const hidden = document.getElementById('hidden-once');
  const show = (v) => { hidden.value = v; thumb.style.left = (v / 100) + '%'; };
  let held = false;
  thumb.addEventListener('pointerdown', (e) => { held = true; thumb.setPointerCapture(e.pointerId); });
  thumb.addEventListener('pointermove', (e) => {
    if (!held) return;
    const r = root.getBoundingClientRect();
    show(Math.round(Math.min(1, Math.max(0, (e.clientX - r.left) / r.width)) * 10000));
  });
  const apply = () => {
    document.getElementById('mui-applied').textContent = hidden.value;
    document.getElementById('mui-pop').remove();
  };
  thumb.addEventListener('pointerup', () => { if (held) { held = false; apply(); } });
  hidden.addEventListener('keydown', (e) => {
    const steps = { ArrowLeft: -100, ArrowRight: 100, PageDown: -1000, PageUp: 1000 };
    if (!(e.key in steps)) return;
    e.preventDefault();
    show(Math.min(10000, Math.max(0, Number(hidden.value) + steps[e.key])));
    apply();
  });
</script>"##;

#[tokio::test]
#[ignore = "needs Chrome; run with `make test-live`"]
async fn a_slider_whose_panel_closes_on_the_first_change() {
    let port = common::serve(POPOVER).await;
    let browser = Browser::launch(LaunchOptions {
        profile: "test-sliders-popover".into(),
        headless: true,
        mute: true,
        ..LaunchOptions::default()
    })
    .await
    .expect("launch");
    let mut tab = browser.open_tab().await.expect("tab");
    tab.navigate(&format!("http://localhost:{port}/"))
        .await
        .expect("navigate");
    // At once, while the popover is still opening.
    let once = backend_id(&tab, "#once").await;
    let done = tab.set_range(once, 5000.0).await.expect("set once");
    let applied = page_text(&tab, "#applied", "textContent").await;
    let other = page_text(&tab, "#other", "value").await;
    // Dragged most of the way in its one change; nothing else on the page is touched.
    let value: f64 = applied.parse().unwrap_or(f64::NAN);
    assert!(
        (value - 5000.0).abs() <= 500.0 && other == "8000",
        "applied {applied}, other {other}, {:?}",
        done.notes
    );
    // The same with a hidden input inside its drawn thumb (MUI's): the thumb is
    // dragged along the slider root, not the input taken for a 22 px slider.
    let hidden = backend_id(&tab, "#hidden-once").await;
    let done = tab
        .set_range(hidden, 5000.0)
        .await
        .expect("set hidden once");
    let applied = page_text(&tab, "#mui-applied", "textContent").await;
    let value: f64 = applied.parse().unwrap_or(f64::NAN);
    let other = page_text(&tab, "#other", "value").await;
    assert!(
        (value - 5000.0).abs() <= 500.0 && other == "8000",
        "applied {applied}, other {other}, {:?}",
        done.notes
    );
    browser.close().await.expect("close");
}
