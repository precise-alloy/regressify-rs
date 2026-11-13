//! Report browsing and image loading are isolated from editable test documents.

use std::{
    cell::{Cell, RefCell},
    path::Path,
    rc::Rc,
    sync::Arc,
};

use super::*;
use crate::compare::image_compare::{
    DiffAlignment, VerticalGap, gaps_to_aligned_row, gaps_to_image_row,
};
use crate::desktop::results::{
    Catalog, Outcome, Pair, ReportArtifacts, ReportEntry, ReportSelection,
};
use gpui_kit::component::hover_card::HoverCard;
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::slider::{Slider, SliderState};

mod log;

const ZOOM_LEVELS: [f32; 19] = [
    0.25,
    1. / 3.,
    0.5,
    2. / 3.,
    0.75,
    0.8,
    0.9,
    1.,
    1.1,
    1.25,
    1.5,
    1.75,
    2.,
    2.5,
    3.,
    4.,
    5.,
    6.,
    8.,
];

fn next_zoom(current: f32, increase: bool) -> f32 {
    if increase {
        ZOOM_LEVELS
            .iter()
            .copied()
            .find(|zoom| *zoom > current + 0.001)
            .unwrap_or(ZOOM_LEVELS[ZOOM_LEVELS.len() - 1])
    } else {
        ZOOM_LEVELS
            .iter()
            .copied()
            .rfind(|zoom| *zoom < current - 0.001)
            .unwrap_or(current.min(ZOOM_LEVELS[0]))
    }
}

fn image_page_url(pair: &Pair, index: usize) -> Option<reqwest::Url> {
    let value = match index {
        0 => pair.reference_url.as_deref(),
        1 => pair.url.as_deref(),
        _ => None,
    }?;
    reqwest::Url::parse(value)
        .ok()
        .filter(|url| matches!(url.scheme(), "http" | "https" | "file"))
}

pub(super) enum ResultEvent {
    Cleanup {
        root: PathBuf,
        suite: Option<String>,
    },
    OpenSuite(Entry),
    ApproveRun(ReportApproval),
}

#[derive(Clone)]
pub(super) struct ReportApproval {
    pub entry: Entry,
    pub output: PathBuf,
    pub suite_id: String,
    pub run_id: String,
    pub scenario_id: String,
    pub capture_key: String,
}

struct Preview {
    label: &'static str,
    result: std::result::Result<LoadedImage, String>,
}

struct LoadedImage {
    image: Arc<Image>,
    width: usize,
    height: usize,
    path: Option<PathBuf>,
}

impl LoadedImage {
    fn load(
        artifacts: &ReportArtifacts,
        root: &Path,
        label: &'static str,
        path: Option<&str>,
    ) -> Result<Self> {
        match path.map(|path| artifacts.read_image(root, path)) {
            Some(Ok((image, bytes))) => {
                if matches!(label, "Reference" | "Test")
                    && crate::test::capture::read_error_marker(&image.path).is_some()
                {
                    return Self::error_placeholder();
                }
                Ok(Self {
                    image: Arc::new(Image::from_bytes(ImageFormat::Png, bytes)),
                    width: image.width,
                    height: image.height,
                    path: Some(image.path),
                })
            }
            Some(Err(error))
                if label == "Reference"
                    && error
                        .downcast_ref::<std::io::Error>()
                        .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
            {
                Self::error_placeholder()
            }
            None if label == "Reference" => Self::error_placeholder(),
            Some(Err(error)) => Err(error),
            None => Err(anyhow!("No {} image", label.to_lowercase())),
        }
    }

    fn error_placeholder() -> Result<Self> {
        let bytes = crate::test::capture::error_icon_png();
        let (width, height) = crate::helpers::png::read_bounded_png_dimensions_from_reader(
            bytes,
            Path::new("error-icon.png"),
        )?;
        Ok(Self {
            image: Arc::new(Image::from_bytes(ImageFormat::Png, bytes.to_vec())),
            width,
            height,
            path: None,
        })
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ZoomMode {
    FitArea,
    FitWidth,
    Manual,
}

#[derive(Clone, Copy)]
struct PreviewView {
    mode: ZoomMode,
    zoom: f32,
    // Fractions of unused lane space keep fitted dragging bounded after resize.
    horizontal_position: Option<f32>,
    vertical_position: Option<f32>,
    padding: Option<gpui_kit::Edges<Pixels>>,
}

impl Default for PreviewView {
    fn default() -> Self {
        Self {
            mode: ZoomMode::FitArea,
            zoom: 1.0,
            horizontal_position: None,
            vertical_position: None,
            padding: None,
        }
    }
}

#[derive(Default, Clone)]
struct PreviewTexture(Rc<RefCell<Option<Arc<gpui_kit::RenderImage>>>>);

impl PreviewTexture {
    fn source(&self, image: Arc<Image>) -> gpui_kit::ImageSource {
        let texture = self.clone();
        gpui_kit::ImageSource::from(move |window: &mut Window, cx: &mut App| {
            let rendered = image.clone().use_render_image(window, cx)?;
            *texture.0.borrow_mut() = Some(rendered.clone());
            Some(Ok(rendered))
        })
    }
}

#[derive(Clone)]
struct PreviewPan {
    position: Point<Pixels>,
    offset: Point<Pixels>,
    scroll: ScrollHandle,
    horizontal: bool,
    horizontal_origin: Pixels,
    horizontal_space: Pixels,
    vertical: bool,
    vertical_origin: Pixels,
    vertical_space: Pixels,
    dragged: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct PointerGeometry {
    canvas: (usize, usize),
    gap: Option<VerticalGap>,
    gaps: Vec<VerticalGap>,
}

impl PointerGeometry {
    fn effective_gaps(&self) -> &[VerticalGap] {
        if !self.gaps.is_empty() {
            &self.gaps
        } else if let Some(gap) = &self.gap {
            std::slice::from_ref(gap)
        } else {
            &[]
        }
    }
}

#[derive(Default)]
struct PointerGuides {
    active: Cell<Option<usize>>,
    bounds: [Cell<Option<Bounds<Pixels>>>; 3],
    geometry: [RefCell<Option<PointerGeometry>>; 3],
    alignment: RefCell<Option<DiffAlignment>>,
    pan: RefCell<Option<PreviewPan>>,
}

impl PointerGuides {
    fn position(&self, index: usize, mouse: Point<Pixels>) -> Option<Point<Pixels>> {
        let active = self.active.get()?;
        let source = self.bounds.get(active)?.get()?;
        let target = self.bounds.get(index)?.get()?;
        let source_geometry = self.geometry.get(active)?.borrow().clone()?;
        let target_geometry = self.geometry.get(index)?.borrow().clone()?;
        if source.size.width <= px(0.) || source.size.height <= px(0.) {
            return None;
        }
        let column =
            (mouse.x - source.origin.x) / source.size.width * source_geometry.canvas.0 as f32;
        let row =
            (mouse.y - source.origin.y) / source.size.height * source_geometry.canvas.1 as f32;
        // Lane hover includes blank space on every side of the shared image canvas.
        let aligned_row = gaps_to_aligned_row(source_geometry.effective_gaps(), row);
        let target_row = gaps_to_image_row(target_geometry.effective_gaps(), aligned_row)?;
        Some(point(
            target.origin.x + target.size.width * (column / target_geometry.canvas.0 as f32),
            target.origin.y + target.size.height * (target_row / target_geometry.canvas.1 as f32),
        ))
    }
}

struct PointerLayer {
    index: usize,
    guides: Rc<PointerGuides>,
    owner: WeakEntity<ResultsPanel>,
}

impl Render for PointerLayer {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let index = self.index;
        let guides = self.guides.clone();
        let owner = self.owner.clone();
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                let panning = guides.clone();
                window.on_mouse_event(
                    move |event: &gpui_kit::MouseMoveEvent, phase, window, cx| {
                        if phase != gpui_kit::DispatchPhase::Bubble {
                            return;
                        }
                        let Some(pan) = panning.pan.borrow().clone() else {
                            return;
                        };
                        if event.pressed_button != Some(MouseButton::Left) {
                            panning.pan.borrow_mut().take();
                            window.refresh();
                            return;
                        }
                        let movement = event.position - pan.position;
                        if !pan.dragged && movement.x.abs().max(movement.y.abs()) < px(4.) {
                            return;
                        }
                        if let Some(pan) = panning.pan.borrow_mut().as_mut() {
                            pan.dragged = true;
                        }
                        if (pan.horizontal_space > px(0.) || pan.vertical_space > px(0.))
                            && let Some(owner) = owner.upgrade()
                        {
                            owner.update(cx, |panel, cx| {
                                let view = &mut panel.preview_views[3];
                                if pan.horizontal_space > px(0.) {
                                    view.horizontal_position = Some(
                                        ((pan.horizontal_origin + movement.x)
                                            / pan.horizontal_space)
                                            .clamp(0., 1.),
                                    );
                                }
                                if pan.vertical_space > px(0.) {
                                    view.vertical_position = Some(
                                        ((pan.vertical_origin + movement.y) / pan.vertical_space)
                                            .clamp(0., 1.),
                                    );
                                }
                                cx.notify();
                            });
                        }
                        let maximum = pan.scroll.max_offset();
                        pan.scroll.set_offset(point(
                            if pan.horizontal_space > px(0.) {
                                px(0.)
                            } else if pan.horizontal {
                                (pan.offset.x + movement.x).clamp(-maximum.x, px(0.))
                            } else {
                                pan.offset.x
                            },
                            if pan.vertical_space > px(0.) {
                                px(0.)
                            } else if pan.vertical {
                                (pan.offset.y + movement.y).clamp(-maximum.y, px(0.))
                            } else {
                                pan.offset.y
                            },
                        ));
                        window.refresh();
                        cx.stop_propagation();
                    },
                );
                let panning = guides.clone();
                window.on_mouse_event(move |event: &gpui_kit::MouseUpEvent, phase, window, cx| {
                    if phase == gpui_kit::DispatchPhase::Capture
                        && event.button == MouseButton::Left
                        && let Some(pan) = panning.pan.borrow_mut().take()
                    {
                        window.refresh();
                        if pan.dragged {
                            cx.stop_propagation();
                        }
                    }
                });
                let Some(pointer) = guides.position(index, window.mouse_position()) else {
                    return;
                };
                for (thickness, color) in [(3., rgba(0x000000b3)), (1., rgba(0xffffffff))] {
                    window.paint_quad(fill(
                        Bounds::new(
                            point(pointer.x - px(thickness / 2.), bounds.origin.y),
                            size(px(thickness), bounds.size.height),
                        ),
                        color,
                    ));
                    window.paint_quad(fill(
                        Bounds::new(
                            point(bounds.origin.x, pointer.y - px(thickness / 2.)),
                            size(bounds.size.width, px(thickness)),
                        ),
                        color,
                    ));
                }
            },
        )
        .size_full()
    }
}

type CaptureListState = gpui_kit::component::list::ListState<CaptureRows>;

struct CaptureRows {
    owner: WeakEntity<ResultsPanel>,
}

impl gpui_kit::component::list::ListDelegate for CaptureRows {
    type Item = gpui_kit::component::list::ListItem;

    fn items_count(&self, _: usize, cx: &App) -> usize {
        self.owner
            .upgrade()
            .map_or(0, |owner| owner.read(cx).visible_captures.len())
    }

    fn render_item(
        &mut self,
        index: IndexPath,
        _: &mut Window,
        cx: &mut Context<CaptureListState>,
    ) -> Option<Self::Item> {
        let owner = self.owner.upgrade()?;
        let panel = owner.read(cx);
        let capture = *panel.visible_captures.get(index.row)?;
        panel.render_capture_row(capture, self.owner.clone(), cx)
    }

    fn set_selected_index(
        &mut self,
        _: Option<IndexPath>,
        _: &mut Window,
        _: &mut Context<CaptureListState>,
    ) {
    }
}

pub(super) struct ResultsPanel {
    capture_log: log::CaptureLogView,
    root: Option<PathBuf>,
    catalog: Catalog,
    suites: Vec<crate::desktop::workspace::SuiteSummary>,
    active_suite: Option<PathBuf>,
    report: Option<usize>,
    capture: Option<usize>,
    visible_captures: Vec<usize>,
    filter: usize,
    capture_list: Entity<CaptureListState>,
    image_mode: usize,
    preview_views: [PreviewView; 6],
    preview_scenario: Option<(PathBuf, String)>,
    visible: bool,
    search: Entity<InputState>,
    _subscription: Subscription,
    focus: FocusHandle,
    capture_focus: FocusHandle,
    preview_focus: FocusHandle,
    blend_focus: FocusHandle,
    blend: Entity<SliderState>,
    _blend_subscription: Subscription,
    previews: Vec<Preview>,
    image_menu_epoch: u64,
    preview_textures: [PreviewTexture; 3],
    image_task: Option<gpui_kit::Task<()>>,
    image_load_gate: Arc<futures::lock::Mutex<()>>,
    preview_scrolls: [ScrollHandle; 6],
    comparison_scroll: ScrollHandle,
    pointer_guides: Rc<PointerGuides>,
    pointer_layers: [Entity<PointerLayer>; 3],
    status: String,
    loading: bool,
    image_loading: bool,
    generation: u64,
    catalog_generation: u64,
    preferred_run: Option<String>,
    preferred_reports: Option<Vec<(String, String)>>,
    approval_confirmation: bool,
    approval_error: Option<String>,
    disabled: bool,
}

impl EventEmitter<ResultEvent> for ResultsPanel {}

pub(super) struct CaptureList {
    results: Entity<ResultsPanel>,
    _subscription: Subscription,
}

impl CaptureList {
    pub(super) fn new(results: Entity<ResultsPanel>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.observe(&results, |_, _, cx| cx.notify());
        Self {
            results,
            _subscription: subscription,
        }
    }
}

impl Render for CaptureList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.results
            .update(cx, |panel, cx| panel.render_captures(window, cx))
    }
}

impl ResultsPanel {
    pub(super) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible && !visible {
            self.reset_view();
            cx.notify();
        }
        self.visible = visible;
    }

    fn reset_view(&mut self) {
        self.approval_confirmation = false;
        self.approval_error = None;
        self.image_menu_epoch = self.image_menu_epoch.wrapping_add(1);
        self.preview_views = Default::default();
        self.comparison_scroll.set_offset(point(px(0.), px(0.)));
        for scroll in &self.preview_scrolls {
            scroll.set_offset(point(px(0.), px(0.)));
        }
        self.pointer_guides.pan.borrow_mut().take();
        self.pointer_guides.active.set(None);
    }

    fn select_image_mode(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.preview_views.len() || (index >= 4 && !self.can_blend()) {
            return;
        }
        self.image_menu_epoch = self.image_menu_epoch.wrapping_add(1);
        self.image_mode = index;
        self.pointer_guides.pan.borrow_mut().take();
        self.pointer_guides.active.set(None);
        cx.notify();
    }

    fn set_zoom_mode(&mut self, mode: ZoomMode, cx: &mut Context<Self>) {
        let view = &mut self.preview_views[self.image_mode];
        if view.mode == mode
            && view.horizontal_position.is_none()
            && view.vertical_position.is_none()
        {
            return;
        }
        view.mode = mode;
        view.horizontal_position = None;
        view.vertical_position = None;
        view.padding = None;
        self.image_scroll().set_offset(point(px(0.), px(0.)));
        self.pointer_guides.pan.borrow_mut().take();
        self.pointer_guides.active.set(None);
        cx.notify();
    }

    fn actual_size(&mut self, cx: &mut Context<Self>) {
        self.preview_views[self.image_mode] = PreviewView {
            mode: ZoomMode::Manual,
            ..Default::default()
        };
        self.pointer_guides.pan.borrow_mut().take();
        self.pointer_guides.active.set(None);
        cx.notify();
    }

    fn image_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let modifiers = event.keystroke.modifiers;
        if !self.preview_focus.is_focused(window)
            || self.capture.is_none()
            || modifiers.control
            || modifiers.platform
            || modifiers.alt
            || gpui_kit::base::GlobalState::is_in_deferred_context(cx)
        {
            return;
        }
        match event.keystroke.key.as_str() {
            "+" | "=" => self.step_zoom(true, cx),
            "-" => self.step_zoom(false, cx),
            "0" => self.set_zoom_mode(ZoomMode::FitArea, cx),
            "1" => self.actual_size(cx),
            "w" => self.set_zoom_mode(ZoomMode::FitWidth, cx),
            "escape" => {
                self.pointer_guides.pan.borrow_mut().take();
                cx.notify();
            }
            key @ ("left" | "right" | "up" | "down") => {
                let scroll = self.image_scroll();
                let step = px(if modifiers.shift { 160. } else { 40. });
                let movement = match key {
                    "left" => point(step, px(0.)),
                    "right" => point(-step, px(0.)),
                    "up" => point(px(0.), step),
                    _ => point(px(0.), -step),
                };
                let offset = scroll.offset() + movement;
                let maximum = scroll.max_offset();
                scroll.set_offset(point(
                    offset.x.clamp(-maximum.x, px(0.)),
                    offset.y.clamp(-maximum.y, px(0.)),
                ));
                self.pointer_guides.active.set(None);
                cx.notify();
            }
            _ => return,
        }
        window.prevent_default();
        cx.stop_propagation();
    }

    pub(super) fn set_disabled(&mut self, disabled: bool, cx: &mut Context<Self>) {
        if self.disabled != disabled {
            self.disabled = disabled;
            if disabled {
                self.approval_confirmation = false;
            }
            cx.notify();
        }
    }

    pub(super) fn approval_failed(&mut self, error: String, cx: &mut Context<Self>) {
        self.approval_confirmation = false;
        self.approval_error = Some(error);
        cx.notify();
    }

    pub(super) fn approval_finished(
        &mut self,
        outcome: Option<&crate::desktop::runner::RunOutcome>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use crate::desktop::runner::RunOutcome;

        if let Some(root) = self.root.clone() {
            self.refresh(root, window, cx);
        }
        self.approval_error = match outcome {
            Some(RunOutcome::Completed) => None,
            Some(RunOutcome::Failed(error)) => Some(format!("Approval failed: {error}")),
            Some(RunOutcome::Cancelled) => Some("Approval cancelled".into()),
            None => Some("Approval did not complete".into()),
        };
        cx.notify();
    }

    pub(super) fn show_run(
        &mut self,
        root: PathBuf,
        run_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_suite = None;
        self.preferred_reports = None;
        self.preferred_run = (!run_id.is_empty()).then_some(run_id);
        self.filter = 0;
        self.image_mode = 3;
        self.reset_view();
        self.search
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.refresh(root, window, cx);
    }

    /// Drops run and saved-report pins so the next refresh selects the latest
    /// default reports, which exclude partial and unfinished runs.
    pub(super) fn follow_latest(&mut self) {
        self.preferred_run = None;
        self.preferred_reports = None;
    }

    pub(super) fn show_saved_reports(
        &mut self,
        root: PathBuf,
        targets: Vec<(String, String)>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let matches = |suite: &&crate::desktop::workspace::SuiteSummary| {
            targets.iter().any(|(id, _)| id == &suite.output_id)
        };
        self.active_suite = self
            .suites
            .iter()
            .filter(matches)
            .find(|suite| self.active_suite.as_ref() == Some(&suite.entry.path))
            .or_else(|| self.suites.iter().find(matches))
            .map(|suite| suite.entry.path.clone());
        self.preferred_reports = Some(targets);
        self.preferred_run = None;
        self.filter = 0;
        self.image_mode = 3;
        self.reset_view();
        self.search
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.refresh(root, window, cx);
    }

    pub(super) fn set_suites(&mut self, suites: Vec<crate::desktop::workspace::SuiteSummary>) {
        if !suites
            .iter()
            .any(|suite| self.active_suite.as_ref() == Some(&suite.entry.path))
        {
            self.active_suite = None;
        }
        self.suites = suites;
    }

    pub(super) fn active_suite(&self) -> Option<&PathBuf> {
        self.active_suite.as_ref()
    }

    pub(super) fn suite_counts(&self, entry: &Entry) -> Option<crate::progress::Counts> {
        self.catalog.reports.iter().find_map(|report| {
            report
                .source
                .as_ref()
                .filter(|source| source.path == entry.path)
                .map(|_| report.report.scenario_counts())
        })
    }

    pub(super) fn select_suite(
        &mut self,
        entry: &Entry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.active_suite.as_ref() == Some(&entry.path) {
            return;
        }
        self.active_suite = Some(entry.path.clone());
        self.report = self.suite_report();
        self.capture = None;
        self.visible_captures.clear();
        self.reset_capture_list(window, cx);
        self.preview_scenario = None;
        self.reset_view();
        self.reset_previews(Some(window), cx);
        let outside_requested = self.preferred_reports.as_ref().is_some_and(|targets| {
            !self.suites.iter().any(|suite| {
                suite.entry.path == entry.path
                    && targets.iter().any(|(id, _)| id == &suite.output_id)
            })
        }) || (self.preferred_run.is_some() && self.report.is_none());
        if outside_requested && let Some(root) = self.root.clone() {
            self.preferred_run = None;
            self.preferred_reports = None;
            self.refresh(root, window, cx);
        } else if !self.loading {
            self.generation += 1;
            self.reconcile_capture(window, cx);
        }
        cx.notify();
    }

    fn suite_report(&self) -> Option<usize> {
        let path = self.active_suite.as_ref()?;
        self.catalog.reports.iter().position(|report| {
            report
                .source
                .as_ref()
                .is_some_and(|entry| &entry.path == path)
        })
    }

    pub(super) fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.generation += 1;
        self.catalog_generation = self.catalog_generation.wrapping_add(1);
        self.root = None;
        self.preferred_run = None;
        self.preferred_reports = None;
        self.catalog = Catalog::default();
        self.suites.clear();
        self.active_suite = None;
        self.report = None;
        self.capture = None;
        self.visible_captures.clear();
        self.filter = 0;
        self.reset_capture_list(window, cx);
        self.preview_scenario = None;
        self.reset_view();
        self.reset_previews(Some(window), cx);
        self.loading = false;
        self.status = "No reports loaded".into();
        self.search
            .update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();
    }

    pub(super) fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        cx.on_release(|this, cx| this.reset_previews(None, cx))
            .detach();
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Filter captures"));
        let subscription =
            cx.subscribe_in(&search, window, |this: &mut Self, _, event, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.reconcile_capture(window, cx);
                }
            });
        let pointer_guides = Rc::<PointerGuides>::default();
        let blend = cx.new(|_| {
            SliderState::new()
                .min(0.)
                .max(100.)
                .step(1.)
                .default_value(50.)
        });
        let blend_subscription = cx.observe(&blend, |_, _, cx| cx.notify());
        let pointer_owner = cx.entity().downgrade();
        let pointer_layers = std::array::from_fn(|index| {
            cx.new(|_| PointerLayer {
                index,
                guides: pointer_guides.clone(),
                owner: pointer_owner.clone(),
            })
        });
        let capture_owner = cx.entity().downgrade();
        let capture_list = cx.new(|cx| {
            CaptureListState::new(
                CaptureRows {
                    owner: capture_owner,
                },
                window,
                cx,
            )
            .selectable(false)
        });
        Self {
            capture_log: log::CaptureLogView::new(window, cx),
            root: None,
            catalog: Catalog::default(),
            suites: Vec::new(),
            active_suite: None,
            report: None,
            capture: None,
            visible_captures: Vec::new(),
            filter: 0,
            capture_list,
            image_mode: 3,
            preview_views: Default::default(),
            preview_scenario: None,
            visible: false,
            search,
            _subscription: subscription,
            focus: cx.focus_handle(),
            capture_focus: cx.focus_handle().tab_index(0),
            preview_focus: cx.focus_handle().tab_index(0),
            blend_focus: cx.focus_handle().tab_index(0),
            blend,
            _blend_subscription: blend_subscription,
            previews: Vec::new(),
            image_menu_epoch: 0,
            preview_textures: Default::default(),
            image_task: None,
            image_load_gate: Arc::default(),
            preview_scrolls: Default::default(),
            comparison_scroll: ScrollHandle::default(),
            pointer_guides,
            pointer_layers,
            status: "No reports loaded".into(),
            loading: false,
            image_loading: false,
            generation: 0,
            catalog_generation: 0,
            preferred_run: None,
            preferred_reports: None,
            approval_confirmation: false,
            approval_error: None,
            disabled: false,
        }
    }

    pub(super) fn refresh(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.refresh_reports(root, true, window, cx);
    }

    pub(super) fn refresh_existing(
        &mut self,
        root: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.refresh_reports(root, false, window, cx);
    }

    fn refresh_reports(
        &mut self,
        root: PathBuf,
        compare_missing: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.catalog_generation = self.catalog_generation.wrapping_add(1);
        self.capture_log.generation = self.capture_log.generation.wrapping_add(1);
        self.capture_log.page = None;
        self.capture_log.editor.update(cx, |state, cx| {
            state.set_value("Loading capture log...", window, cx)
        });
        let requested_run = self.preferred_run.clone();
        let requested_reports = self.preferred_reports.clone();
        let previous = self.selected_report().map(|report| {
            (
                report.path.clone(),
                self.capture
                    .and_then(|index| report.report.tests.get(index))
                    .filter(|selected| {
                        report
                            .report
                            .tests
                            .iter()
                            .filter(|test| test.pair.capture_key() == selected.pair.capture_key())
                            .count()
                            == 1
                    })
                    .map(|test| test.pair.capture_key().to_string()),
            )
        });
        self.generation += 1;
        let generation = self.generation;
        self.root = Some(root.clone());
        self.loading = true;
        self.visible_captures.clear();
        self.reset_previews(Some(window), cx);
        self.status = "Loading reports...".into();
        let suites = self.suites.clone();
        let task = cx.background_executor().spawn(async move {
            let selection = if let Some(targets) = &requested_reports {
                ReportSelection::Exact(targets)
            } else if let Some(run) = &requested_run {
                ReportSelection::Run(run)
            } else {
                ReportSelection::Latest
            };
            let mut catalog = if compare_missing {
                Catalog::load_selected(&root, selection)?
            } else {
                Catalog::load_existing_selected(&root, selection)?
            };
            catalog.resolve_names(&suites);
            Ok::<_, anyhow::Error>(catalog)
        });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |this, window, cx| {
                if this.generation != generation {
                    return;
                }
                this.loading = false;
                match result {
                    Ok(catalog) => {
                        this.catalog = catalog;
                        if this.active_suite.is_none() {
                            this.active_suite = this.catalog.reports.iter().find_map(|report| {
                                report.source.as_ref().map(|entry| entry.path.clone())
                            });
                        }
                        this.report = this.suite_report();
                        this.capture = None;
                        this.reset_capture_list(window, cx);
                        this.rebuild_capture_filter(cx);
                        this.status = format!(
                            "{} {}, {} {}",
                            this.catalog.reports.len(),
                            if this.catalog.reports.len() == 1 {
                                "report"
                            } else {
                                "reports"
                            },
                            this.catalog.warnings.len(),
                            if this.catalog.warnings.len() == 1 {
                                "warning"
                            } else {
                                "warnings"
                            }
                        );
                        if let Some(report) = this.selected_report()
                            && !report.report.tests.is_empty()
                        {
                            let capture = previous
                                .as_ref()
                                .filter(|(path, _)| *path == report.path)
                                .and_then(|(_, key)| key.as_ref())
                                .filter(|key| {
                                    report
                                        .report
                                        .tests
                                        .iter()
                                        .filter(|test| test.pair.capture_key() == key.as_str())
                                        .count()
                                        == 1
                                })
                                .and_then(|key| {
                                    report
                                        .report
                                        .tests
                                        .iter()
                                        .position(|test| test.pair.capture_key() == key)
                                })
                                .unwrap_or(0);
                            if this.visible_captures.contains(&capture) {
                                this.select_capture(capture, window, cx);
                            } else {
                                this.reconcile_capture(window, cx);
                            }
                        }
                    }
                    Err(error) => {
                        this.catalog = Catalog::default();
                        this.status = format!("{error:#}");
                    }
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn selected_report(&self) -> Option<&ReportEntry> {
        self.catalog.reports.get(self.report?).filter(|report| {
            report
                .source
                .as_ref()
                .is_some_and(|entry| self.active_suite.as_ref() == Some(&entry.path))
        })
    }

    fn selected_approval(&self) -> Option<ReportApproval> {
        let report = self.selected_report()?;
        let test = report.report.tests.get(self.capture?)?;
        let scenario_id = test.pair.scenario_id.as_ref().filter(|id| !id.is_empty())?;
        self.previews.get(1)?.result.as_ref().ok()?.path.as_ref()?;
        Some(ReportApproval {
            entry: report.source.clone()?,
            output: self.root.clone()?,
            suite_id: report.report.id.clone(),
            run_id: report.run_id.clone(),
            scenario_id: scenario_id.clone(),
            capture_key: test.pair.capture_key().to_string(),
        })
    }

    fn reset_capture_list(&self, window: &mut Window, cx: &mut Context<Self>) {
        self.capture_list.update(cx, |list, cx| {
            list.set_selected_index(None, window, cx);
            list.scroll_to_item(IndexPath::new(0), ScrollStrategy::Top, window, cx);
        });
    }

    fn scroll_capture_to(&self, position: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.capture_list.update(cx, |list, cx| {
            let index = IndexPath::new(position);
            list.set_selected_index(Some(index), window, cx);
            list.scroll_to_item(index, ScrollStrategy::Nearest, window, cx);
        });
    }

    /// Keep selection inside the current filter without reloading unchanged images.
    fn reconcile_capture(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading {
            return;
        }
        self.rebuild_capture_filter(cx);
        let filtered = &self.visible_captures;
        let selection = self
            .capture
            .filter(|capture| filtered.contains(capture))
            .or_else(|| filtered.first().copied());
        if let Some(position) =
            selection.and_then(|capture| filtered.iter().position(|index| *index == capture))
        {
            self.scroll_capture_to(position, window, cx);
        }
        if selection != self.capture {
            if let Some(capture) = selection {
                self.select_capture(capture, window, cx);
            } else {
                self.generation += 1;
                self.capture = None;
                self.reset_previews(Some(window), cx);
            }
        }
        cx.notify();
    }

    fn reset_previews(&mut self, mut window: Option<&mut Window>, cx: &mut App) {
        self.approval_confirmation = false;
        self.image_menu_epoch = self.image_menu_epoch.wrapping_add(1);
        self.image_task = None;
        self.image_loading = false;
        for preview in &self.previews {
            if let Ok(image) = &preview.result {
                image.image.clone().remove_asset(cx);
            }
        }
        for texture in &self.preview_textures {
            if let Some(image) = texture.0.borrow_mut().take() {
                cx.drop_image(image, window.as_deref_mut());
            }
        }
        self.previews.clear();
        for view in &mut self.preview_views {
            view.padding = None;
            view.horizontal_position = None;
            view.vertical_position = None;
        }
        self.pointer_guides.pan.borrow_mut().take();
        self.pointer_guides.active.set(None);
        *self.pointer_guides.alignment.borrow_mut() = None;
        for geometry in &self.pointer_guides.geometry {
            *geometry.borrow_mut() = None;
        }
        for bounds in &self.pointer_guides.bounds {
            bounds.set(None);
        }
    }

    fn select_capture(&mut self, capture: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.loading
            || (self.capture == Some(capture) && (self.image_loading || !self.previews.is_empty()))
        {
            return;
        }
        let Some(report) = self.selected_report() else {
            return;
        };
        let Some(test) = report.report.tests.get(capture) else {
            return;
        };
        let Some(root) = self.root.clone() else {
            return;
        };
        let scenario = (
            report.path.clone(),
            test.pair
                .scenario_id
                .as_ref()
                .filter(|id| !id.is_empty())
                .map(|id| format!("scenario:{id}"))
                .unwrap_or_else(|| format!("capture:{capture}")),
        );
        let artifacts = Arc::new(report.artifacts());
        let alignment = test
            .pair
            .diff
            .as_ref()
            .and_then(|diff| diff.alignment.clone())
            .filter(DiffAlignment::is_valid);
        let images = [
            ("Reference", test.pair.reference.clone()),
            ("Test", test.pair.test.clone()),
            ("Diff", test.pair.diff_image.clone()),
        ];
        if self.preview_scenario.as_ref() != Some(&scenario) {
            self.image_mode = 3;
            self.reset_view();
            self.preview_scenario = Some(scenario);
        }
        self.reset_previews(Some(window), cx);
        self.comparison_scroll.set_offset(point(px(0.), px(0.)));
        for scroll in &self.preview_scrolls {
            scroll.set_offset(point(px(0.), px(0.)));
        }
        if let Some(position) = self
            .visible_captures
            .iter()
            .position(|index| *index == capture)
        {
            self.scroll_capture_to(position, window, cx);
        }
        self.capture = Some(capture);
        if self.capture_log.active {
            self.capture_log.query.offset = 0;
            self.load_capture_log(window, cx);
        }
        self.image_loading = true;
        self.generation += 1;
        let generation = self.generation;
        let load_gate = self.image_load_gate.clone();
        self.image_task = Some(cx.spawn_in(window, async move |view, cx| {
            let mut previews = Vec::with_capacity(images.len());
            for (label, path) in images {
                let root = root.clone();
                let artifacts = artifacts.clone();
                let gate = load_gate.clone();
                let task = cx.background_executor().spawn(async move {
                    let _permit = gate.lock().await;
                    let result = LoadedImage::load(&artifacts, &root, label, path.as_deref())
                        .map_err(|error| format!("{error:#}"));
                    Preview { label, result }
                });
                previews.push(task.await);
            }
            let _ = view.update_in(cx, |this, _, cx| {
                if this.generation == generation {
                    this.previews = previews;
                    if this.image_mode >= 4 && !this.can_blend() {
                        this.select_image_mode(3, cx);
                    }
                    *this.pointer_guides.alignment.borrow_mut() = alignment;
                    this.image_loading = false;
                    cx.notify();
                }
            });
        }));
        cx.notify();
    }

    fn rebuild_capture_filter(&mut self, cx: &App) {
        let query = self.search.read(cx).value().trim().to_lowercase();
        self.visible_captures = self
            .selected_report()
            .map(|report| {
                report
                    .report
                    .tests
                    .iter()
                    .enumerate()
                    .filter(|(_, test)| {
                        let status = match self.filter {
                            1 => test.outcome() != Outcome::Passed,
                            2 => test.outcome() == Outcome::Passed,
                            _ => true,
                        };
                        status && (query.is_empty() || test.pair.matches(&query))
                    })
                    .map(|(index, _)| index)
                    .collect()
            })
            .unwrap_or_default();
    }

    fn open_external(
        &mut self,
        target: Result<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let target = match target {
            Ok(path) => path,
            Err(error) => {
                self.status = format!("{error:#}");
                cx.notify();
                return;
            }
        };
        let task = cx
            .background_executor()
            .spawn(async move { open::that(target) });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |this, _, cx| {
                if let Err(error) = result {
                    this.status = format!("Cannot open artifact: {error}");
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn build_image_menu(
        &mut self,
        index: usize,
        menu: PopupMenu,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> PopupMenu {
        let Some(image) = self
            .previews
            .get(index)
            .and_then(|preview| preview.result.as_ref().ok())
        else {
            return menu;
        };
        let Some(path) = &image.path else {
            return menu;
        };
        let viewer_path = path.clone();
        let reveal_path = path.clone();
        let browser_url = reqwest::Url::from_file_path(path).ok();
        let page_url = self.selected_report().and_then(|report| {
            self.capture
                .and_then(|capture| report.report.tests.get(capture))
                .and_then(|test| image_page_url(&test.pair, index))
        });
        let browser_disabled = browser_url.is_none();
        let page_disabled = page_url.is_none();
        let copy_url = page_url.clone();
        let url_kind = if index == 0 { "Reference" } else { "Test" };
        let browser = cx.listener(move |this, _, window, cx| {
            if let Some(url) = &browser_url {
                this.open_browser(url.clone(), window, cx);
            }
        });
        let viewer = cx.listener(move |this, _, window, cx| {
            this.open_external(Ok(viewer_path.clone()), window, cx);
        });
        let page = cx.listener(move |this, _, window, cx| {
            if let Some(url) = &page_url {
                this.open_browser(url.clone(), window, cx);
            }
        });
        let focus = window.focused(cx).unwrap_or_else(|| self.focus.clone());
        menu.action_context(focus)
            .item(
                PopupMenuItem::new("Open in default browser")
                    .disabled(browser_disabled)
                    .on_click(browser),
            )
            .item(PopupMenuItem::new("Open in default image viewer").on_click(viewer))
            .item(
                PopupMenuItem::new("Show in file manager")
                    .on_click(move |_, _, cx| cx.reveal_path(&reveal_path)),
            )
            .when(index < 2, |menu| {
                menu.separator()
                    .item(
                        PopupMenuItem::new(format!("Open {url_kind} URL"))
                            .disabled(page_disabled)
                            .on_click(page),
                    )
                    .item(
                        PopupMenuItem::new(format!("Copy {url_kind} URL"))
                            .disabled(page_disabled)
                            .on_click(move |_, _, cx| {
                                if let Some(url) = &copy_url {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        url.to_string(),
                                    ));
                                }
                            }),
                    )
            })
    }

    fn open_browser(&mut self, url: reqwest::Url, window: &mut Window, cx: &mut Context<Self>) {
        let task = cx
            .background_executor()
            .spawn(async move { webbrowser::open(url.as_str()) });
        cx.spawn_in(window, async move |view, cx| {
            let result = task.await;
            let _ = view.update_in(cx, |this, _, cx| {
                if let Err(error) = result {
                    this.status = format!("Cannot open browser: {error}");
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn render_captures(&self, _: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let filtered = &self.visible_captures;
        let list = div()
            .id("capture-list")
            .test_support()
            .flex()
            .flex_col()
            .gap_1()
            .p_3()
            .size_full()
            .min_w_0()
            .min_h_0()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .pb_2()
                    .child("Captures"),
            )
            .child(
                Input::new(&self.search)
                    .id("capture-search")
                    .prefix(IconName::Search)
                    .cleanable(true),
            )
            .child(
                TabBar::new("result-filter")
                    .segmented()
                    .selected_index(self.filter)
                    .on_click(cx.listener(|this, index, window, cx| {
                        this.filter = *index;
                        this.reconcile_capture(window, cx);
                    }))
                    .child(Tab::new().label("All"))
                    .child(Tab::new().label("Failed"))
                    .child(Tab::new().label("Passed")),
            );
        let mut rows = div()
            .id("capture-scroll")
            .test_support()
            .key_context("RegressifyList")
            .track_focus(&self.capture_focus)
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| {
                    window.focus(&this.capture_focus, cx);
                }),
            )
            .on_action(
                cx.listener(|this, action: &lists::MoveListSelection, window, cx| {
                    if this.loading {
                        return;
                    }
                    let current = this.capture.and_then(|capture| {
                        this.visible_captures
                            .iter()
                            .position(|index| *index == capture)
                    });
                    if let Some(position) = action.index(current, this.visible_captures.len()) {
                        this.select_capture(this.visible_captures[position], window, cx);
                    }
                }),
            )
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .relative();
        if filtered.is_empty() {
            let message = if self.loading {
                "Loading captures..."
            } else if self.active_suite.is_none() {
                "Select a test suite"
            } else if self.selected_report().is_none() {
                "No captures for this suite"
            } else {
                "No matching captures"
            };
            let empty = if self.loading {
                div()
                    .py_6()
                    .text_center()
                    .text_color(cx.theme().muted_foreground)
                    .child(message)
                    .into_any_element()
            } else {
                gpui_kit::component::empty::Empty::new()
                    .text_color(cx.theme().muted_foreground)
                    .child(message)
                    .into_any_element()
            };
            rows = rows.child(
                div()
                    .id("capture-empty")
                    .role(Role::Status)
                    .aria_label(message)
                    .test_support()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .w_full()
                    .child(empty),
            );
        }
        if !filtered.is_empty() {
            rows = rows.child(gpui_kit::component::list::List::new(&self.capture_list));
        }
        list.child(rows).into_any_element()
    }

    fn render_capture_row(
        &self,
        index: usize,
        owner: WeakEntity<Self>,
        cx: &App,
    ) -> Option<gpui_kit::component::list::ListItem> {
        let report = self.selected_report()?;
        let test = report.report.tests.get(index)?;
        let path = report.path.clone();
        let root = self.root.clone();
        let key = test.pair.capture_key().to_string();
        let catalog_generation = self.catalog_generation;
        let duplicate_key = report
            .report
            .tests
            .iter()
            .filter(|test| test.pair.capture_key() == key)
            .count()
            > 1;
        let identity = if duplicate_key {
            format!(
                "{}:{key}:duplicate-{catalog_generation}-{index}",
                display_path(&path)
            )
        } else {
            format!("{}:{key}", display_path(&path))
        };
        let outcome = test.outcome();
        let color = match outcome {
            Outcome::Passed => cx.theme().success,
            Outcome::Mismatch => cx.theme().warning,
            _ => cx.theme().danger,
        };
        let tooltip = test
            .pair
            .engine_error_msg
            .clone()
            .map(|error| format!("{error}\n{key}"))
            .unwrap_or_else(|| {
                format!(
                    "{}\n{}\n{key}",
                    test.pair.display.title, test.pair.display.detail
                )
            });
        Some(
            gpui_kit::component::list::ListItem::new(SharedString::from(identity))
                .w_full()
                .justify_start()
                .selected(self.capture == Some(index))
                .accessibility_label(format!("{} {}", outcome.label(), test.pair.display.title))
                .child(
                    div()
                        .id(("capture", index))
                        .role(Role::ListBoxOption)
                        .aria_label(format!("{} {}", outcome.label(), test.pair.display.title))
                        .test_support()
                        .flex()
                        .items_center()
                        .gap_3()
                        .w_full()
                        .min_w_0()
                        .child(
                            Icon::new(match outcome {
                                Outcome::Passed => gpui_kit::assets::IconName::CircleCheck,
                                Outcome::Mismatch => gpui_kit::assets::IconName::Split,
                                _ => gpui_kit::assets::IconName::CircleAlert,
                            })
                            .small()
                            .text_color(color),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .text_left()
                                .child(
                                    gpui_kit::component::label::Label::new(
                                        test.pair.display.title.clone(),
                                    )
                                    .secondary(if test.pair.display.detail.is_empty() {
                                        outcome.label().to_string()
                                    } else {
                                        format!(
                                            "{} / {}",
                                            outcome.label(),
                                            test.pair.display.detail
                                        )
                                    })
                                    .truncate(),
                                ),
                        ),
                )
                .tooltip(move |window, cx| {
                    gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)
                })
                .text_color(color)
                .on_click(move |_, window, cx| {
                    let _ = owner.update(cx, |this, cx| {
                        if this.root != root
                            || this.loading
                            || this.catalog_generation != catalog_generation
                        {
                            return;
                        }
                        let selected = this
                            .selected_report()
                            .filter(|report| report.path == path)
                            .and_then(|report| report.report.tests.get(index))
                            .filter(|test| test.pair.capture_key() == key)
                            .map(|_| index)
                            .filter(|capture| this.visible_captures.contains(capture));
                        if let Some(capture) = selected {
                            window.focus(&this.capture_focus, cx);
                            this.select_capture(capture, window, cx);
                        }
                    });
                }),
        )
    }

    fn comparison_preview_count(&self) -> usize {
        if self
            .previews
            .get(2)
            .is_some_and(|preview| preview.result.is_ok())
        {
            3
        } else {
            2
        }
    }

    fn can_blend(&self) -> bool {
        self.previews.len() >= 2
            && self.previews[..2].iter().all(|preview| {
                preview
                    .result
                    .as_ref()
                    .is_ok_and(|image| image.path.is_some())
            })
    }

    fn image_scroll(&self) -> &ScrollHandle {
        if self.image_mode == 3 {
            &self.comparison_scroll
        } else {
            &self.preview_scrolls[self.image_mode]
        }
    }

    fn preview_canvas_size(&self, index: usize) -> (usize, usize) {
        let images = if self.image_mode == 3 {
            &self.previews[..self.previews.len().min(self.comparison_preview_count())]
        } else if self.image_mode >= 4 {
            &self.previews[..self.previews.len().min(2)]
        } else {
            self.previews
                .get(index)
                .map(std::slice::from_ref)
                .unwrap_or_default()
        };
        images
            .iter()
            .filter_map(|preview| preview.result.as_ref().ok())
            .fold((1, 1), |(width, height), image| {
                (width.max(image.width), height.max(image.height))
            })
    }

    fn zoom_preview_index(&self) -> usize {
        if self.image_mode >= 3 {
            self.previews
                .iter()
                .take(self.comparison_preview_count())
                .position(|preview| preview.result.is_ok())
                .unwrap_or(0)
        } else {
            self.image_mode
        }
    }

    fn render_zoom_value(&self) -> AnyElement {
        let view = self.preview_views[self.image_mode];
        let index = self.zoom_preview_index();
        let canvas_width = self.preview_canvas_size(index).0 as f32;
        let guides = self.pointer_guides.clone();
        div()
            .w(px(56.))
            .self_stretch()
            .flex_shrink_0()
            // Fitted bounds are only current after the image panes have been laid out.
            .child(gpui_kit::deferred(
                canvas(
                    move |bounds, window, cx| {
                        let zoom = if view.mode == ZoomMode::Manual {
                            view.zoom
                        } else {
                            guides.bounds[index]
                                .get()
                                .map(|bounds| bounds.size.width / px(canvas_width))
                                .unwrap_or(view.zoom)
                        };
                        let label = format!("{:.1}%", zoom * 100.);
                        let mut display = div()
                            .id("zoom-value")
                            .role(Role::Status)
                            .aria_label(label.clone())
                            .test_support()
                            .size_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(label)
                            .into_any_element();
                        display.prepaint_as_root(
                            bounds.origin,
                            bounds.size.map(gpui_kit::AvailableSpace::Definite),
                            window,
                            cx,
                        );
                        display
                    },
                    |_, mut display, window, cx| display.paint(window, cx),
                )
                .size_full(),
            ))
            .into_any_element()
    }

    fn step_zoom(&mut self, increase: bool, cx: &mut Context<Self>) {
        let index = self.zoom_preview_index();
        let view = self.preview_views[self.image_mode];
        let current = if view.mode != ZoomMode::Manual {
            self.pointer_guides.bounds[index]
                .get()
                .map(|bounds| bounds.size.width / px(self.preview_canvas_size(index).0 as f32))
                .unwrap_or(view.zoom)
        } else {
            view.zoom
        };
        let zoom = next_zoom(current, increase);
        if zoom == current {
            return;
        }
        self.preview_views[self.image_mode] = PreviewView {
            mode: ZoomMode::Manual,
            zoom,
            horizontal_position: view.horizontal_position,
            vertical_position: view.vertical_position,
            padding: None,
        };
        self.pointer_guides.pan.borrow_mut().take();
        cx.notify();
    }

    fn zoom_at_pointer(
        &mut self,
        index: usize,
        event: &gpui_kit::ScrollWheelEvent,
        cx: &mut Context<Self>,
    ) {
        if !event.modifiers.control && !event.modifiers.platform {
            return;
        }
        cx.stop_propagation();
        let steps = event.delta.pixel_delta(px(40.)).y / px(40.);
        if steps == 0. || !steps.is_finite() {
            return;
        }
        let Some(bounds) = self.pointer_guides.bounds[index].get() else {
            return;
        };
        if bounds.size.width <= px(0.) || bounds.size.height <= px(0.) {
            return;
        }
        let (width, height) = self.preview_canvas_size(index);
        let view = self.preview_views[self.image_mode];
        let scale = if view.mode != ZoomMode::Manual {
            bounds.size.width / px(width as f32)
        } else {
            view.zoom
        };
        let zoom = next_zoom(scale, steps > 0.);
        if zoom == scale {
            return;
        }
        let next_size = size(px(width as f32 * zoom), px(height as f32 * zoom));
        let scroll = self.image_scroll().clone();
        let viewport = scroll.bounds().size;
        let padding = view.padding.filter(|_| view.mode == ZoomMode::Manual);
        let horizontal_space = (viewport.width - bounds.size.width).max(px(0.));
        let vertical_space = (viewport.height - bounds.size.height).max(px(0.));
        let padding = point(
            view.horizontal_position
                .map(|position| horizontal_space * position)
                .or_else(|| padding.map(|padding| padding.left))
                .unwrap_or(horizontal_space / 2.),
            view.vertical_position
                .map(|position| vertical_space * position)
                .or_else(|| padding.map(|padding| padding.top))
                .unwrap_or(px(0.)),
        );
        let origin = bounds.origin - scroll.offset() - padding;
        let anchored = event.position
            - origin
            - point(
                (event.position.x - bounds.origin.x) * (next_size.width / bounds.size.width),
                (event.position.y - bounds.origin.y) * (next_size.height / bounds.size.height),
            );
        self.preview_views[self.image_mode] = PreviewView {
            mode: ZoomMode::Manual,
            zoom,
            horizontal_position: None,
            vertical_position: None,
            padding: Some(gpui_kit::Edges {
                left: anchored.x.max(px(0.)),
                top: anchored.y.max(px(0.)),
                right: (viewport.width - anchored.x - next_size.width).max(px(0.)),
                bottom: (viewport.height - anchored.y - next_size.height).max(px(0.)),
            }),
        };
        scroll.set_offset(point(anchored.x.min(px(0.)), anchored.y.min(px(0.))));
        self.pointer_guides.pan.borrow_mut().take();
        cx.notify();
    }

    fn render_preview(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(preview) = self.previews.get(index) else {
            if self.image_loading {
                return div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .p_4()
                    .child("Loading image...")
                    .into_any_element();
            }
            return div()
                .id(("capture-preview-empty", index))
                .role(Role::Status)
                .aria_label("No capture selected")
                .test_support()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(
                    gpui_kit::component::empty::Empty::new()
                        .size_full()
                        .child("No capture selected"),
                )
                .into_any_element();
        };
        let content = match &preview.result {
            Ok(image) => {
                let view = self.preview_views[self.image_mode];
                let horizontal_position = view.horizontal_position.unwrap_or(0.5);
                let side_by_side = self.image_mode == 3;
                let (canvas_width, canvas_height) = self.preview_canvas_size(index);
                let gaps = self
                    .pointer_guides
                    .alignment
                    .borrow()
                    .as_ref()
                    .and_then(|alignment| {
                        let diff = self.previews.get(2)?.result.as_ref().ok()?;
                        match index {
                            0 => alignment.reference_for_image(image.height, diff.height),
                            1 => alignment.test_for_image(image.height, diff.height),
                            _ => None,
                        }
                    })
                    .unwrap_or_default();
                let gap = gaps.first().copied();
                *self.pointer_guides.geometry[index].borrow_mut() =
                    image.path.is_some().then_some(PointerGeometry {
                        canvas: (canvas_width, canvas_height),
                        gap,
                        gaps,
                    });
                let scroll = self.image_scroll();
                let blend = self.blend.read(cx).value().end() / 100.;
                let mut layers = div().absolute().top_0().left_0().size_full().child(
                    img(self.preview_textures[index].source(image.image.clone()))
                        .absolute()
                        .top_0()
                        .left_0()
                        .w(relative(image.width as f32 / canvas_width as f32))
                        .h(relative(image.height as f32 / canvas_height as f32))
                        .object_fit(ObjectFit::Contain),
                );
                if self.image_mode >= 4
                    && let Some(current) = self
                        .previews
                        .get(1)
                        .and_then(|preview| preview.result.as_ref().ok())
                {
                    let visible = if self.image_mode == 4 { 1. - blend } else { 1. };
                    if visible > 0. {
                        // Clip the container, keeping both images at the same pixel scale and origin.
                        layers = layers.child(
                            div()
                                .id("comparison-test-layer")
                                .test_support()
                                .absolute()
                                .top_0()
                                .right_0()
                                .h_full()
                                .w(relative(visible))
                                .overflow_hidden()
                                .opacity(if self.image_mode == 5 { blend } else { 1. })
                                .child(
                                    div()
                                        .absolute()
                                        .top_0()
                                        .right_0()
                                        .h_full()
                                        .w(relative(1. / visible))
                                        .child(
                                            img(self.preview_textures[1]
                                                .source(current.image.clone()))
                                            .absolute()
                                            .top_0()
                                            .left_0()
                                            .w(relative(current.width as f32 / canvas_width as f32))
                                            .h(relative(
                                                current.height as f32 / canvas_height as f32,
                                            ))
                                            .object_fit(ObjectFit::Contain),
                                        ),
                                ),
                        );
                    }
                    if self.image_mode == 4 {
                        layers = layers.child(
                            div()
                                .id("comparison-wipe-divider")
                                .test_support()
                                .absolute()
                                .top_0()
                                .left(relative(blend))
                                .h_full()
                                .w(px(1.))
                                .bg(rgba(0xffffffff))
                                .border_l_1()
                                .border_color(rgba(0x000000b3)),
                        );
                    }
                }
                let geometry = self.pointer_guides.clone();
                let display = div()
                    .id(("image-content", index))
                    .role(if self.image_mode == 3 {
                        Role::Button
                    } else {
                        Role::Group
                    })
                    .aria_label(format!("{} image canvas", preview.label))
                    .test_support()
                    .relative()
                    .child(layers)
                    .cursor(
                        if self
                            .pointer_guides
                            .pan
                            .borrow()
                            .as_ref()
                            .is_some_and(|pan| pan.horizontal || pan.vertical)
                        {
                            CursorStyle::ClosedHand
                        } else if self.pointer_guides.bounds[index]
                                .get()
                                .is_some_and(|bounds| {
                                    bounds.size.width > scroll.bounds().size.width
                                        || bounds.size.height > scroll.bounds().size.height
                                        || (side_by_side && (bounds.size.width < scroll.bounds().size.width
                                            || bounds.size.height < scroll.bounds().size.height))
                                })
                        {
                            CursorStyle::OpenHand
                        } else if self.image_mode == 3 {
                            CursorStyle::PointingHand
                        } else {
                            CursorStyle::Crosshair
                        },
                    )
                    .on_mouse_down(MouseButton::Left, {
                        let guides = self.pointer_guides.clone();
                        let scroll = scroll.clone();
                        let focus = self.preview_focus.clone();
                        move |event, window, cx| {
                            let Some(bounds) = guides.bounds[index].get() else {
                                return;
                            };
                            let viewport = scroll.bounds().size;
                            let horizontal_space = if side_by_side {
                                (viewport.width - bounds.size.width).max(px(0.))
                            } else {
                                px(0.)
                            };
                            let horizontal_origin = match (
                                view.horizontal_position,
                                view.padding.filter(|_| view.mode == ZoomMode::Manual),
                            ) {
                                (Some(position), _) => horizontal_space * position,
                                (None, Some(padding)) => padding.left + scroll.offset().x,
                                _ => horizontal_space / 2.,
                            };
                            let horizontal = bounds.size.width > viewport.width || horizontal_space > px(0.);
                            let vertical_space = if side_by_side {
                                (viewport.height - bounds.size.height).max(px(0.))
                            } else {
                                px(0.)
                            };
                            let vertical = bounds.size.height > viewport.height || vertical_space > px(0.);
                            *guides.pan.borrow_mut() = Some(PreviewPan {
                                position: event.position,
                                offset: scroll.offset(),
                                scroll: scroll.clone(),
                                horizontal,
                                horizontal_origin,
                                horizontal_space,
                                vertical,
                                vertical_origin: bounds.top() - scroll.bounds().top(),
                                vertical_space,
                                dragged: false,
                            });
                            guides.active.set(None);
                            window.focus(&focus, cx);
                            window.refresh();
                            window.prevent_default();
                            cx.stop_propagation();
                        }
                    })
                    .on_click(cx.listener(move |this, event: &gpui_kit::ClickEvent, _, cx| {
                        // Mixed-button releases must not turn a context-menu gesture into pane activation.
                        if matches!(event, gpui_kit::ClickEvent::Mouse(click)
                            if click.down.button != MouseButton::Left || click.up.button != MouseButton::Left) {
                            return;
                        }
                        if this.image_mode == 3 {
                            this.select_image_mode(index, cx);
                        } else if matches!(event, gpui_kit::ClickEvent::Mouse(click) if click.up.click_count == 2) {
                            let view = this.preview_views[this.image_mode];
                            if view.mode == ZoomMode::Manual && view.zoom == 1. {
                                this.set_zoom_mode(ZoomMode::FitArea, cx);
                            } else {
                                this.actual_size(cx);
                            }
                        }
                    }))
                    .on_mouse_move(cx.listener(move |this, event: &gpui_kit::MouseMoveEvent, window, cx| {
                            if this.image_mode == 4 && event.pressed_button.is_none()
                                && let Some(bounds) = this.pointer_guides.bounds[index].get()
                                && bounds.size.width > px(0.)
                            {
                                let value = (((event.position.x - bounds.left()) / bounds.size.width) * 100.).round().clamp(0., 100.);
                                if this.blend.read(cx).value().end() != value {
                                    this.blend.update(cx, |state, cx| state.set_value(value, window, cx));
                                }
                            }
                    }))
                    .child(
                        canvas(
                            move |bounds, _, _| geometry.bounds[index].set(Some(bounds)),
                            |_, _, _, _| {},
                        )
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full(),
                    );
                let display = if view.mode == ZoomMode::FitArea {
                    div().size_full().child(
                        canvas(
                            move |bounds, window, cx| {
                                let scale = (bounds.size.width / px(canvas_width as f32))
                                    .min(bounds.size.height / px(canvas_height as f32))
                                    .min(1.0);
                                let mut display = display
                                    .w(px(canvas_width as f32 * scale))
                                    .h(px(canvas_height as f32 * scale))
                                    .into_any_element();
                                display.prepaint_as_root(
                                    bounds.origin
                                        + point(
                                            (bounds.size.width - px(canvas_width as f32 * scale))
                                                * horizontal_position,
                                            (bounds.size.height - px(canvas_height as f32 * scale))
                                                * view.vertical_position.unwrap_or(0.),
                                        ),
                                    bounds.size.map(gpui_kit::AvailableSpace::Definite),
                                    window,
                                    cx,
                                );
                                display
                            },
                            |_, mut display, window, cx| display.paint(window, cx),
                        )
                        .size_full(),
                    )
                } else if view.mode == ZoomMode::FitWidth {
                    div()
                        .w_full()
                        .flex()
                        .items_start()
                        .child(div().flex_basis(px(0.)).flex_grow(horizontal_position))
                        .child(
                            display
                                .w_full()
                                .max_w(px(canvas_width as f32))
                                .aspect_ratio(canvas_width as f32 / canvas_height as f32)
                                .flex_shrink_0(),
                        )
                        .child(div().flex_basis(px(0.)).flex_grow(1. - horizontal_position))
                } else {
                    let width = px(canvas_width as f32 * view.zoom);
                    let height = px(canvas_height as f32 * view.zoom);
                    let display = display.w(width).h(height).flex_shrink_0();
                    let padding = view.padding.map(|mut padding| {
                        if view.vertical_position.is_some() {
                            padding.top = px(0.);
                            padding.bottom = px(0.);
                        }
                        padding
                    });
                    if let Some(padding) = padding.filter(|_| view.horizontal_position.is_none()) {
                        div()
                            .relative()
                            .w(width + padding.left + padding.right)
                            .h(height + padding.top + padding.bottom)
                            .child(display.absolute().left(padding.left).top(padding.top))
                    } else {
                        let top = padding.map_or(px(0.), |padding| padding.top);
                        let bottom = padding.map_or(px(0.), |padding| padding.bottom);
                        div()
                            .w(width)
                            .min_w_full()
                            .h(height + top + bottom)
                            .flex()
                            .child(div().flex_basis(px(0.)).flex_grow(horizontal_position))
                            .child(display.mt(top))
                            .child(div().flex_basis(px(0.)).flex_grow(1. - horizontal_position))
                    }
                }
                .map(|display| {
                    if view.mode != ZoomMode::FitArea
                        && let Some(position) = view.vertical_position
                    {
                        div()
                            .w_full()
                            .flex()
                            .flex_col()
                            .child(div().flex_basis(px(0.)).flex_grow(position))
                            .child(display.flex_shrink_0())
                            .child(div().flex_basis(px(0.)).flex_grow(1. - position))
                    } else {
                        display
                    }
                })
                .id(("image-surface", index))
                .min_h_full()
                .on_scroll_wheel(cx.listener(move |this, event, _, cx| {
                    this.zoom_at_pointer(index, event, cx);
                }));
                let viewport = div()
                    .id(SharedString::from(format!(
                        "image-menu-{}-{index}",
                        self.image_menu_epoch
                    )))
                    .size_full()
                    .relative()
                    .overflow_scroll()
                    .map(|mut viewport| {
                        viewport.style().allow_concurrent_scroll = Some(true);
                        viewport
                    })
                    .track_scroll(scroll)
                    .bg(cx.theme().muted)
                    .on_mouse_move(cx.listener(
                        move |this, event: &gpui_kit::MouseMoveEvent, _, cx| {
                            this.pointer_guides.active.set(
                                (this.image_mode == 3
                                    && event.pressed_button.is_none()
                                    && this.pointer_guides.pan.borrow().is_none())
                                .then_some(index),
                            );
                            for layer in &this.pointer_layers {
                                layer.update(cx, |_, cx| cx.notify());
                            }
                        },
                    ))
                    .on_hover({
                        let guides = self.pointer_guides.clone();
                        let layers = self.pointer_layers.clone();
                        move |hovered, _, cx| {
                            let active = guides
                                .active
                                .get()
                                .filter(|active| *hovered || *active != index);
                            if guides.active.replace(active) != active {
                                for layer in &layers {
                                    layer.update(cx, |_, cx| cx.notify());
                                }
                            }
                        }
                    })
                    .capture_any_mouse_down({
                        let guides = self.pointer_guides.clone();
                        move |event, window, _| {
                            if event.button == MouseButton::Right {
                                guides.pan.borrow_mut().take();
                                window.prevent_default();
                            }
                        }
                    })
                    .child(display);
                let viewport = if image.path.is_some() {
                    use gpui_kit::component::menu::ContextMenuExt;
                    let owner = cx.entity().downgrade();
                    let epoch = self.image_menu_epoch;
                    viewport
                        .context_menu(move |menu, window, cx| {
                            let Some(panel) = owner.upgrade() else {
                                return menu;
                            };
                            if panel.read(cx).image_menu_epoch != epoch {
                                return menu;
                            }
                            let restore = window
                                .focused(cx)
                                .unwrap_or_else(|| panel.read(cx).focus.clone());
                            design::restore_managed_menu_focus(
                                owner.clone(),
                                menu.focus_handle(cx),
                                restore,
                                |this| {
                                    this.image_menu_epoch = this.image_menu_epoch.wrapping_add(1)
                                },
                                window,
                                cx,
                            );
                            panel.update(cx, |this, cx| {
                                this.build_image_menu(index, menu, window, cx)
                            })
                        })
                        .into_any_element()
                } else {
                    viewport.into_any_element()
                };
                div()
                    .id(("image-details", index))
                    .role(Role::Group)
                    .aria_label(format!(
                        "{} {} x {}",
                        preview.label, image.width, image.height
                    ))
                    .test_support()
                    .size_full()
                    .min_h_0()
                    .relative()
                    .child(
                        div()
                            .id(("image-scroll", index))
                            .role(Role::Group)
                            .aria_label(format!("{} image preview", preview.label))
                            .test_support()
                            .size_full()
                            .child(viewport),
                    )
                    .child(
                        div()
                            .id(("image-pointer-layer", index))
                            .test_support()
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .overflow_hidden()
                            .child(self.pointer_layers[index].clone()),
                    )
                    .scrollbar(scroll, scroll::ScrollbarAxis::Both)
                    .into_any_element()
            }
            Err(error) => div()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap_3()
                .p_4()
                .size_full()
                .child(preview.label)
                .child(
                    div()
                        .text_sm()
                        .text_color(cx.theme().muted_foreground)
                        .child(error.clone()),
                )
                .into_any_element(),
        };
        div()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(content)
            .into_any_element()
    }

    fn render_capture_information(&self, pair: &Pair, cx: &mut Context<Self>) -> AnyElement {
        let pair = pair.clone();
        let panel = cx.entity().downgrade();
        HoverCard::new("capture-information-card")
            .anchor(gpui_kit::Anchor::TopLeft)
            .open_delay(std::time::Duration::from_millis(200))
            .close_delay(std::time::Duration::from_millis(300))
            .trigger(
                Button::new("capture-information")
                    .ghost()
                    .icon(gpui_kit::assets::IconName::Info)
                    .accessibility_label("Scenario information"),
            )
            .content(move |_, window, cx| {
                let difference = pair.diff.as_ref();
                let dimensions = difference.and_then(|diff| diff.dimension_difference.as_ref());
                let percentage = difference
                    .and_then(|diff| diff.formatted_mismatch())
                    .unwrap_or_else(|| "Unavailable".into());
                let content = div()
                    .id("capture-information-content")
                    .role(Role::Group)
                    .aria_label("Scenario information details")
                    .test_support()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .w(px(720.))
                    .max_w(window.bounds().size.width - px(48.))
                    .text_sm();
                let selector = if pair.selector.is_empty() {
                    "Unavailable".to_string()
                } else {
                    pair.selector.clone()
                };
                let mut details = gpui_kit::component::description_list::DescriptionList::new()
                    .columns(2)
                    .bordered(false)
                    .label_width(px(112.));
                for (id, visible_label, aria_label, value) in [
                    (
                        "capture-info-label",
                        "Label:",
                        format!("Label: {}", pair.display.title),
                        pair.display.title.clone(),
                    ),
                    (
                        "capture-info-selector",
                        "Selector:",
                        format!("Selector: {selector}"),
                        selector,
                    ),
                ] {
                    details = details.item(
                        div().text_sm().child(visible_label).into_any_element(),
                        div()
                            .id(id)
                            .role(Role::Status)
                            .aria_label(aria_label)
                            .test_support()
                            .min_w_0()
                            .flex_shrink_1()
                            .truncate()
                            .child(value)
                            .into_any_element(),
                        2,
                    );
                }
                for (index, kind) in [(0, "Reference"), (1, "Test")] {
                    let url = image_page_url(&pair, index);
                    let same = index == 1 && url.is_some() && url == image_page_url(&pair, 0);
                    let value = if same {
                        "Same URL".into()
                    } else {
                        url.as_ref()
                            .map(|url| url.as_str().to_string())
                            .unwrap_or_else(|| "Unavailable".into())
                    };
                    let link_url = url.clone();
                    let copy_url = url.clone();
                    let panel = panel.clone();
                    let label = div()
                        .id(("capture-info-url-label", index))
                        .role(Role::Status)
                        .aria_label(format!("{kind} URL:"))
                        .test_support()
                        .flex_shrink_0()
                        .w_full()
                        .h_full()
                        .flex()
                        .items_center()
                        .child(format!("{kind} URL:"))
                        .into_any_element();
                    let value = div()
                        .flex()
                        .items_center()
                        .gap_1()
                        .min_w_0()
                        .child(
                            Button::new(("capture-info-url", index))
                                .ghost()
                                .flex_shrink_1()
                                .min_w_0()
                                .justify_start()
                                .text_left()
                                .cursor(if url.is_some() {
                                    CursorStyle::PointingHand
                                } else {
                                    CursorStyle::Arrow
                                })
                                .accessibility_label(format!("Open {kind} URL"))
                                .tooltip(
                                    url.as_ref()
                                        .map(|url| url.as_str())
                                        .unwrap_or("URL unavailable")
                                        .to_string(),
                                )
                                .disabled(url.is_none())
                                .child(
                                    div()
                                        .id(("capture-info-url-value", index))
                                        .role(Role::Status)
                                        .aria_label(value.clone())
                                        .test_support()
                                        .min_w_0()
                                        .truncate()
                                        .text_color(cx.theme().primary)
                                        .underline()
                                        .child(value),
                                )
                                .on_click(move |_, window, cx| {
                                    if let Some(url) = &link_url {
                                        let _ = panel.update(cx, |this, cx| {
                                            this.open_browser(url.clone(), window, cx)
                                        });
                                    }
                                }),
                        )
                        .child(
                            Button::new(("capture-info-copy-url", index))
                                .ghost()
                                .icon(IconName::Copy)
                                .tooltip(format!("Copy {kind} URL"))
                                .accessibility_label(format!("Copy {kind} URL"))
                                .disabled(url.is_none())
                                .on_click(move |_, _, cx| {
                                    if let Some(url) = &copy_url {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            url.to_string(),
                                        ));
                                    }
                                }),
                        )
                        .into_any_element();
                    details = details.item(label, value, 1);
                }
                let mut diff_stats = div().flex().items_center().gap_3().min_w_0();
                for (id, label) in [
                    ("capture-info-diff", format!("Diff %: {percentage}")),
                    (
                        "capture-info-diff-x",
                        format!(
                            "Diff X: {}",
                            dimensions
                                .map(|value| value.width.to_string())
                                .unwrap_or_else(|| "Unavailable".into())
                        ),
                    ),
                    (
                        "capture-info-diff-y",
                        format!(
                            "Diff Y: {}",
                            dimensions
                                .map(|value| value.height.to_string())
                                .unwrap_or_else(|| "Unavailable".into())
                        ),
                    ),
                ] {
                    diff_stats = diff_stats.child(
                        div()
                            .id(id)
                            .role(Role::Status)
                            .aria_label(label.clone())
                            .test_support()
                            .min_w_0()
                            .flex_shrink_1()
                            .truncate()
                            .child(label),
                    );
                }
                details = details.item(
                    div().text_sm().child("Difference:").into_any_element(),
                    diff_stats.into_any_element(),
                    2,
                );
                let pixels = difference
                    .and_then(|diff| diff.mismatch_pixels)
                    .map(|pixels| pixels.to_string())
                    .unwrap_or_else(|| "Unavailable".into());
                details = details.item(
                    div().text_sm().child("Mismatch pixels:").into_any_element(),
                    div()
                        .id("capture-info-mismatch-pixels")
                        .role(Role::Status)
                        .aria_label(format!("Mismatch pixels: {pixels}"))
                        .test_support()
                        .child(pixels)
                        .into_any_element(),
                    2,
                );
                content.child(details)
            })
            .into_any_element()
    }

    fn render_comparison(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.capture_log.active && self.capture.is_some() && self.selected_report().is_some() {
            return self.render_capture_log(cx);
        }
        let view = self.preview_views[self.image_mode];
        let test = self.selected_report().and_then(|report| {
            self.capture
                .and_then(|index| report.report.tests.get(index))
        });
        let title = test
            .map(|test| test.pair.display.title.clone())
            .unwrap_or_else(|| "Comparison".into());
        let previews = if self.image_mode == 3 {
            let count = self.comparison_preview_count();
            let panes = (0..count)
                .map(|index| self.render_preview(index, cx))
                .collect::<Vec<_>>();
            if view.mode == ZoomMode::FitWidth {
                canvas(
                    move |bounds, window, cx| {
                        let gap = window.rem_size() * 0.5;
                        let scale = window.scale_factor();
                        let width = (((bounds.size.width - gap * (count - 1) as f32)
                            / count as f32
                            * scale)
                            .floor()
                            / scale)
                            .max(px(0.));
                        // Fractional pane widths amplify rounding errors in tall aspect-ratio images.
                        let mut row =
                            div()
                                .flex()
                                .size_full()
                                .gap(gap)
                                .children(panes.into_iter().map(|pane| {
                                    div().w(width).h_full().flex_shrink_0().child(pane)
                                }))
                                .into_any_element();
                        row.prepaint_as_root(
                            bounds.origin,
                            bounds.size.map(gpui_kit::AvailableSpace::Definite),
                            window,
                            cx,
                        );
                        row
                    },
                    |_, mut row, window, cx| row.paint(window, cx),
                )
                .size_full()
                .into_any_element()
            } else {
                div()
                    .flex()
                    .size_full()
                    .gap_2()
                    .children(panes)
                    .into_any_element()
            }
        } else {
            self.render_preview(self.zoom_preview_index(), cx)
        };
        let mismatch = test
            .and_then(|test| test.pair.diff.as_ref())
            .map(|diff| {
                [
                    diff.formatted_mismatch()
                        .map(|percentage| format!("{percentage} mismatch")),
                    diff.mismatch_pixels.map(|pixels| {
                        format!(
                            "{pixels} mismatched {}",
                            if pixels == 1 { "pixel" } else { "pixels" }
                        )
                    }),
                ]
                .into_iter()
                .flatten()
                .collect::<Vec<_>>()
                .join(", ")
            })
            .unwrap_or_default();
        let source = self
            .selected_report()
            .and_then(|report| report.source.clone());
        let approval = self.selected_approval();
        div()
            .id("comparison-view")
            .test_support()
            .on_key_down(cx.listener(|this, event, window, cx| this.image_key(event, window, cx)))
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .child(
                div()
                    .px_3()
                    .py(px(1.))
                    .flex_shrink_0()
                    .flex()
                    .flex_wrap()
                    .items_center()
                    .gap_2()
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .id("capture-heading")
                                    .role(Role::Heading)
                                    .aria_label(title.clone())
                                    .test_support()
                                    .min_w_0()
                                    .truncate()
                                    .child(title),
                            )
                            .when_some(test, |heading, test| {
                                heading.child(self.render_capture_information(&test.pair, cx))
                            }),
                    )
                    .child(
                        div()
                            .id("capture-mismatch")
                            .role(Role::Status)
                            .aria_label(mismatch.clone())
                            .test_support()
                            .text_xs()
                            .flex_shrink_0()
                            .child(mismatch),
                    )
                    .when_some(test, |header, test| {
                        header
                            .child(Button::new("results-show-capture-log").ghost().icon(gpui_kit::assets::IconName::List)
                                .tooltip("Capture Log").accessibility_label("Capture Log")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.capture_log.active = true;
                                    this.capture_log.query.offset = 0;
                                    this.load_capture_log(window, cx);
                                    cx.notify();
                                })))
                            .child(
                                design::CopyText::new("copy-capture-details", test.copy_summary(), "Copy capture details")
                                    .tooltip(format!(
                                        "Copy capture details\n\n{}",
                                        test.copy_summary()
                                    ))
                                    .on_copied({
                                        let panel = cx.entity().downgrade();
                                        move |cx| { let _ = panel.update(cx, |this, cx| {
                                            this.status = "Capture details copied".into();
                                            cx.notify();
                                        }); }
                                    }),
                            )
                            .child(
                                Button::new("edit-capture-suite")
                                    .ghost()
                                    .icon(gpui_kit::assets::IconName::Pencil)
                                    .accessibility_label("Edit suite")
                                    .tooltip("Open the suite that produced this capture")
                                    .disabled(source.is_none() || self.loading)
                                    .on_click(cx.listener(move |_, _, _, cx| {
                                        if let Some(entry) = &source {
                                            cx.emit(ResultEvent::OpenSuite(entry.clone()));
                                        }
                                    })),
                            )
                    })
                    .child(
                        Button::new("approve-scenario")
                            .flex_shrink_0()
                            .icon(gpui_kit::assets::IconName::Check)
                            .label("Approve This Scenario")
                            .tooltip("Promote this scenario's successful captures for this viewport only and recalculate the report")
                            .disabled(self.disabled || self.loading || self.image_loading || approval.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if !this.disabled && !this.loading && !this.image_loading && this.selected_approval().is_some() {
                                    this.approval_confirmation = true;
                                    this.approval_error = None;
                                    cx.notify();
                                }
                            })),
                    ),
            )
            .when_some(approval.filter(|_| self.approval_confirmation), |comparison, approval| {
                let viewport = approval.capture_key.strip_prefix(&approval.scenario_id)
                    .and_then(|suffix| suffix.split_once("__"))
                    .map(|(viewport, _)| viewport.trim_start_matches('_'))
                    .filter(|viewport| !viewport.is_empty())
                    .unwrap_or("Default");
                comparison.child(
                    div()
                        .id("scenario-approval-confirmation")
                        .role(Role::Group)
                        .aria_label("Scenario approval confirmation")
                        .test_support()
                        .flex()
                        .items_center()
                        .flex_wrap()
                        .flex_shrink_0()
                        .gap_2()
                        .px_3()
                        .py_2()
                        .bg(cx.theme().warning.opacity(0.12))
                        .child(div().flex().flex_col().flex_1().min_w_0().gap_1()
                            .child(div().text_sm().child("Replace this scenario's baseline for this viewport only?"))
                            .child(div().text_xs().min_w_0().truncate().child(
                                test.map(|test| test.pair.display.title.clone()).unwrap_or_default()
                            ))
                            .child(div().id("scenario-approval-viewport").role(Role::Status)
                                .aria_label(format!("Viewport: {viewport}")).test_support()
                                .text_xs().child(format!("Viewport: {viewport}")))
                            .child(div().id("scenario-approval-run").role(Role::Status)
                                .aria_label(format!("Run: {}", approval.run_id)).test_support()
                                .text_xs().min_w_0().truncate().child(format!("Run: {}", approval.run_id))))
                        .child(Button::new("scenario-approval-cancel").label("Cancel")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.approval_confirmation = false;
                                cx.notify();
                            })))
                        .child(Button::new("scenario-approval-confirm").danger()
                            .icon(gpui_kit::assets::IconName::Check)
                            .label("Approve This Scenario")
                            .disabled(self.disabled || self.loading || self.image_loading)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if this.approval_confirmation && !this.disabled && !this.loading && !this.image_loading
                                    && let Some(approval) = this.selected_approval()
                                {
                                    this.approval_confirmation = false;
                                    cx.emit(ResultEvent::ApproveRun(approval));
                                    cx.notify();
                                }
                            })))
                )
            })
            .children(self.approval_error.as_ref().map(|error| {
                div()
                    .id("scenario-approval-error")
                    .role(Role::Alert)
                    .aria_label(error.clone())
                    .test_support()
                    .flex_shrink_0()
                    .child(
                        gpui_kit::component::marker::Marker::new()
                            .w_full()
                            .px_4()
                            .py_2()
                            .bg(cx.theme().danger.opacity(0.08))
                            .text_color(cx.theme().danger)
                            .child(Icon::new(IconName::CircleX))
                            .child(div().flex_1().min_w_0().whitespace_normal().child(error.clone())),
                    )
            }))
            .child(
                div()
                    .id("image-toolbar")
                    .test_support()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .flex_shrink_0()
                    .min_w_0()
                    .gap_1()
                    .px_3()
                    .py(px(1.))
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        TabBar::new("image-modes")
                            .underline()
                            .flex_shrink_0()
                            .selected_index(self.image_mode)
                            .on_click(cx.listener(|this, index, _, cx| {
                                this.select_image_mode(*index, cx);
                            }))
                            .child(Tab::new().label("Reference"))
                            .child(Tab::new().label("Test"))
                            .child(Tab::new().label("Diff"))
                            .child(Tab::new().label("Side by side"))
                            .child(Tab::new().label("Wipe").disabled(!self.can_blend()))
                            .child(Tab::new().label("Overlay").disabled(!self.can_blend())),
                    )
                    .child(div().flex_1())
                    .child(
                        gpui_kit::component::button::ButtonGroup::new("image-fit-modes")
                            .disabled(self.capture.is_none())
                            .child(
                                Button::new("fit-image")
                                    .icon(gpui_kit::assets::IconName::Maximize)
                                    .tooltip("Fit to window (0)")
                                    .accessibility_label("Fit area")
                                    .selected(view.mode == ZoomMode::FitArea)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_zoom_mode(ZoomMode::FitArea, cx);
                                    })),
                            )
                            .child(
                                Button::new("fit-width")
                                    .icon(gpui_kit::assets::IconName::ArrowLeftRight)
                                    .tooltip("Fit width (W)")
                                    .accessibility_label("Fit width")
                                    .selected(view.mode == ZoomMode::FitWidth)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.set_zoom_mode(ZoomMode::FitWidth, cx);
                                    })),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .flex_shrink_0()
                            .child(Button::new("zoom-out")
                            .ghost()
                            .icon(gpui_kit::assets::IconName::ZoomOut)
                            .tooltip("Zoom out")
                            .accessibility_label("Zoom out")
                            .disabled(self.capture.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.step_zoom(false, cx);
                            })))
                    .child(self.render_zoom_value())
                    .child(
                        Button::new("zoom-in")
                            .ghost()
                            .icon(gpui_kit::assets::IconName::ZoomIn)
                            .tooltip("Zoom in")
                            .accessibility_label("Zoom in")
                            .disabled(self.capture.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.step_zoom(true, cx);
                            })),
                    )
                    .child(
                        Button::new("actual-size")
                            .ghost()
                            .icon(gpui_kit::assets::IconName::Scan)
                            .tooltip("Actual size (100%)")
                            .accessibility_label("Actual size (100%)")
                            .disabled(self.capture.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.actual_size(cx);
                            })),
                            ),
                    ),
            )
            .when(self.image_mode >= 4 && self.can_blend(), |content| {
                let label = if self.image_mode == 4 { "Wipe position" } else { "Test image opacity" };
                content.child(div().id("comparison-blend-controls").role(Role::Group)
                    .aria_label(label).test_support().flex().items_center().gap_3().px_3().py_1()
                    .flex_shrink_0().child(div().text_sm().child(if self.image_mode == 4 { "Wipe" } else { "Opacity" }))
                    .child(div().id("comparison-blend-slider").role(Role::Group)
                        .aria_label(format!("Adjust {label}")).test_support().track_focus(&self.blend_focus)
                        .w(px(180.)).max_w_full()
                        .capture_any_mouse_down(cx.listener(|this, event: &gpui_kit::MouseDownEvent, window, cx| {
                            if event.button == MouseButton::Left {
                                window.focus(&this.blend_focus, cx);
                            }
                        }))
                        .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                            if !this.blend_focus.is_focused(window)
                                || event.keystroke.modifiers != Modifiers::default()
                                || gpui_kit::base::GlobalState::is_in_deferred_context(cx) { return; }
                            let value = this.blend.read(cx).value().end();
                            let next = match event.keystroke.key.as_str() {
                                "left" | "down" => value - 1.,
                                "right" | "up" => value + 1.,
                                "home" => 0.,
                                "end" => 100.,
                                _ => return,
                            }.clamp(0., 100.);
                            this.blend.update(cx, |state, cx| state.set_value(next, window, cx));
                            window.prevent_default();
                            cx.stop_propagation();
                        }))
                        .child(Slider::new(&self.blend)))
                    .child(div().id("comparison-blend-value").role(Role::Status)
                        .aria_label(format!("{:.0}%", self.blend.read(cx).value().end())).test_support()
                        .w(px(40.)).text_sm().child(format!("{:.0}%", self.blend.read(cx).value().end()))))
            })
            .children(
                test.and_then(|test| test.pair.engine_error_msg.as_ref())
                    .map(|error| {
                        div()
                            .id("capture-error")
                            .role(Role::Alert)
                            .aria_label(error.clone())
                            .test_support()
                            .child(
                                gpui_kit::component::marker::Marker::new()
                                    .w_full()
                                    .px_4()
                                    .py_2()
                                    .bg(cx.theme().danger.opacity(0.08))
                                    .text_color(cx.theme().danger)
                                    .child(Icon::new(IconName::CircleX))
                                    .child(div().flex_1().min_w_0().whitespace_normal().child(error.clone())),
                            )
                    }),
            )
            .child(div().id("image-viewer-surface").track_focus(&self.preview_focus).flex_1().min_h_0().child(previews))
            .into_any_element()
    }
}

impl Render for ResultsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let report_name = self
            .selected_report()
            .map(ReportEntry::label)
            .unwrap_or_else(|| "No reports".into());
        let counts = self
            .selected_report()
            .map(|report| report.counts())
            .map(|(passed, mismatch, errors)| {
                format!(
                    "{passed} passed   {mismatch} mismatched   {errors} {}",
                    if errors == 1 { "error" } else { "errors" }
                )
            })
            .unwrap_or_default();
        let log_warning = self.selected_report().is_some_and(|report| {
            report.report.tests.iter().any(|test| {
                [&test.pair.reference_log, &test.pair.test_log]
                    .into_iter()
                    .flatten()
                    .any(|log| !log.state.can_export())
            })
        }) || self
            .capture_log
            .page
            .as_ref()
            .is_some_and(|page| !page.state.can_export());
        div()
            .id("results-panel")
            .test_support()
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .min_w_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_wrap()
                    .gap_2()
                    .px_3()
                    .py(px(1.))
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        div()
                            .id("report-title")
                            .role(Role::Status)
                            .aria_label(report_name.clone())
                            .test_support()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(report_name),
                    )
                    .child(
                        div()
                            .id("results-status")
                            .role(Role::Status)
                            .aria_label(self.status.clone())
                            .test_support()
                            .child(
                                Button::new("results-information")
                                    .ghost()
                                    .icon(gpui_kit::assets::IconName::Info)
                                    .accessibility_label("Report status")
                                    .tooltip(self.status.clone()),
                            ),
                    )
                    .child(
                        Button::new("results-cleanup").ghost().icon(gpui_kit::assets::IconName::Trash)
                            .tooltip("Clean up old test runs").accessibility_label("Clean up old test runs")
                            .disabled(self.disabled || self.loading || self.root.is_none())
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(root) = this.root.clone() {
                                    cx.emit(ResultEvent::Cleanup { root, suite: this.selected_report().map(|report| report.report.id.clone()) });
                                }
                            })),
                    )
                    .child(
                        Button::new("refresh-results")
                            .ghost()
                            .icon(gpui_kit::assets::IconName::RefreshCw)
                            .accessibility_label("Refresh reports")
                            .tooltip(format!("Refresh reports\n{counts}"))
                            .disabled(self.loading || self.root.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                if let Some(root) = this.root.clone() {
                                    this.refresh(root, window, cx);
                                }
                            })),
                    )
                    .child(
                        Button::new("open-artifacts")
                            .ghost()
                            .icon(IconName::FolderOpen)
                            .tooltip("Open artifact directory")
                            .disabled(self.root.is_none())
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.open_external(
                                    this.root
                                        .clone()
                                        .ok_or_else(|| anyhow!("No output directory")),
                                    window,
                                    cx,
                                )
                            })),
                    ),
            )
            .when(log_warning, |panel| panel.child(div().id("results-log-health-warning").role(Role::Status)
                .aria_label("Some capture logs are incomplete or unavailable; image results are unchanged").test_support()
                .px_3().py_1().text_xs().text_color(cx.theme().warning)
                .child("Some capture logs are incomplete or unavailable; image results are unchanged")))
            .child(div().flex_1().min_h_0().child(self.render_comparison(cx)))
            .children(self.catalog.warnings.first().map(|warning| {
                div()
                    .px_4()
                    .py_2()
                    .text_xs()
                    .text_color(cx.theme().warning)
                    .child(warning.clone())
            }))
    }
}

#[cfg(test)]
mod pointer_tests {
    use super::{
        PointerGeometry, PointerGuides, VerticalGap, ZOOM_LEVELS, gaps_to_image_row, next_zoom,
    };
    use gpui_kit::{Bounds, point, px, size};

    #[test]
    fn marked_reference_and_test_failures_use_the_shared_rounded_artwork() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join(".regressify");
        let suite = root.join("shop");
        let html = suite.join("html_report");
        let run = suite.join("bitmaps_test/run-1");
        std::fs::create_dir_all(&html).unwrap();
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("report.json"), r#"{"id":"shop","tests":[]}"#).unwrap();
        let legacy = lodepng::encode32(&[214_u8, 40, 40, 255], 1, 1).unwrap();
        let legacy_path = html.join("legacy-error.png");
        std::fs::write(&legacy_path, &legacy).unwrap();
        std::fs::write(
            crate::test::capture::error_marker_path(&legacy_path),
            r#"{"error":true,"message":"Capture failed"}"#,
        )
        .unwrap();
        std::fs::write(
            html.join("diff-error.png"),
            crate::test::capture::error_icon_png(),
        )
        .unwrap();
        let catalog = super::Catalog::load(&root).unwrap();
        let artifacts = catalog.reports[0].artifacts();
        let diff =
            super::LoadedImage::load(&artifacts, &root, "Diff", Some("diff-error.png")).unwrap();
        let shared = super::LoadedImage::error_placeholder().unwrap();
        for label in ["Reference", "Test"] {
            let preview =
                super::LoadedImage::load(&artifacts, &root, label, Some("legacy-error.png"))
                    .unwrap();
            assert_eq!((preview.width, preview.height), (diff.width, diff.height));
            assert_eq!(
                (preview.width, preview.height),
                (shared.width, shared.height)
            );
            assert!(preview.path.is_none());
        }
        assert!(std::fs::read(&legacy_path).unwrap() == legacy);
        std::fs::write(
            crate::test::capture::error_marker_path(&legacy_path),
            r#"{"error":false,"message":""}"#,
        )
        .unwrap();
        let valid =
            super::LoadedImage::load(&artifacts, &root, "Test", Some("legacy-error.png")).unwrap();
        assert_eq!((valid.width, valid.height), (1, 1));
        assert!(valid.path.is_some());
    }

    #[test]
    fn reference_placeholder_only_replaces_missing_reference_images() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join(".regressify");
        let suite = root.join("shop");
        let html = suite.join("html_report");
        let run = suite.join("bitmaps_test/run-1");
        std::fs::create_dir_all(&html).unwrap();
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("report.json"), r#"{"id":"shop","tests":[]}"#).unwrap();
        let png = lodepng::encode32(&[30_u8, 150, 180, 255], 1, 1).unwrap();
        std::fs::write(html.join("valid.png"), &png).unwrap();
        std::fs::write(html.join("invalid.png"), "not a PNG").unwrap();
        std::fs::write(directory.path().join("outside.png"), &png).unwrap();
        let catalog = super::Catalog::load(&root).unwrap();
        let artifacts = catalog.reports[0].artifacts();
        let placeholder = lodepng::decode32(crate::test::capture::error_icon_png()).unwrap();

        for path in [None, Some("../bitmaps_reference/missing.png")] {
            let image = super::LoadedImage::load(&artifacts, &root, "Reference", path).unwrap();
            assert_eq!(
                (image.width, image.height),
                (placeholder.width, placeholder.height)
            );
            assert!(image.path.is_none());
            for label in ["Test", "Diff"] {
                assert!(super::LoadedImage::load(&artifacts, &root, label, path).is_err());
            }
        }
        for label in ["Reference", "Test", "Diff"] {
            let image =
                super::LoadedImage::load(&artifacts, &root, label, Some("valid.png")).unwrap();
            assert_eq!((image.width, image.height), (1, 1));
            assert_eq!(
                image.path,
                Some(html.join("valid.png").canonicalize().unwrap())
            );
        }
        for (path, message) in [
            ("invalid.png", "PNG header"),
            ("/absolute.png", "relative file path"),
            ("../../../outside.png", "escapes the output directory"),
        ] {
            let error = match super::LoadedImage::load(&artifacts, &root, "Reference", Some(path)) {
                Ok(_) => panic!("Invalid reference must not become a placeholder: {path}"),
                Err(error) => error,
            };
            assert!(format!("{error:#}").contains(message));
        }
        assert!(!suite.join("bitmaps_reference").exists());
        assert_eq!(std::fs::read(html.join("valid.png")).unwrap(), png);
    }

    #[test]
    fn image_urls_use_each_recorded_initial_url_without_cross_fallback() {
        let mut pair: crate::desktop::results::Pair = serde_json::from_value(serde_json::json!({
            "referenceUrl": "https://production.example.test/start?source=reference",
            "url": "https://staging.example.test/start?source=test"
        }))
        .unwrap();
        pair.display.page_url = Some("https://current-workspace.example.test/changed".into());
        assert_eq!(
            super::image_page_url(&pair, 0).unwrap().as_str(),
            pair.reference_url.as_ref().unwrap()
        );
        assert_eq!(
            super::image_page_url(&pair, 1).unwrap().as_str(),
            pair.url.as_ref().unwrap()
        );
        assert!(super::image_page_url(&pair, 2).is_none());
        pair.reference_url = None;
        assert!(super::image_page_url(&pair, 0).is_none());
        assert!(super::image_page_url(&pair, 1).is_some());
        for invalid in [
            "",
            "relative/path",
            "javascript:alert(1)",
            "data:text/html,test",
            "vscode://file/private",
        ] {
            pair.url = Some(invalid.into());
            assert!(super::image_page_url(&pair, 1).is_none());
        }
        pair.url = Some("file:///tmp/scenario.html".into());
        assert_eq!(super::image_page_url(&pair, 1).unwrap().scheme(), "file");
        pair.url = None;
        assert!(super::image_page_url(&pair, 1).is_none());
    }

    #[test]
    fn zoom_steps_follow_chrome_presets_and_snap_in_the_requested_direction() {
        for levels in ZOOM_LEVELS.windows(2) {
            assert_eq!(next_zoom(levels[0], true), levels[1]);
            assert_eq!(next_zoom(levels[1], false), levels[0]);
        }
        for (current, smaller, larger) in [
            (0.42, 1. / 3., 0.5),
            (0.68, 2. / 3., 0.75),
            (0.92, 0.9, 1.),
            (1.17, 1.1, 1.25),
            (1.5625, 1.5, 1.75),
        ] {
            assert_eq!(next_zoom(current, false), smaller);
            assert_eq!(next_zoom(current, true), larger);
        }
        assert_eq!(next_zoom(0.25, false), 0.25);
        assert_eq!(next_zoom(5., true), 6.);
        assert_eq!(next_zoom(6., true), 8.);
        assert_eq!(next_zoom(8., true), 8.);
        assert_eq!(next_zoom(0.1, true), 0.25);
        assert_eq!(next_zoom(0.1, false), 0.1);
        assert_eq!(next_zoom(1.0005, true), 1.1);
        assert_eq!(next_zoom(0.9995, false), 0.9);
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn image_previews_blend_controls_and_keyboard_are_scoped(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::test::{TestAppContextExt as _, TestWindowExt as _};
        use gpui_kit::{AppContext as _, InputEvent as _};
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let run = directory
            .path()
            .join(".regressify/ui-test/bitmaps_test/run-1");
        std::fs::create_dir_all(&run).unwrap();
        for (name, width, height, color) in [
            ("reference", 1200, 1800, [200_u8, 40, 60, 255]),
            ("current", 1000, 1600, [40_u8, 160, 200, 255]),
        ] {
            lodepng::encode32_file(
                run.join(format!("{name}.png")),
                &color.repeat(width * height),
                width,
                height,
            )
            .unwrap();
        }
        std::fs::write(run.join("report.json"), serde_json::to_vec(&serde_json::json!({
            "id": "ui-test", "tests": [
                {"status": "fail", "pair": {"label": "Blend", "reference": "../bitmaps_test/run-1/reference.png", "test": "../bitmaps_test/run-1/current.png"}},
                {"status": "fail", "pair": {"label": "Missing", "test": "../bitmaps_test/run-1/current.png"}}
            ]
        })).unwrap()).unwrap();
        let (handle, editor) = crate::desktop::ui::tests::editor(cx, directory.path());
        let results = cx.update(|cx| editor.read(cx).results.clone());
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.within("workbench-tabs").click(2_usize, cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
            window.try_find(("image-details", 1_usize)).is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            for mode in [4_usize, 5] {
                window.within("image-modes").click(mode, cx);
                window.click("comparison-blend-slider", cx);
                window.press("home", cx);
                assert_eq!(window.find("comparison-blend-value").label(), Some("0%"));
                window.press("end", cx);
                assert_eq!(window.find("comparison-blend-value").label(), Some("100%"));
                window.press("left", cx);
                assert_eq!(window.find("comparison-blend-value").label(), Some("99%"));
                assert_eq!(window.find("fit-image").checked(), Some(true));
                if mode == 4 {
                    window.press("home", cx);
                    let full = window.find(("image-content", 0_usize)).bounds();
                    assert_eq!(
                        window.find("comparison-test-layer").bounds().size,
                        full.size
                    );
                    window.press("end", cx);
                    assert!(window.try_find("comparison-test-layer").is_none());
                    let pointer =
                        full.origin + point(full.size.width * 0.25, full.size.height * 0.5);
                    window.dispatch_event(
                        gpui_kit::MouseMoveEvent {
                            position: pointer,
                            ..Default::default()
                        }
                        .to_platform_input(),
                        cx,
                    );
                    window.render_frame(cx);
                    assert_eq!(window.find("comparison-blend-value").label(), Some("25%"));
                    let divider = window.find("comparison-wipe-divider").bounds();
                    assert!((divider.left() - pointer.x).abs() <= px(1.));
                }
                window.click(("image-content", 0_usize), cx);
                window.press("1", cx);
                assert_eq!(window.find("zoom-value").label(), Some("100.0%"));
                window.press("=", cx);
                assert_eq!(window.find("zoom-value").label(), Some("110.0%"));
                window.press("right", cx);
                assert!(results.read(cx).image_scroll().offset().x < px(0.));
                window.press("0", cx);
                assert_eq!(window.find("fit-image").checked(), Some(true));
                window.press("w", cx);
                assert_eq!(window.find("fit-width").checked(), Some(true));
            }
            window.within("image-modes").click(4_usize, cx);
            assert_eq!(window.find("fit-width").checked(), Some(true));
            window.click(("capture", 1_usize), cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
            window.try_find(("image-details", 1_usize)).is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            for mode in [4_usize, 5] {
                window.within("image-modes").click(mode, cx);
                assert_eq!(results.read(cx).image_mode, 3);
                assert!(window.try_find("comparison-blend-slider").is_none());
            }
        })
        .unwrap();
    }

    #[test]
    fn crosshairs_use_the_same_image_coordinate_after_scroll_and_scaling() {
        let guides = PointerGuides::default();
        for geometry in &guides.geometry[..2] {
            *geometry.borrow_mut() = Some(PointerGeometry {
                canvas: (1000, 2000),
                gap: None,
                gaps: Vec::new(),
            });
        }
        guides.active.set(Some(0));
        guides.bounds[0].set(Some(Bounds::new(
            point(px(-100.), px(-200.)),
            size(px(1000.), px(2000.)),
        )));
        guides.bounds[1].set(Some(Bounds::new(
            point(px(600.), px(-200.)),
            size(px(1000.), px(2000.)),
        )));
        assert_eq!(
            guides.position(1, point(px(150.), px(300.))),
            Some(point(px(850.), px(300.)))
        );
        guides.bounds[1].set(Some(Bounds::new(
            point(px(600.), px(10.)),
            size(px(500.), px(1000.)),
        )));
        assert_eq!(
            guides.position(1, point(px(150.), px(300.))),
            Some(point(px(725.), px(260.)))
        );
        assert_eq!(
            guides.position(1, point(px(-101.), px(300.))),
            Some(point(px(599.5), px(260.)))
        );
        guides.active.set(None);
        assert!(guides.position(1, point(px(150.), px(300.))).is_none());
    }

    #[test]
    fn crosshairs_project_empty_lane_space_without_clamping_to_images() {
        let guides = PointerGuides::default();
        for index in 0..2 {
            *guides.geometry[index].borrow_mut() = Some(PointerGeometry {
                canvas: (200, 300),
                gap: None,
                gaps: Vec::new(),
            });
            guides.bounds[index].set(Some(Bounds::new(
                point(px(100. + index as f32 * 300.), px(50.)),
                size(px(100.), px(150.)),
            )));
        }
        for source in 0..2 {
            guides.active.set(Some(source));
            let source_bounds = guides.bounds[source].get().unwrap();
            for offset in [point(px(-30.), px(-20.)), point(px(140.), px(180.))] {
                let mouse = source_bounds.origin + offset;
                for target in 0..2 {
                    let expected = guides.bounds[target].get().unwrap().origin + offset;
                    let actual = guides
                        .position(target, mouse)
                        .expect("Blank lane space must retain its crosshair");
                    assert!((actual.x - expected.x).abs() < px(0.01));
                    assert!((actual.y - expected.y).abs() < px(0.01));
                }
            }
        }
    }

    #[test]
    fn crosshairs_map_diff_bands_across_shared_canvas_in_every_pane() {
        let gap = VerticalGap {
            start: 100,
            height: 48,
        };
        for shorter in [0, 1] {
            for scale in [0.5, 1., 1.25] {
                let guides = PointerGuides::default();
                for index in 0..3 {
                    guides.bounds[index].set(Some(Bounds::new(
                        point(px(index as f32 * 600. - 80.), px(-40.)),
                        size(px(320. * scale), px(448. * scale)),
                    )));
                    *guides.geometry[index].borrow_mut() = Some(PointerGeometry {
                        canvas: (320, 448),
                        gap: (index == shorter).then_some(gap),
                        gaps: if index == shorter {
                            vec![gap]
                        } else {
                            Vec::new()
                        },
                    });
                }
                for source in 0..3 {
                    guides.active.set(Some(source));
                    for row in [0., 99.5, 100., 124., 147.5, 148., 220., 447.5] {
                        let Some(source_row) = (if source == shorter {
                            gap.to_image_row(row)
                        } else {
                            Some(row)
                        }) else {
                            continue;
                        };
                        let mouse = guides.bounds[source].get().unwrap().origin
                            + point(px(160. * scale), px(source_row * scale));
                        for target in 0..3 {
                            let expected_row = if target == shorter {
                                gap.to_image_row(row)
                            } else {
                                Some(row)
                            };
                            let actual = guides.position(target, mouse);
                            match expected_row {
                                Some(expected_row) => {
                                    let expected = guides.bounds[target].get().unwrap().origin
                                        + point(px(160. * scale), px(expected_row * scale));
                                    let actual = actual
                                        .expect("Corresponding image pixel must have a guide");
                                    assert!((actual.x - expected.x).abs() < px(0.01));
                                    assert!((actual.y - expected.y).abs() < px(0.01));
                                }
                                None => assert!(
                                    actual.is_none(),
                                    "Missing band pixels must not have a guide"
                                ),
                            }
                        }
                    }
                }
                guides.active.set(Some(shorter));
                let padding = guides.bounds[shorter].get().unwrap().origin
                    + point(px(100. * scale), px(420. * scale));
                let expected = guides.bounds[2].get().unwrap().origin
                    + point(px(100. * scale), px(468. * scale));
                let actual = guides
                    .position(2, padding)
                    .expect("Blank shared canvas must retain its aligned guide");
                assert!((actual.x - expected.x).abs() < px(0.01));
                assert!((actual.y - expected.y).abs() < px(0.01));
                *guides.geometry[2].borrow_mut() = None;
                assert!(
                    guides
                        .position(2, guides.bounds[shorter].get().unwrap().center())
                        .is_none()
                );
            }
        }

        // Verify multi-band alignment (e.g., two separate insertions in test).
        let multi_gaps = vec![
            VerticalGap {
                start: 40,
                height: 10,
            },
            VerticalGap {
                start: 100,
                height: 20,
            },
        ];
        let guides = PointerGuides::default();
        for index in 0..3 {
            guides.bounds[index].set(Some(Bounds::new(
                point(px(index as f32 * 400.), px(0.)),
                size(px(320.), px(230.)),
            )));
            *guides.geometry[index].borrow_mut() = Some(PointerGeometry {
                canvas: (320, 230),
                gap: (index == 0).then_some(multi_gaps[0]),
                gaps: if index == 0 {
                    multi_gaps.clone()
                } else {
                    Vec::new()
                },
            });
        }
        guides.active.set(Some(1));
        for (aligned_row, expected_ref_row) in [
            (20., Some(20.)),
            (45., None),
            (70., Some(60.)),
            (115., None),
            (150., Some(120.)),
        ] {
            let mouse = point(px(400. + 160.), px(aligned_row));
            let actual = guides.position(0, mouse);
            assert_eq!(actual.map(|p| p.y), expected_ref_row.map(px));
        }
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn comparison_completion_replaces_previously_opened_run(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        check_comparison_completion(cx, false, crate::desktop::runner::RunOutcome::Completed).await;
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn full_test_capture_completion_selects_latest_full_suite_report(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        check_capture_completion(cx, CaptureCompletion::test(Some("compared"))).await;
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn partial_reference_capture_completion_keeps_latest_full_suite_report(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        check_capture_completion(
            cx,
            CaptureCompletion {
                command: crate::desktop::runner::RunCommand::Reference,
                full_suite: false,
                ..CaptureCompletion::test(Some("previous"))
            },
        )
        .await;
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn failed_capture_completion_keeps_reviewed_report(cx: &mut gpui_kit::TestAppContext) {
        check_capture_completion(
            cx,
            CaptureCompletion {
                outcome: crate::desktop::runner::RunOutcome::Failed("Capture failed".into()),
                ..CaptureCompletion::test(Some("previous"))
            },
        )
        .await;
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn cancelled_capture_completion_keeps_reviewed_report(cx: &mut gpui_kit::TestAppContext) {
        check_capture_completion(
            cx,
            CaptureCompletion {
                outcome: crate::desktop::runner::RunOutcome::Cancelled,
                ..CaptureCompletion::test(Some("previous"))
            },
        )
        .await;
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn hidden_capture_completion_leaves_missing_comparison_ungenerated(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        check_capture_completion(
            cx,
            CaptureCompletion {
                saved_report: false,
                ..CaptureCompletion::test(None)
            },
        )
        .await;
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn visible_capture_completion_generates_missing_comparison(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        check_capture_completion(
            cx,
            CaptureCompletion {
                results_visible: true,
                saved_report: false,
                ..CaptureCompletion::test(Some("compared"))
            },
        )
        .await;
    }

    #[cfg(feature = "gui-tests")]
    struct CaptureCompletion {
        command: crate::desktop::runner::RunCommand,
        full_suite: bool,
        outcome: crate::desktop::runner::RunOutcome,
        results_visible: bool,
        // False leaves the finished run as a skipped-comparison capture.
        saved_report: bool,
        expected_run: Option<&'static str>,
    }

    #[cfg(feature = "gui-tests")]
    impl CaptureCompletion {
        fn test(expected_run: Option<&'static str>) -> Self {
            Self {
                command: crate::desktop::runner::RunCommand::Test,
                full_suite: true,
                outcome: crate::desktop::runner::RunOutcome::Completed,
                results_visible: false,
                saved_report: true,
                expected_run,
            }
        }
    }

    #[cfg(feature = "gui-tests")]
    async fn check_capture_completion(
        cx: &mut gpui_kit::TestAppContext,
        completion: CaptureCompletion,
    ) {
        use crate::desktop::runner::{RunOptions, RunOutcome, RunRequest};
        use crate::progress::LiveProgress;
        use crate::run_history::{RunLease, RunState};
        use gpui_kit::AppContext;
        use gpui_kit::test::TestAppContextExt;
        use std::time::Duration;

        let CaptureCompletion {
            command,
            full_suite,
            outcome,
            results_visible,
            saved_report,
            expected_run,
        } = completion;
        let use_requested_output = matches!(outcome, RunOutcome::Completed);
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = crate::desktop::ui::tests::editor(cx, directory.path());
        let (results, runs, suites) = cx.update(|cx| {
            let editor = view.read(cx);
            (
                editor.results.clone(),
                editor.runs.clone(),
                editor.session.suites.clone(),
            )
        });
        let scenario_id = suites[0].scenarios[0].id.clone();
        let root = directory.path().canonicalize().unwrap();
        let requested_output = root.join("requested-output");
        let old_output = root.join(".regressify");

        let write_run = |output: &std::path::Path,
                         run_id: &str,
                         full: bool,
                         failed: bool,
                         with_report: bool| {
            let suite_dir = output.join("ui-test");
            let mut lease = RunLease::begin(&suite_dir, run_id, full).unwrap();
            let run = lease.suite_dir().join("bitmaps_test").join(run_id);
            if with_report {
                std::fs::write(
                    run.join("report.json"),
                    serde_json::to_vec(&serde_json::json!({
                        "id": "ui-test",
                        "testSuite": "ui-test",
                        "captureInventoryComplete": true,
                        "tests": [{
                            "status": if failed { "fail" } else { "pass" },
                            "pair": {
                                "scenarioId": scenario_id,
                                "scenarioLabel": "Checkout",
                                "fileName": format!("{scenario_id}_desktop__FINAL.png"),
                            },
                        }],
                    }))
                    .unwrap(),
                )
                .unwrap();
            } else {
                image::GrayImage::from_pixel(2, 2, image::Luma([80]))
                    .save(run.join("home.png"))
                    .unwrap();
            }
            lease.finish(RunState::Completed, with_report).unwrap();
        };
        write_run(&old_output, "previous", true, false, true);
        write_run(&requested_output, "previous", true, false, true);
        write_run(
            &requested_output,
            "compared",
            full_suite,
            true,
            saved_report,
        );
        let compared_report = requested_output.join("ui-test/bitmaps_test/compared/report.json");

        cx.update_window(handle.into(), |_, window, cx| {
            results.update(cx, |panel, cx| {
                panel.set_suites(suites);
                panel.show_run(old_output.clone(), "previous".into(), window, cx);
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
            let panel = results.read(cx);
            !panel.loading && panel.selected_report().is_some()
        })
        .await;
        let generation = cx.update(|cx| {
            let panel = results.read(cx);
            assert_eq!(panel.selected_report().unwrap().run_id, "previous");
            panel.generation
        });

        cx.update(|cx| {
            if results_visible {
                view.update(cx, |this, cx| {
                    this.main_tab = 2;
                    cx.notify();
                });
            }
            runs.update(cx, |panel, cx| {
                panel.manager.request = Some(RunRequest {
                    root,
                    command,
                    options: RunOptions {
                        suites: vec!["ui-test".into()],
                        run_id: "compared".into(),
                        regressify_dir: "requested-output".into(),
                        ..Default::default()
                    },
                });
                panel.manager.outcome = Some(outcome);
                panel.manager.progress = LiveProgress::default();
                cx.emit(crate::desktop::ui::runs::RunEvent::Finished);
            });
        });
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
            let panel = results.read(cx);
            panel.generation > generation && !panel.loading
        })
        .await;

        cx.update(|cx| {
            let panel = results.read(cx);
            assert_eq!(
                panel.selected_report().map(|report| report.run_id.as_str()),
                expected_run
            );
            assert_eq!(
                panel.root.as_ref(),
                Some(if use_requested_output {
                    &requested_output
                } else {
                    &old_output
                })
            );
        });
        assert_eq!(
            compared_report.is_file(),
            saved_report || expected_run == Some("compared")
        );

        if expected_run.is_none() {
            // Opening Results is explicit viewing, which generates the pending
            // comparison for the latest default run left unpinned by completion.
            let generation = cx.update(|cx| results.read(cx).generation);
            cx.update_window(handle.into(), |_, window, cx| {
                results.update(cx, |panel, cx| {
                    panel.refresh(requested_output.clone(), window, cx)
                });
            })
            .unwrap();
            cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
                let panel = results.read(cx);
                panel.generation > generation && !panel.loading
            })
            .await;
            cx.update(|cx| {
                assert_eq!(
                    results.read(cx).selected_report().unwrap().run_id,
                    "compared"
                );
            });
            assert!(compared_report.is_file());
        }
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    fn local_html_report_action_is_not_offered(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext;
        use gpui_kit::test::TestWindowExt;
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = crate::desktop::ui::tests::editor(cx, directory.path());
        let output = directory.path().join("missing-custom-output");
        let results = cx.update(|cx| view.read(cx).results.clone());
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |this, cx| {
                this.main_tab = 2;
                cx.notify();
            });
            for (has_output, disabled) in [(false, false), (true, false), (true, true)] {
                results.update(cx, |panel, cx| {
                    panel.root = has_output.then(|| output.clone());
                    panel.disabled = disabled;
                    cx.notify();
                });
                window.render_frame(cx);
                assert!(window.try_find("open-report").is_none());
                assert!(window.find("refresh-results").visible());
                assert!(window.find("open-artifacts").visible());
                assert!(!view.read(cx).busy);
            }
        })
        .unwrap();
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn comparison_completion_replaces_saved_selection_in_requested_output(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        check_comparison_completion(cx, true, crate::desktop::runner::RunOutcome::Completed).await;
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn comparison_completion_failed_command_keeps_reviewed_run(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        check_comparison_completion(
            cx,
            false,
            crate::desktop::runner::RunOutcome::Failed("Report could not be written".into()),
        )
        .await;
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn comparison_completion_cancelled_command_keeps_reviewed_run(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        check_comparison_completion(cx, false, crate::desktop::runner::RunOutcome::Cancelled).await;
    }

    #[cfg(feature = "gui-tests")]
    async fn check_comparison_completion(
        cx: &mut gpui_kit::TestAppContext,
        saved_selection: bool,
        outcome: crate::desktop::runner::RunOutcome,
    ) {
        use crate::desktop::runner::{RunCommand, RunOptions, RunOutcome, RunRequest};
        use crate::progress::{Event, LiveProgress, Record};
        use gpui_kit::AppContext;
        use gpui_kit::test::{TestAppContextExt, TestWindowExt};
        use std::time::Duration;

        let completed = matches!(outcome, RunOutcome::Completed);
        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = crate::desktop::ui::tests::editor(cx, directory.path());
        let (results, runs, suites) = cx.update(|cx| {
            let editor = view.read(cx);
            (
                editor.results.clone(),
                editor.runs.clone(),
                editor.session.suites.clone(),
            )
        });
        let scenario_id = suites[0].scenarios[0].id.clone();
        let root = directory.path().canonicalize().unwrap();
        let output = root.join(".regressify");
        let compared_output = if saved_selection {
            root.join("comparison-output")
        } else {
            output.clone()
        };
        for run_id in ["previous", "compared"] {
            let selected_output = if run_id == "compared" {
                &compared_output
            } else {
                &output
            };
            let run = selected_output.join("ui-test/bitmaps_test").join(run_id);
            std::fs::create_dir_all(&run).unwrap();
            let tests = [
                "$load-page",
                "$input-username",
                "$input-email",
                "$input-password",
                "FINAL",
            ]
            .iter()
            .enumerate()
            .map(|(index, capture)| {
                serde_json::json!({
                    "status": if run_id == "compared" && index == 1 { "fail" } else { "pass" },
                    "pair": {
                        "scenarioId": scenario_id,
                        "scenarioLabel": "Checkout",
                        "fileName": format!("{scenario_id}_desktop__{capture}.png"),
                    },
                })
            })
            .collect::<Vec<_>>();
            std::fs::write(
                run.join("report.json"),
                serde_json::to_vec(&serde_json::json!({
                    "id": "ui-test", "testSuite": "ui-test", "tests": tests,
                }))
                .unwrap(),
            )
            .unwrap();
        }
        let unrelated = compared_output.join("unrelated/bitmaps_test/compared");
        std::fs::create_dir_all(&unrelated).unwrap();
        std::fs::write(
            unrelated.join("report.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "unrelated", "tests": [{
                    "status": "pass", "pair": { "fileName": "other_desktop__FINAL.png" },
                }],
            }))
            .unwrap(),
        )
        .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            results.update(cx, |panel, cx| {
                panel.set_suites(suites);
                if saved_selection {
                    panel.show_saved_reports(
                        output.clone(),
                        vec![("ui-test".into(), "previous".into())],
                        window,
                        cx,
                    );
                } else {
                    panel.show_run(output.clone(), "previous".into(), window, cx);
                }
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
            let panel = results.read(cx);
            !panel.loading && panel.selected_report().is_some()
        })
        .await;
        let generation = cx.update(|cx| {
            let panel = results.read(cx);
            assert_eq!(panel.selected_report().unwrap().counts(), (5, 0, 0));
            panel.generation
        });
        cx.update(|cx| {
            view.update(cx, |editor, _| editor.main_tab = 1);
            runs.update(cx, |panel, cx| {
                panel.manager.request = Some(RunRequest {
                    root,
                    command: RunCommand::Compare,
                    options: RunOptions {
                        suites: vec!["ui-test".into()],
                        run_id: "compared".into(),
                        regressify_dir: if saved_selection {
                            "comparison-output".into()
                        } else {
                            ".regressify".into()
                        },
                        ..Default::default()
                    },
                });
                panel.manager.outcome = Some(outcome);
                panel.manager.progress = LiveProgress::default();
                panel.manager.progress.apply(Record {
                    version: 1,
                    elapsed_ms: 1,
                    event: Event::Compared {
                        suite_id: "ui-test".into(),
                        capture: format!("{scenario_id}_desktop__$input-username"),
                        label: "Checkout".into(),
                        scenario_id: Some(scenario_id),
                        passed: false,
                        error: Some("Visual mismatch".into()),
                    },
                });
                panel.manager.progress.end(false, None);
                cx.emit(crate::desktop::ui::runs::RunEvent::Finished);
            });
        });
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
            let panel = results.read(cx);
            panel.generation > generation && !panel.loading
        })
        .await;
        cx.update(|cx| {
            let panel = results.read(cx);
            assert_eq!(panel.catalog.reports.len(), 1);
            let report = panel.selected_report().unwrap();
            assert_eq!(
                report.run_id,
                if completed { "compared" } else { "previous" }
            );
            assert_eq!(
                report.counts(),
                if completed { (4, 1, 0) } else { (5, 0, 0) }
            );
            assert_eq!(
                report.report.scenario_counts().failed,
                usize::from(completed)
            );
            assert_eq!(
                panel.root.as_ref(),
                Some(if completed { &compared_output } else { &output })
            );
            assert_eq!(view.read(cx).main_tab, 1);
            view.update(cx, |editor, cx| {
                editor.main_tab = 2;
                cx.notify();
            });
        });
        cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
            window
                .try_find(("capture", 1_usize))
                .is_some_and(|capture| {
                    capture.label()
                        == Some(if completed {
                            "Mismatch Checkout"
                        } else {
                            "Passed Checkout"
                        })
                })
        })
        .await;
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn approval_completion_preserves_comparison_and_updates_suite_counts(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use crate::desktop::runner::{RunCommand, RunOptions, RunOutcome, RunRequest};
        use crate::progress::{Event, LiveProgress, Record};
        use gpui_kit::AppContext;
        use gpui_kit::test::{TestAppContextExt, TestWindowExt};
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = crate::desktop::ui::tests::editor(cx, directory.path());
        let (results, runs, scenario_id, suite_index) = cx.update(|cx| {
            let editor = view.read(cx);
            (
                editor.results.clone(),
                editor.runs.clone(),
                editor.session.suites[0].scenarios[0].id.clone(),
                editor
                    .session
                    .entries
                    .iter()
                    .position(|entry| entry.suite_name().as_deref() == Some("ui-test"))
                    .unwrap(),
            )
        });
        let output = directory.path().join("review-output");
        let suite = output.join("ui-test");
        let run = suite.join("bitmaps_test/reviewed");
        let reference = suite.join("bitmaps_reference");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::create_dir_all(&reference).unwrap();
        let mut metadata = serde_json::Map::new();
        for viewport in ["desktop", "phone"] {
            let key = format!("{scenario_id}_{viewport}__FINAL");
            lodepng::encode32_file(run.join(format!("{key}.png")), &[200_u8, 20, 30, 255], 1, 1)
                .unwrap();
            lodepng::encode32_file(
                reference.join(format!("{key}.png")),
                &[10_u8, 20, 30, 255],
                1,
                1,
            )
            .unwrap();
            metadata.insert(key, serde_json::json!({
                "scenarioId": scenario_id, "scenarioLabel": "Checkout",
                "referenceUrl": "https://production.example.test", "url": "https://staging.example.test",
            }));
        }
        std::fs::write(
            run.join(".report-urls.json"),
            serde_json::to_vec(&serde_json::json!({
                "version": 1, "captures": metadata,
            }))
            .unwrap(),
        )
        .unwrap();
        crate::compare::report_gen::generate_comparison_report(
            &suite, "ui-test", "reviewed", 0.1, 1,
        )
        .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            view.update(cx, |editor, _| editor.main_tab = 2);
            runs.update(cx, |panel, cx| {
                panel.manager.request = Some(RunRequest {
                    root: directory.path().canonicalize().unwrap(),
                    command: RunCommand::Test,
                    options: RunOptions {
                        suites: vec!["ui-test".into()],
                        ..Default::default()
                    },
                });
                panel.manager.progress = LiveProgress::default();
                for viewport in ["desktop", "phone"] {
                    panel.manager.progress.apply(Record {
                        version: 1,
                        elapsed_ms: 1,
                        event: Event::Compared {
                            suite_id: "ui-test".into(),
                            capture: format!("{scenario_id}_{viewport}__FINAL"),
                            label: "Checkout".into(),
                            scenario_id: Some(scenario_id.clone()),
                            passed: false,
                            error: Some("Visual mismatch".into()),
                        },
                    });
                }
                panel.manager.progress.end(false, None);
                panel.manager.outcome = Some(RunOutcome::Completed);
                cx.notify();
            });
            let suites = view.read(cx).session.suites.clone();
            results.update(cx, |panel, cx| {
                panel.set_suites(suites);
                panel.show_run(output.clone(), "reviewed".into(), window, cx);
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
            let panel = results.read(cx);
            panel.capture == Some(0) && !panel.image_loading && !panel.previews.is_empty()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(
                window.find(("suite-activity", suite_index)).label(),
                Some("2 failed / 2 finished")
            );
            window.click(("capture", 1_usize), cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
            let panel = results.read(cx);
            panel.capture == Some(1) && !panel.image_loading
        })
        .await;
        let request = cx
            .update_window(handle.into(), |_, window, cx| {
                window.within("image-modes").click(1_usize, cx);
                window.click("actual-size", cx);
                window.click("zoom-in", cx);
                window.click("approve-scenario", cx);
                assert_eq!(view.read(cx).main_tab, 2);
                assert_eq!(
                    window.find("scenario-approval-viewport").label(),
                    Some("Viewport: phone")
                );
                let approval = results.read(cx).selected_approval().unwrap();
                let intent = runs.read(cx).report_approval_intent(&approval).unwrap();
                assert!(!intent.review_after_run);
                window.click("scenario-approval-cancel", cx);
                intent.resolve(&view.read(cx).session.suites).unwrap()
            })
            .unwrap();
        crate::compare::report_gen::approve_scenario_viewport(
            &suite,
            "reviewed",
            &scenario_id,
            &format!("{scenario_id}_phone__FINAL"),
        )
        .unwrap();
        cx.update(|cx| {
            runs.update(cx, |panel, cx| {
                panel.manager.request = Some(request);
                panel.manager.progress = LiveProgress::default();
                panel.manager.progress.end(false, None);
                panel.manager.outcome = Some(RunOutcome::Completed);
                cx.emit(crate::desktop::ui::runs::RunEvent::Finished);
            })
        });
        cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
            window
                .try_find(("suite-activity", suite_index))
                .is_some_and(|status| status.label() == Some("1 failed / 2 finished"))
                && window.try_find(("image-details", 1_usize)).is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(view.read(cx).main_tab, 2);
            assert_eq!(results.read(cx).capture, Some(1));
            assert_eq!(results.read(cx).root.as_ref(), Some(&output));
            assert_eq!(
                window.within("image-modes").find(1_usize).selected(),
                Some(true)
            );
            assert_eq!(window.find("zoom-value").label(), Some("110.0%"));
            assert_eq!(
                window.find(("capture", 1_usize)).label(),
                Some("Passed Checkout")
            );
            runs.update(cx, |panel, cx| {
                panel.manager.outcome =
                    Some(RunOutcome::Failed("Report could not be written".into()));
                cx.emit(crate::desktop::ui::runs::RunEvent::Finished);
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
            window
                .try_find("scenario-approval-error")
                .is_some_and(|error| {
                    error.label() == Some("Approval failed: Report could not be written")
                })
                && window.try_find(("image-details", 1_usize)).is_some()
        })
        .await;
        cx.update(|cx| assert_eq!(view.read(cx).main_tab, 2));
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn scenario_approval_tracks_viewport_and_rejects_unavailable_captures(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::AppContext;
        use gpui_kit::test::{TestAppContextExt, TestWindowExt};
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let (handle, view) = crate::desktop::ui::tests::editor(cx, directory.path());
        let (results, scenario_id) = cx.update(|cx| {
            (
                view.read(cx).results.clone(),
                view.read(cx).session.suites[0].scenarios[0].id.clone(),
            )
        });
        let run = directory
            .path()
            .join(".regressify/ui-test/bitmaps_test/reviewed");
        std::fs::create_dir_all(&run).unwrap();
        let mut captures = Vec::new();
        for (index, viewport) in ["desktop", "mobile", "failed", "legacy", "missing"]
            .iter()
            .enumerate()
        {
            let filename = format!("{scenario_id}_{viewport}__FINAL.png");
            if index != 4 {
                lodepng::encode32_file(run.join(&filename), &[30_u8, 150, 180, 255], 1, 1).unwrap();
            }
            if index == 2 {
                std::fs::write(
                    crate::test::capture::error_marker_path(&run.join(&filename)),
                    r#"{"error":true,"message":"Capture failed"}"#,
                )
                .unwrap();
            }
            captures.push(serde_json::json!({
                "status": "fail", "pair": {
                    "label": viewport, "fileName": filename,
                    "scenarioId": (index != 3).then_some(&scenario_id),
                    "test": format!("../bitmaps_test/reviewed/{filename}")
                }
            }));
        }
        std::fs::write(
            run.join("report.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "ui-test", "tests": captures,
            }))
            .unwrap(),
        )
        .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.within("workbench-tabs").click(2_usize, cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
            let panel = results.read(cx);
            panel.capture == Some(0) && !panel.image_loading && !panel.previews.is_empty()
        })
        .await;
        for (index, viewport) in ["desktop", "mobile", "failed", "legacy", "missing"]
            .iter()
            .enumerate()
        {
            cx.update_window(handle.into(), |_, window, cx| {
                results.update(cx, |panel, cx| panel.select_capture(index, window, cx));
                if index != 0 {
                    assert!(results.read(cx).selected_approval().is_none());
                    assert!(!results.read(cx).approval_confirmation);
                }
            })
            .unwrap();
            cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
                let panel = results.read(cx);
                panel.capture == Some(index) && !panel.image_loading
            })
            .await;
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                assert!(window.try_find("approve-report").is_none());
                let approval = results.read(cx).selected_approval();
                assert_eq!(approval.is_some(), index < 2);
                if let Some(approval) = approval {
                    assert_eq!(approval.scenario_id, scenario_id);
                    assert_eq!(
                        approval.capture_key,
                        format!("{scenario_id}_{viewport}__FINAL")
                    );
                    assert_eq!(approval.run_id, "reviewed");
                    window.click("approve-scenario", cx);
                    assert_eq!(view.read(cx).main_tab, 2);
                    assert!(window.try_find("scenario-approval-confirm").is_some());
                    assert_eq!(
                        window.find("scenario-approval-viewport").label(),
                        Some(format!("Viewport: {viewport}").as_str())
                    );
                    if index == 0 {
                        results.update(cx, |panel, cx| panel.set_disabled(true, cx));
                        window.render_frame(cx);
                        assert!(window.try_find("scenario-approval-confirm").is_none());
                        results.update(cx, |panel, cx| panel.set_disabled(false, cx));
                        window.click("approve-scenario", cx);
                    }
                } else {
                    window.click("approve-scenario", cx);
                    assert_eq!(view.read(cx).main_tab, 2);
                    assert!(window.try_find("scenario-approval-confirm").is_none());
                }
            })
            .unwrap();
        }
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn results_normal_controls_fit_minimum_and_large_windows(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::AppContext;
        use gpui_kit::test::{TestAppContextExt, TestWindowExt};
        use std::time::Duration;

        for (width, height) in [(1000., 640.), (1080., 720.), (1600., 1000.)] {
            let directory = tempfile::tempdir().unwrap();
            let (handle, view) = crate::desktop::ui::tests::editor_at_size(
                cx,
                directory.path(),
                size(px(width), px(height)),
            );
            let (results, scenario_id) = cx.update(|cx| {
                let editor = view.read(cx);
                (
                    editor.results.clone(),
                    editor.session.suites[0].scenarios[0].id.clone(),
                )
            });
            let run = directory
                .path()
                .join(".regressify/ui-test/bitmaps_test/run-1");
            std::fs::create_dir_all(&run).unwrap();
            lodepng::encode32_file(
                run.join("capture.png"),
                &[30_u8, 150, 180, 255].repeat(120 * 80),
                120,
                80,
            )
            .unwrap();
            std::fs::write(
                run.join("report.json"),
                serde_json::to_vec(&serde_json::json!({
                    "id": "ui-test", "tests": [{"status": "pass", "pair": {
                        "scenarioId": scenario_id,
                        "fileName": format!("{scenario_id}_desktop__FINAL.png"),
                        "label": "A long capture title that must not displace comparison commands",
                        "reference": "../bitmaps_test/run-1/capture.png",
                        "test": "../bitmaps_test/run-1/capture.png",
                        "diffImage": "../bitmaps_test/run-1/capture.png"
                    }}]
                }))
                .unwrap(),
            )
            .unwrap();
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                window.within("workbench-tabs").click(2_usize, cx);
            })
            .unwrap();
            cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
                let panel = results.read(cx);
                !panel.loading && !panel.image_loading && panel.capture.is_some()
            })
            .await;
            for dark in [false, true] {
                cx.update_window(handle.into(), |_, window, cx| {
                    crate::desktop::ui::design::apply_theme(dark, Some(window), cx);
                    window.render_frame(cx);
                    let panel = window.find("results-panel").bounds();
                    let toolbar = window.find("image-toolbar").bounds();
                    let image = window.find(("image-scroll", 1_usize)).bounds();
                    let normal_height = window.rem_size() * 2.;
                    eprintln!(
                        "Results {width}x{height} dark={dark}: header space={:?}, toolbar height={:?}, image fraction={:.3}",
                        image.top() - panel.top(), toolbar.size.height, image.size.height / panel.size.height,
                    );
                    for id in [
                        "results-information", "refresh-results", "open-artifacts",
                        "capture-information", "results-show-capture-log", "copy-capture-details",
                        "edit-capture-suite", "approve-scenario", "fit-image", "fit-width",
                        "zoom-out", "zoom-in", "actual-size",
                    ] {
                        let control = window.find(id);
                        let bounds = control.bounds();
                        assert!(control.visible(), "{id} at {width}x{height} dark={dark}");
                        assert!(bounds.size.height >= normal_height, "{id}: {bounds:?}");
                        assert!(bounds.left() >= panel.left() && bounds.right() <= panel.right(), "{id}: {bounds:?}, panel={panel:?}");
                        assert!(bounds.top() >= panel.top() && bounds.bottom() <= panel.bottom(), "{id}: {bounds:?}, panel={panel:?}");
                    }
                    let search = window.find("capture-search").bounds();
                    assert!(search.size.height >= normal_height, "search={search:?}");
                    let mut controls = (0_usize..4)
                        .map(|index| window.within("image-modes").find(index).bounds())
                        .collect::<Vec<_>>();
                    controls.extend(["fit-image", "fit-width", "zoom-out", "zoom-value", "zoom-in", "actual-size"]
                        .map(|id| window.find(id).bounds()));
                    for (index, bounds) in controls.iter().enumerate() {
                        assert!(bounds.left() >= toolbar.left() && bounds.right() <= toolbar.right(), "control={bounds:?}, toolbar={toolbar:?}");
                        assert!(bounds.top() >= toolbar.top() && bounds.bottom() <= toolbar.bottom(), "control={bounds:?}, toolbar={toolbar:?}");
                        for other in &controls[index + 1..] {
                            assert!(bounds.right() <= other.left() || other.right() <= bounds.left() || bounds.bottom() <= other.top() || other.bottom() <= bounds.top(), "overlapping controls: {bounds:?}, {other:?}");
                        }
                    }
                    assert!(toolbar.bottom() <= image.top());
                    assert!(image.size.height > px(0.));
                    window.click("approve-scenario", cx);
                    for id in ["scenario-approval-cancel", "scenario-approval-confirm"] {
                        let bounds = window.find(id).bounds();
                        assert!(bounds.size.height >= normal_height, "{id}: {bounds:?}");
                        assert!(bounds.left() >= panel.left() && bounds.right() <= panel.right(), "{id}: {bounds:?}, panel={panel:?}");
                    }
                    window.click("scenario-approval-cancel", cx);
                    assert!(!results.read(cx).approval_confirmation);
                })
                .unwrap();
            }
        }
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn results_normal_metadata_controls_preserve_url_gates(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::test::{TestAppContextExt, TestWindowExt};
        use gpui_kit::{AppContext, ClipboardItem};
        use std::time::Duration;

        let urls = [
            format!(
                "https://reference.example.test/{}?value=a%20b&other=%26#raw",
                "long-path/".repeat(24)
            ),
            "https://test.example.test/changed?value=%2F&other=unchanged#test".to_string(),
        ];
        for (width, height) in [(1000., 640.), (1080., 720.), (1600., 1000.)] {
            let directory = tempfile::tempdir().unwrap();
            let run = directory
                .path()
                .join(".regressify/ui-test/bitmaps_test/run-1");
            std::fs::create_dir_all(&run).unwrap();
            std::fs::write(
                run.join("report.json"),
                serde_json::to_vec(&serde_json::json!({"id": "ui-test", "tests": [
                    {"status": "fail", "pair": {"fileName": "valid.png", "scenarioLabel": "Valid URLs", "referenceUrl": urls[0], "url": urls[1], "diff": {"dimensionDifference": {"width": -20, "height": 40}}}},
                    {"status": "fail", "pair": {"fileName": "unsafe.png", "scenarioLabel": "Unsafe URLs", "referenceUrl": "javascript:alert(1)", "url": "data:text/html,unavailable"}}
                ]})).unwrap(),
            ).unwrap();
            let (handle, _) = crate::desktop::ui::tests::editor_at_size(
                cx,
                directory.path(),
                size(px(width), px(height)),
            );
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                window.within("workbench-tabs").click(2_usize, cx);
            })
            .unwrap();
            cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
                window.try_find("capture-information").is_some()
            })
            .await;
            for dark in [false, true] {
                for capture in 0_usize..2 {
                    cx.update_window(handle.into(), |_, window, cx| {
                        crate::desktop::ui::design::apply_theme(dark, Some(window), cx);
                        window.render_frame(cx);
                        window.click(("capture", capture), cx);
                        window.hover("capture-information", cx);
                    })
                    .unwrap();
                    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
                        window.try_find("capture-info-label").is_some_and(|label| {
                            label.label()
                                == Some(if capture == 0 {
                                    "Label: Valid URLs"
                                } else {
                                    "Label: Unsafe URLs"
                                })
                        })
                    })
                    .await;
                    cx.update_window(handle.into(), |_, window, cx| {
                        let bounds = window.find("capture-information-content").bounds();
                        eprintln!("Results metadata {width}x{height} dark={dark} capture={capture}: height={:?}", bounds.size.height);
                        assert!(bounds.left() >= px(0.) && bounds.right() <= px(width), "{bounds:?}");
                        assert!(bounds.top() >= px(0.) && bounds.bottom() <= px(height), "{bounds:?}");
                        let normal_height = window.rem_size() * 2.;
                        for (index, expected) in urls.iter().enumerate() {
                            let url = window.find(("capture-info-url", index)).bounds();
                            let value = window.find(("capture-info-url-value", index)).bounds();
                            let copy = window.find(("capture-info-copy-url", index)).bounds();
                            assert!(url.size.height >= normal_height, "url={url:?}");
                            assert!(copy.size.height >= normal_height, "copy={copy:?}");
                            assert!(value.left() >= bounds.left() && value.right() <= copy.left(), "value={value:?}, copy={copy:?}");
                            assert!(copy.right() <= bounds.right(), "copy={copy:?}, content={bounds:?}");
                            cx.write_to_clipboard(ClipboardItem::new_string("unchanged".into()));
                            window.click(("capture-info-copy-url", index), cx);
                            assert_eq!(cx.read_from_clipboard().unwrap().text().as_deref(), Some(if capture == 0 { expected.as_str() } else { "unchanged" }));
                        }
                        assert_eq!(window.find("capture-info-diff-x").label(), Some(if capture == 0 { "Diff X: -20" } else { "Diff X: Unavailable" }));
                        assert_eq!(window.find("capture-info-diff-y").label(), Some(if capture == 0 { "Diff Y: 40" } else { "Diff Y: Unavailable" }));
                        window.hover("capture-search", cx);
                    }).unwrap();
                    cx.wait_for(handle.into(), Duration::from_secs(3), |window, _| {
                        window.try_find("capture-information-content").is_none()
                    })
                    .await;
                }
            }
        }
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn flat_collection_capture_refresh_retains_key_and_clears_approval(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::test::{TestAppContextExt, TestWindowExt};
        use gpui_kit::{AppContext, Focusable};
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let output = directory.path().join(".regressify");
        let report_path = output.join("ui-test/bitmaps_test/run-1/report.json");
        std::fs::create_dir_all(report_path.parent().unwrap()).unwrap();
        let first = serde_json::json!({"status": "pass", "pair": {"fileName": "first.png", "label": "First"}});
        let second = serde_json::json!({"status": "fail", "pair": {"fileName": "second.png", "label": "Second"}});
        std::fs::write(
            &report_path,
            serde_json::to_vec(&serde_json::json!({
                "id": "ui-test", "tests": [first, second],
            }))
            .unwrap(),
        )
        .unwrap();
        let (handle, view) = crate::desktop::ui::tests::editor(cx, directory.path());
        let results = cx.update(|cx| view.read(cx).results.clone());
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.within("workbench-tabs").click(2_usize, cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
            let panel = results.read(cx);
            !panel.loading && panel.capture.is_some() && !panel.image_loading
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.click(("capture", 1_usize), cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
            let panel = results.read(cx);
            panel.capture == Some(1) && !panel.image_loading
        })
        .await;
        std::fs::write(
            &report_path,
            serde_json::to_vec(&serde_json::json!({
                "id": "ui-test", "tests": [second, first],
            }))
            .unwrap(),
        )
        .unwrap();
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("capture-search", cx);
            results.update(cx, |panel, cx| {
                panel.approval_confirmation = true;
                panel.refresh(output.clone(), window, cx);
                assert!(!panel.approval_confirmation);
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
            let panel = results.read(cx);
            !panel.loading && !panel.image_loading && panel.capture.is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            let panel = results.read(cx);
            let test = &panel.selected_report().unwrap().report.tests[panel.capture.unwrap()];
            assert_eq!(test.pair.capture_key(), "second");
            assert_eq!(panel.capture, Some(0));
            assert!(!panel.approval_confirmation);
            assert!(panel.search.read(cx).focus_handle(cx).is_focused(window));
            assert!(!view.read(cx).session.dirty());
        })
        .unwrap();
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn flat_collection_capture_duplicate_keys_remain_distinct(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::AppContext;
        use gpui_kit::test::{TestAppContextExt, TestWindowExt};
        use std::time::Duration;
        let directory = tempfile::tempdir().unwrap();
        let run = directory
            .path()
            .join(".regressify/ui-test/bitmaps_test/run-1");
        std::fs::create_dir_all(&run).unwrap();
        std::fs::write(run.join("report.json"), serde_json::to_vec(&serde_json::json!({
            "id": "ui-test", "tests": [
                {"status": "pass", "pair": {"fileName": "same.png", "label": "First duplicate"}},
                {"status": "fail", "pair": {"fileName": "same.png", "label": "Second duplicate"}}
            ]
        })).unwrap()).unwrap();
        let (handle, view) = crate::desktop::ui::tests::editor(cx, directory.path());
        let results = cx.update(|cx| view.read(cx).results.clone());
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.within("workbench-tabs").click(2_usize, cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
            let panel = results.read(cx);
            panel.capture == Some(0) && !panel.loading && !panel.image_loading
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.click(("capture", 1_usize), cx);
            assert_eq!(results.read(cx).capture, Some(1));
            window.click(("capture", 0_usize), cx);
            assert_eq!(results.read(cx).capture, Some(0));
            window.click(("capture", 1_usize), cx);
            assert_eq!(results.read(cx).capture, Some(1));
            results.update(cx, |panel, cx| {
                panel.approval_confirmation = true;
                panel.refresh(directory.path().join(".regressify"), window, cx);
                assert!(!panel.approval_confirmation);
            });
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |_, cx| {
            let panel = results.read(cx);
            !panel.loading && !panel.image_loading
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(results.read(cx).capture, Some(0));
            assert!(!results.read(cx).approval_confirmation);
            window.render_frame(cx);
            window.click(("capture", 1_usize), cx);
            assert_eq!(results.read(cx).capture, Some(1));
        })
        .unwrap();
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn preview_loading_keeps_only_the_latest_queued_capture(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::AppContext;
        use gpui_kit::test::{TestAppContextExt, TestWindowExt};
        use std::{sync::Arc, time::Duration};

        let directory = tempfile::tempdir().unwrap();
        let run = directory
            .path()
            .join(".regressify/ui-test/bitmaps_test/run-1");
        std::fs::create_dir_all(&run).unwrap();
        lodepng::encode32_file(run.join("capture.png"), &[30_u8, 150, 180, 255], 1, 1).unwrap();
        let captures = (0..40).map(|index| serde_json::json!({
            "status": "pass", "pair": {"label": format!("Capture {index}"), "test": "../bitmaps_test/run-1/capture.png"}
        })).collect::<Vec<_>>();
        std::fs::write(
            run.join("report.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "ui-test", "tests": captures
            }))
            .unwrap(),
        )
        .unwrap();
        let (handle, view) = crate::desktop::ui::tests::editor(cx, directory.path());
        let results = cx.update(|cx| view.read(cx).results.clone());
        let gate = cx.update(|cx| results.read(cx).image_load_gate.clone());
        let permit = gate.lock().await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.within("workbench-tabs").click(2_usize, cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
            window
                .try_find("capture-heading")
                .is_some_and(|heading| heading.label() == Some("Capture 0"))
        })
        .await;
        for index in 1..40 {
            cx.update_window(handle.into(), |_, window, cx| {
                results.update(cx, |panel, cx| panel.select_capture(index, window, cx));
            })
            .unwrap();
            cx.run_until_parked();
            assert!(
                Arc::strong_count(&gate) <= 4,
                "Superseded image tasks must be dropped"
            );
        }
        drop(permit);
        cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
            window.try_find(("image-details", 1_usize)).is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            assert_eq!(window.find("capture-heading").label(), Some("Capture 39"));
            results.update(cx, |panel, cx| panel.clear(window, cx));
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(Arc::strong_count(&gate), 2);
        cx.update(|cx| {
            assert!(results.read(cx).image_task.is_none());
            assert!(!results.read(cx).image_loading);
            assert!(results.read(cx).previews.is_empty());
        });
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn image_previews_drag_fitted_comparison_canvases_within_lanes(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::test::{TestAppContextExt, TestWindowExt};
        use gpui_kit::{AppContext, InputEvent as _};
        use std::time::Duration;

        for has_diff in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let run = directory
                .path()
                .join(".regressify/ui-test/bitmaps_test/run-1");
            std::fs::create_dir_all(&run).unwrap();
            for (name, width, height) in [
                ("reference", 60, 1000),
                ("test", 90, 500),
                ("diff", 75, 800),
            ] {
                lodepng::encode32_file(
                    run.join(format!("{name}.png")),
                    &[30_u8, 150, 180, 255].repeat(width * height),
                    width,
                    height,
                )
                .unwrap();
            }
            std::fs::write(
                run.join("report.json"),
                serde_json::to_vec(&serde_json::json!({
                    "id": "ui-test", "tests": [{"status": "fail", "pair": {
                        "label": "Shared canvas",
                        "reference": "../bitmaps_test/run-1/reference.png",
                        "test": "../bitmaps_test/run-1/test.png",
                        "diffImage": has_diff.then_some("../bitmaps_test/run-1/diff.png")
                    }}]
                }))
                .unwrap(),
            )
            .unwrap();
            let (handle, view) = crate::desktop::ui::tests::editor(cx, directory.path());
            let count = if has_diff { 3 } else { 2 };
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                window.within("workbench-tabs").click(2_usize, cx);
            })
            .unwrap();
            cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
                window.try_find(("image-details", count - 1)).is_some()
            })
            .await;
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                let results = view.read(cx).results.clone();
                let guides = results.read(cx).pointer_guides.clone();
                for source in 0..count {
                    assert_eq!(results.read(cx).preview_canvas_size(source), (90, 1000));
                    let bounds = guides.bounds[source].get().unwrap();
                    let mouse = bounds.origin
                        + point(bounds.size.width * (80. / 90.), bounds.size.height * 0.95);
                    window.dispatch_event(
                        gpui_kit::MouseMoveEvent {
                            position: mouse,
                            pressed_button: None,
                            modifiers: Default::default(),
                        }
                        .to_platform_input(),
                        cx,
                    );
                    window.render_frame(cx);
                    assert_eq!(guides.active.get(), Some(source));
                    for target in 0..count {
                        let bounds = guides.bounds[target].get().unwrap();
                        let expected = bounds.origin
                            + point(bounds.size.width * (80. / 90.), bounds.size.height * 0.95);
                        let actual = guides
                            .position(target, window.mouse_position())
                            .expect("Shared canvas must show guides beyond smaller images");
                        assert!((actual.x - expected.x).abs() < px(0.1));
                        assert!((actual.y - expected.y).abs() < px(0.1));
                    }
                }
                for control in ["fit-image", "fit-width", "actual-size", "zoom-in"] {
                    window.click(control, cx);
                    window.render_frame(cx);
                    let zoom = window.find("zoom-value").label().unwrap().to_owned();
                    let mode = results.read(cx).preview_views[3].mode;
                    for source in 0..count {
                        for direction in [-1., 1.] {
                            let content = window.find(("image-content", source)).bounds();
                            let viewport = window.find(("image-scroll", source)).bounds();
                            assert!(content.size.width < viewport.size.width);
                            let start = point(content.center().x, viewport.top() + px(12.));
                            window.drag(
                                start,
                                start + point(viewport.size.width * direction, px(0.)),
                                cx,
                            );
                            window.render_frame(cx);
                            assert_eq!(results.read(cx).image_mode, 3);
                            assert!(results.read(cx).preview_views[3].mode == mode);
                            assert_eq!(window.find("zoom-value").label(), Some(zoom.as_str()));
                            assert_eq!(results.read(cx).comparison_scroll.max_offset().x, px(0.));
                            for target in 0..count {
                                let content = window.find(("image-content", target)).bounds();
                                let viewport = window.find(("image-scroll", target)).bounds();
                                let distance = if direction < 0. {
                                    content.left() - viewport.left()
                                } else {
                                    viewport.right() - content.right()
                                };
                                assert!(
                                    distance.abs() <= px(1.),
                                    "{control}, source={source}, target={target}, direction={direction}, distance={distance:?}"
                                );
                            }
                            if control != "fit-image" {
                                window.scroll(
                                    ("image-scroll", source),
                                    gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-40.))),
                                    cx,
                                );
                                window.render_frame(cx);
                                assert!(results.read(cx).comparison_scroll.offset().y < px(0.));
                            }
                        }
                    }
                }
                for hidden in [true, false] {
                    view.update(cx, |editor, cx| {
                        editor.preferences.navigator_hidden = hidden;
                        cx.notify();
                    });
                    window.render_frame(cx);
                    for index in 0..count {
                        let content = window.find(("image-content", index)).bounds();
                        let viewport = window.find(("image-scroll", index)).bounds();
                        assert!((content.right() - viewport.right()).abs() <= px(1.));
                    }
                }
                let before = window.find(("image-content", 0_usize)).bounds();
                let viewport = window.find(("image-scroll", 0_usize)).bounds();
                let pointer = point(before.center().x, viewport.top() + px(30.));
                window.dispatch_event(
                    gpui_kit::ScrollWheelEvent {
                        position: pointer,
                        delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-40.))),
                        modifiers: gpui_kit::Modifiers {
                            control: true,
                            ..Default::default()
                        },
                        ..Default::default()
                    }
                    .to_platform_input(),
                    cx,
                );
                window.render_frame(cx);
                let after = window.find(("image-content", 0_usize)).bounds();
                let anchor = after.origin
                    + point(
                        (pointer.x - before.left()) * (after.size.width / before.size.width),
                        (pointer.y - before.top()) * (after.size.height / before.size.height),
                    );
                assert!((anchor.x - pointer.x).abs() <= px(1.));
                assert!((anchor.y - pointer.y).abs() <= px(1.));
                assert_eq!(window.find("zoom-value").label(), Some("100.0%"));
                window.drag(
                    pointer,
                    pointer - point(viewport.size.width, px(0.)),
                    cx,
                );
                window.render_frame(cx);
                for index in 0..count {
                    let content = window.find(("image-content", index)).bounds();
                    let viewport = window.find(("image-scroll", index)).bounds();
                    assert!((content.left() - viewport.left()).abs() <= px(1.));
                    assert!((content.top() - after.top()).abs() <= px(1.));
                }
                assert_eq!(window.find("zoom-value").label(), Some("100.0%"));
                window.click("fit-image", cx);
                window.render_frame(cx);
                for index in 0..count {
                    let content = window.find(("image-content", index)).bounds();
                    let viewport = window.find(("image-scroll", index)).bounds();
                    assert!((content.center().x - viewport.center().x).abs() <= px(1.));
                }
                window.within("image-modes").click(1_usize, cx);
                window.click("actual-size", cx);
                window.render_frame(cx);
                assert_eq!(
                    window.find(("image-content", 1_usize)).bounds().size,
                    size(px(90.), px(500.))
                );
            })
            .unwrap();
            cx.run_until_parked();
        }
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn image_previews_drag_short_canvases_vertically_within_lanes(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use super::{PreviewView, ZoomMode};
        use gpui_kit::test::{TestAppContextExt, TestWindowExt};
        use gpui_kit::{AppContext, InputEvent as _};
        use std::time::Duration;

        for has_diff in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let run = directory
                .path()
                .join(".regressify/ui-test/bitmaps_test/run-1");
            std::fs::create_dir_all(&run).unwrap();
            for (name, width, height) in [
                ("reference", 1000, 60),
                ("test", 500, 90),
                ("diff", 800, 75),
            ] {
                lodepng::encode32_file(
                    run.join(format!("{name}.png")),
                    &[30_u8, 150, 180, 255].repeat(width * height),
                    width,
                    height,
                )
                .unwrap();
            }
            std::fs::write(
                run.join("report.json"),
                serde_json::to_vec(&serde_json::json!({
                    "id": "ui-test", "tests": [{"status": "fail", "pair": {
                        "label": "Short canvas",
                        "reference": "../bitmaps_test/run-1/reference.png",
                        "test": "../bitmaps_test/run-1/test.png",
                        "diffImage": has_diff.then_some("../bitmaps_test/run-1/diff.png")
                    }}]
                }))
                .unwrap(),
            )
            .unwrap();
            let (handle, view) = crate::desktop::ui::tests::editor(cx, directory.path());
            let count: usize = if has_diff { 3 } else { 2 };
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                window.within("workbench-tabs").click(2_usize, cx);
            })
            .unwrap();
            cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
                window.try_find(("image-details", count - 1)).is_some()
            })
            .await;
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                let results = view.read(cx).results.clone();
                for control in ["fit-image", "fit-width", "actual-size", "zoom-in"] {
                    window.click(control, cx);
                    window.render_frame(cx);
                    let zoom = window.find("zoom-value").label().unwrap().to_owned();
                    let mode = results.read(cx).preview_views[3].mode;
                    for source in 0..count {
                        for direction in [1., -1., 1.] {
                            let before = window.find(("image-content", source)).bounds();
                            let viewport = window.find(("image-scroll", source)).bounds();
                            assert!(before.size.height < viewport.size.height);
                            let start = point(before.left().max(viewport.left()) + px(12.), before.center().y);
                            window.drag(start, start + point(px(0.), viewport.size.height * direction), cx);
                            window.render_frame(cx);
                            assert_eq!(results.read(cx).image_mode, 3);
                            assert!(results.read(cx).preview_views[3].mode == mode);
                            assert_eq!(window.find("zoom-value").label(), Some(zoom.as_str()));
                            assert_eq!(results.read(cx).comparison_scroll.max_offset().y, px(0.));
                            for target in 0..count {
                                let content = window.find(("image-content", target)).bounds();
                                let viewport = window.find(("image-scroll", target)).bounds();
                                let distance = if direction < 0. {
                                    content.top() - viewport.top()
                                } else { viewport.bottom() - content.bottom() };
                                assert!(distance.abs() <= px(1.), "{control}, source={source}, target={target}, direction={direction}, distance={distance:?}");
                                assert!((content.size.width - before.size.width).abs() <= px(1.));
                                assert!((content.size.height - before.size.height).abs() <= px(1.));
                            }
                        }
                        if matches!(control, "actual-size" | "zoom-in") {
                            window.scroll(("image-scroll", source), gpui_kit::ScrollDelta::Pixels(point(px(-40.), px(0.))), cx);
                            window.render_frame(cx);
                            assert!(results.read(cx).comparison_scroll.offset().x < px(0.));
                        }
                    }
                }
                for height in [1000., 640.] {
                    window.resize(size(px(1200.), px(height)));
                    window.bounds_changed(cx);
                    window.render_frame(cx);
                    for index in 0..count {
                        let content = window.find(("image-content", index)).bounds();
                        let viewport = window.find(("image-scroll", index)).bounds();
                        assert!((content.bottom() - viewport.bottom()).abs() <= px(1.));
                    }
                    assert_eq!(results.read(cx).comparison_scroll.max_offset().y, px(0.));
                }
                let before = window.find(("image-content", 0_usize)).bounds();
                let viewport = window.find(("image-scroll", 0_usize)).bounds();
                let pointer = point(viewport.left() + px(30.), before.center().y);
                window.dispatch_event(gpui_kit::ScrollWheelEvent {
                    position: pointer,
                    delta: gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-40.))),
                    modifiers: gpui_kit::Modifiers { control: true, ..Default::default() },
                    ..Default::default()
                }.to_platform_input(), cx);
                window.render_frame(cx);
                let after = window.find(("image-content", 0_usize)).bounds();
                let anchor = after.origin + point(
                    (pointer.x - before.left()) * (after.size.width / before.size.width),
                    (pointer.y - before.top()) * (after.size.height / before.size.height),
                );
                assert!((anchor.x - pointer.x).abs() <= px(1.));
                assert!((anchor.y - pointer.y).abs() <= px(1.));
                window.drag(pointer, pointer - point(px(0.), viewport.size.height), cx);
                window.render_frame(cx);
                for index in 0..count {
                    let content = window.find(("image-content", index)).bounds();
                    let viewport = window.find(("image-scroll", index)).bounds();
                    assert!((content.top() - viewport.top()).abs() <= px(1.));
                }
                for control in ["fit-image", "fit-width"] {
                    window.click(control, cx);
                    window.render_frame(cx);
                    assert!(results.read(cx).preview_views[3].vertical_position.is_none());
                }
                // Both underfilled axes must move together, not overwrite one another.
                results.update(cx, |panel, cx| {
                    panel.preview_views[3] = PreviewView { mode: ZoomMode::Manual, zoom: 0.1, ..Default::default() };
                    panel.comparison_scroll.set_offset(point(px(0.), px(0.)));
                    cx.notify();
                });
                window.render_frame(cx);
                for direction in [1., -1.] {
                    let content = window.find(("image-content", 0_usize)).bounds();
                    let viewport = window.find(("image-scroll", 0_usize)).bounds();
                    window.drag(content.center(), content.center() + point(viewport.size.width * direction, viewport.size.height * direction), cx);
                    window.render_frame(cx);
                    for index in 0..count {
                        let content = window.find(("image-content", index)).bounds();
                        let viewport = window.find(("image-scroll", index)).bounds();
                        let distance = if direction < 0. { content.origin - viewport.origin } else { viewport.bottom_right() - content.bottom_right() };
                        assert!(distance.x.abs() <= px(1.) && distance.y.abs() <= px(1.), "{distance:?}");
                    }
                    assert_eq!(results.read(cx).comparison_scroll.max_offset(), point(px(0.), px(0.)));
                }
            }).unwrap();
            cx.run_until_parked();
        }
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn crosshairs_follow_generated_diff_bands_from_every_pane(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use gpui_kit::test::{TestAppContextExt, TestWindowExt};
        use gpui_kit::{AppContext, InputEvent as _};
        use std::time::Duration;

        let gaps = vec![
            VerticalGap {
                start: 100,
                height: 24,
            },
            VerticalGap {
                start: 200,
                height: 24,
            },
        ];
        for shorter in [0, 1] {
            let directory = tempfile::tempdir().unwrap();
            let run = directory
                .path()
                .join(".regressify/ui-test/bitmaps_test/run-1");
            std::fs::create_dir_all(&run).unwrap();
            let pixels: Vec<_> = (0..320 * 400)
                .map(|index| {
                    let row = index / 320;
                    let col = index % 320;
                    rgb::RGBA8::new(
                        ((row * 73 + col * 17) % 256) as u8,
                        ((row * 151 + col * 43) % 256) as u8,
                        ((row * 199 + col * 97) % 256) as u8,
                        255,
                    )
                })
                .collect();
            let mut taller = pixels[..gaps[0].start * 320].to_vec();
            taller.extend(vec![rgb::RGBA8::new(0, 0, 0, 255); gaps[0].height * 320]);
            taller.extend_from_slice(&pixels[gaps[0].start * 320..gaps[1].start * 320]);
            taller.extend(vec![
                rgb::RGBA8::new(255, 255, 255, 255);
                gaps[1].height * 320
            ]);
            taller.extend_from_slice(&pixels[gaps[1].start * 320..]);
            for (index, name) in ["reference", "test"].iter().enumerate() {
                let (pixels, height) = if index == shorter {
                    (&pixels, 400)
                } else {
                    (&taller, 448)
                };
                lodepng::encode32_file(run.join(format!("{name}.png")), pixels, 320, height)
                    .unwrap();
            }
            let (comparison, diff) = crate::compare::image_compare::compare_images(
                &run.join("reference.png"),
                &run.join("test.png"),
            )
            .unwrap();
            assert_eq!(
                if shorter == 0 {
                    comparison.alignment.reference_gaps()
                } else {
                    comparison.alignment.test_gaps()
                },
                gaps.as_slice()
            );
            std::fs::write(run.join("diff.png"), diff.unwrap()).unwrap();
            std::fs::write(run.join("report.json"), serde_json::to_vec(&serde_json::json!({
                "id": "ui-test", "tests": [{"status": "fail", "pair": {
                    "label": "Aligned band fixture",
                    "reference": "../bitmaps_test/run-1/reference.png",
                    "test": "../bitmaps_test/run-1/test.png",
                    "diffImage": "../bitmaps_test/run-1/diff.png",
                    "diff": {"alignment": comparison.alignment, "rawMisMatchPercentage": comparison.raw_mis_match_percentage}
                }}]
            })).unwrap()).unwrap();
            let (handle, view) = crate::desktop::ui::tests::editor(cx, directory.path());
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                window.within("workbench-tabs").click(2_usize, cx);
            })
            .unwrap();
            cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
                window.try_find(("image-details", 2_usize)).is_some()
            })
            .await;
            cx.update_window(handle.into(), |_, window, cx| {
                window.render_frame(cx);
                let results = view.read(cx).results.clone();
                let guides = results.read(cx).pointer_guides.clone();
                assert_eq!(
                    guides.geometry[shorter].borrow().as_ref().unwrap().gaps,
                    gaps
                );
                for source in 0..3 {
                    for row in [50., 112., 160., 236., 300.] {
                        let Some(source_row) = (if source == shorter {
                            gaps_to_image_row(&gaps, row)
                        } else {
                            Some(row)
                        }) else {
                            continue;
                        };
                        window.hover(("image-content", source), cx);
                        window.render_frame(cx);
                        let bounds = guides.bounds[source].get().unwrap();
                        let mouse = bounds.origin
                            + point(
                                bounds.size.width * 0.5,
                                bounds.size.height * (source_row / 448.),
                            );
                        window.dispatch_event(
                            gpui_kit::MouseMoveEvent {
                                position: mouse,
                                pressed_button: None,
                                modifiers: Default::default(),
                            }
                            .to_platform_input(),
                            cx,
                        );
                        window.render_frame(cx);
                        assert_eq!(guides.active.get(), Some(source));
                        for target in 0..3 {
                            let expected_row = if target == shorter {
                                gaps_to_image_row(&gaps, row)
                            } else {
                                Some(row)
                            };
                            let actual = guides.position(target, window.mouse_position());
                            assert_eq!(
                                window.find(("image-pointer-layer", target)).bounds(),
                                window.find(("image-scroll", target)).bounds()
                            );
                            if let Some(row) = expected_row {
                                let bounds = guides.bounds[target].get().unwrap();
                                let expected = bounds.origin
                                    + point(
                                        bounds.size.width * 0.5,
                                        bounds.size.height * (row / 448.),
                                    );
                                let actual = actual
                                    .expect("Generated band metadata must map to the target pixel");
                                assert!((actual.x - expected.x).abs() < px(0.1));
                                assert!((actual.y - expected.y).abs() < px(0.1));
                            } else {
                                assert!(actual.is_none());
                            }
                        }
                    }
                }
                results.update(cx, |panel, cx| panel.clear(window, cx));
                assert!(guides.alignment.borrow().is_none());
                assert!(
                    guides
                        .geometry
                        .iter()
                        .all(|geometry| geometry.borrow().is_none())
                );
            })
            .unwrap();
            cx.run_until_parked();
        }
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    async fn crosshairs_follow_either_pane_and_clear_on_exit(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::test::{TestAppContextExt, TestWindowExt};
        use gpui_kit::{AppContext, InputEvent as _};
        use std::time::Duration;

        let directory = tempfile::tempdir().unwrap();
        let run = directory
            .path()
            .join(".regressify/ui-test/bitmaps_test/run-1");
        std::fs::create_dir_all(&run).unwrap();
        lodepng::encode32_file(
            run.join("capture.png"),
            &[30_u8, 150, 180, 255].repeat(1200 * 1800),
            1200,
            1800,
        )
        .unwrap();
        std::fs::write(
            run.join("report.json"),
            serde_json::to_vec(&serde_json::json!({
                "id": "ui-test", "tests": [{"status": "pass", "pair": {
                    "label": "Pointer fixture",
                    "reference": "../bitmaps_test/run-1/capture.png",
                    "test": "../bitmaps_test/run-1/capture.png"
                }}]
            }))
            .unwrap(),
        )
        .unwrap();
        let (handle, view) = crate::desktop::ui::tests::editor(cx, directory.path());
        cx.update_window(handle.into(), |_, window, cx| {
            window.render_frame(cx);
            window.within("workbench-tabs").click(2_usize, cx);
        })
        .unwrap();
        cx.wait_for(handle.into(), Duration::from_secs(5), |window, _| {
            window.try_find(("image-details", 1_usize)).is_some()
        })
        .await;
        cx.update_window(handle.into(), |_, window, cx| {
            window.within("image-modes").click(3_usize, cx);
            window.click("actual-size", cx);
            let results = view.read(cx).results.clone();
            let filtered = results.read(cx).visible_captures.as_ptr();
            for _ in 0..20 {
                window.render_frame(cx);
                assert_eq!(results.read(cx).visible_captures.as_ptr(), filtered);
                assert_eq!(results.read(cx).visible_captures.as_slice(), &[0]);
            }
            for id in [
                "previous-capture",
                "next-capture",
                "previous-failed-capture",
                "next-failed-capture",
                "previous-captures",
                "next-captures",
                "capture-position",
            ] {
                assert!(window.try_find(id).is_none());
            }
            let assert_position =
                |actual: Option<gpui_kit::Point<gpui_kit::Pixels>>,
                 expected: gpui_kit::Point<gpui_kit::Pixels>| {
                    let actual = actual.expect("Pointer guide must be visible");
                    assert!((actual.x - expected.x).abs() < px(0.01));
                    assert!((actual.y - expected.y).abs() < px(0.01));
                };
            for index in [0_usize, 1] {
                window.hover(("image-scroll", index), cx);
                window.render_frame(cx);
                let guides = results.read(cx).pointer_guides.clone();
                assert_eq!(guides.active.get(), Some(index));
                let mouse = window.mouse_position();
                let source = guides.bounds[index].get().unwrap();
                let target = guides.bounds[1 - index].get().unwrap();
                assert_position(guides.position(index, mouse), mouse);
                assert_position(
                    guides.position(1 - index, mouse),
                    mouse - source.origin + target.origin,
                );
                window.scroll(
                    ("image-scroll", index),
                    gpui_kit::ScrollDelta::Pixels(point(px(-90.), px(-140.))),
                    cx,
                );
                window.render_frame(cx);
                assert_eq!(window.mouse_position(), mouse);
                assert_eq!(
                    window.find(("image-pointer-layer", index)).bounds(),
                    window.find(("image-scroll", index)).bounds()
                );
                let source = guides.bounds[index].get().unwrap();
                let target = guides.bounds[1 - index].get().unwrap();
                assert_position(
                    guides.position(1 - index, mouse),
                    mouse - source.origin + target.origin,
                );
            }
            let image = results.read(cx).previews[0]
                .result
                .as_ref()
                .unwrap()
                .image
                .clone();
            let offset = results.read(cx).comparison_scroll.offset();
            window.click(("capture", 0_usize), cx);
            assert!(std::sync::Arc::ptr_eq(
                &image,
                &results.read(cx).previews[0].result.as_ref().unwrap().image
            ));
            assert_eq!(results.read(cx).comparison_scroll.offset(), offset);
            assert!(!results.read(cx).image_loading);
            window.hover("actual-size", cx);
            window.render_frame(cx);
            let guides = results.read(cx).pointer_guides.clone();
            assert_eq!(guides.active.get(), None);
            assert!(guides.position(0, window.mouse_position()).is_none());
            assert!(guides.position(1, window.mouse_position()).is_none());
        })
        .unwrap();
        cx.run_until_parked();
        let results = cx.update(|cx| view.read(cx).results.clone());
        let notifications = std::rc::Rc::new(std::cell::Cell::new(0));
        let observed = notifications.clone();
        let _subscription = cx.update(|cx| {
            cx.observe(&results, move |_, _| {
                observed.set(observed.get() + 1);
            })
        });
        for index in [0_usize, 1] {
            cx.update_window(handle.into(), |_, window, cx| {
                window.hover(("image-scroll", index), cx);
            })
            .unwrap();
            cx.run_until_parked();
            notifications.set(0);
            for step in 1..=20 {
                cx.update_window(handle.into(), |_, window, cx| {
                    let origin = window.find(("image-scroll", index)).bounds().center();
                    window.dispatch_event(
                        gpui_kit::MouseMoveEvent {
                            position: origin + point(px(step as f32), px(step as f32)),
                            pressed_button: None,
                            modifiers: Default::default(),
                        }
                        .to_platform_input(),
                        cx,
                    );
                    window.render_frame(cx);
                })
                .unwrap();
                cx.run_until_parked();
            }
            assert_eq!(
                notifications.get(),
                0,
                "Pointer movement within pane {index} must not invalidate Results or its navigation observers"
            );
        }
        cx.update_window(handle.into(), |_, window, cx| {
            window.click("fit-image", cx);
            for index in [0_usize, 1] {
                window.hover(("image-content", index), cx);
                window.render_frame(cx);
                let guides = results.read(cx).pointer_guides.clone();
                assert_eq!(guides.active.get(), Some(index));
                let mouse = window.mouse_position();
                let source = window.find(("image-content", index)).bounds();
                let target = window.find(("image-content", 1 - index)).bounds();
                assert_eq!(guides.bounds[index].get(), Some(source));
                assert_eq!(guides.position(index, mouse), Some(mouse));
                assert_eq!(
                    guides.position(1 - index, mouse),
                    Some(mouse - source.origin + target.origin)
                );
            }
            results.update(cx, |panel, cx| {
                panel.preview_views[3] = super::PreviewView {
                    mode: super::ZoomMode::Manual,
                    zoom: 0.1,
                    horizontal_position: Some(0.5),
                    vertical_position: Some(0.5),
                    ..Default::default()
                };
                panel.comparison_scroll.set_offset(point(px(0.), px(0.)));
                cx.notify();
            });
            window.render_frame(cx);
            for index in [0_usize, 1] {
                let viewport = window.find(("image-scroll", index)).bounds();
                assert_eq!(
                    window.find(("image-pointer-layer", index)).bounds(),
                    viewport
                );
                for mouse in [
                    viewport.origin + point(px(12.), px(12.)),
                    viewport.bottom_right() - point(px(12.), px(12.)),
                ] {
                    let source = window.find(("image-content", index)).bounds();
                    assert!(!source.contains(&mouse));
                    window.dispatch_event(
                        gpui_kit::MouseMoveEvent {
                            position: mouse,
                            ..Default::default()
                        }
                        .to_platform_input(),
                        cx,
                    );
                    window.render_frame(cx);
                    let guides = results.read(cx).pointer_guides.clone();
                    assert_eq!(guides.active.get(), Some(index));
                    for target in 0..2 {
                        let bounds = window.find(("image-content", target)).bounds();
                        let expected = mouse - source.origin + bounds.origin;
                        let actual = guides
                            .position(target, mouse)
                            .expect("Blank lane space must retain its crosshair");
                        assert!((actual.x - expected.x).abs() < px(0.01));
                        assert!((actual.y - expected.y).abs() < px(0.01));
                    }
                }
                assert_eq!(
                    results.read(cx).comparison_scroll.max_offset(),
                    point(px(0.), px(0.))
                );
            }
            window.hover("actual-size", cx);
            window.render_frame(cx);
            assert_eq!(results.read(cx).pointer_guides.active.get(), None);
        })
        .unwrap();
        let released = cx
            .update_window(handle.into(), |_, window, cx| {
                let source = gpui_kit::ImageSource::from(
                    results.read(cx).previews[1]
                        .result
                        .as_ref()
                        .unwrap()
                        .image
                        .clone(),
                );
                let decoded = results.read(cx).preview_textures[1]
                    .0
                    .borrow()
                    .as_ref()
                    .map(std::sync::Arc::downgrade)
                    .expect("Displayed image should be decoded");
                assert!(
                    source.is_asset_cached(cx),
                    "Displayed images should have reached GPUI's decoder cache"
                );
                results.update(cx, |panel, cx| panel.clear(window, cx));
                assert!(
                    !source.is_asset_cached(cx),
                    "Clearing previews must evict their decoded image assets"
                );
                assert!(
                    results
                        .read(cx)
                        .preview_textures
                        .iter()
                        .all(|texture| texture.0.borrow().is_none())
                );
                window.render_frame(cx);
                decoded
            })
            .unwrap();
        cx.run_until_parked();
        assert!(
            released.upgrade().is_none(),
            "Decoded pixels must be freed after obsolete image frames are dropped"
        );
    }

    #[cfg(feature = "gui-tests")]
    #[gpui_kit::test]
    fn released_results_evict_preview_assets(cx: &mut gpui_kit::TestAppContext) {
        use gpui_kit::AppContext;
        use std::sync::Arc;

        let directory = tempfile::tempdir().unwrap();
        let (handle, _) = crate::desktop::ui::tests::editor(cx, directory.path());
        let encoded = Arc::new(gpui_kit::Image::from_bytes(
            gpui_kit::ImageFormat::Png,
            lodepng::encode32(&[30_u8, 150, 180, 255], 1, 1).unwrap(),
        ));
        let panel = cx
            .update_window(handle.into(), |_, window, cx| {
                let panel = cx.new(|cx| super::ResultsPanel::new(window, cx));
                panel.update(cx, |panel, _| {
                    panel.previews.push(super::Preview {
                        label: "Current",
                        result: Ok(super::LoadedImage {
                            image: encoded.clone(),
                            width: 1,
                            height: 1,
                            path: Some(directory.path().join("fixture.png")),
                        }),
                    })
                });
                encoded.clone().get_render_image(window, cx);
                assert!(encoded.is_asset_cached(cx));
                panel
            })
            .unwrap();
        drop(panel);
        cx.run_until_parked();
        cx.update(|cx| {
            assert!(
                !encoded.is_asset_cached(cx),
                "Releasing Results must remove its preview assets"
            )
        });
    }
}
