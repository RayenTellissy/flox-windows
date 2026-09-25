//! The shell's ingest side: the Details action bar and selection, the paste links,
//! local files and 4KHDHub dialogs, the Queue screen and the Library manager.
//!
//! A child module of `app` so it can extend [`Shell`] without widening its fields.
//! The services sit behind small traits ([`JobQueue`], [`LibraryAdmin`],
//! [`HubSource`], [`FilePicker`]) so snapshots and `--dev-fixtures` can run without
//! Telegram, ffmpeg, the network or a native picker.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use flox_core::error::{Error, Result};
use flox_core::model::{MediaType, TitleDetails, TmdbId};
use flox_rip::hub::{self, Variant};
use flox_rip::job::{Job, JobView};
use flox_rip::queue::{Queue, QueueHooks};
use flox_sys::power::KeepAwake;
use flox_td::library::{Entry, Library};
use parking_lot::Mutex;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel};
use tokio::sync::watch;
use uuid::Uuid;

use super::{AppContext, Shell, Telegram};
use crate::focus::{Focus, FocusGraph, Modifiers, ZoneId};
use crate::ui::{
    DetailsState, FileRow, FilesState, FocusState, HomeState, HubRow, HubState, IngestBar,
    LibraryEntry, LibraryGroup, LibraryState, LinkRow, PasteState, QueueRow, QueueState, Screen,
};
use crate::vm::details as dvm;
use crate::vm::ingest::{
    self as ivm, Footer, HubPicker, LocalFiles, PasteLinks, Selection, Target,
};
use crate::vm::library as lvm;
use crate::vm::queue as qvm;

// ---------------------------------------------------------------------------
// Services

/// The job queue as the screens use it.
pub trait JobQueue: Send + Sync {
    fn add(&self, jobs: Vec<Job>);
    fn cancel(&self, id: Uuid);
    fn retry(&self, id: Uuid);
    fn remove(&self, id: Uuid);
    fn clear_finished(&self);
    fn snapshot(&self) -> watch::Receiver<Vec<JobView>>;
}

impl JobQueue for Queue {
    fn add(&self, jobs: Vec<Job>) {
        Queue::add(self, jobs);
    }

    fn cancel(&self, id: Uuid) {
        Queue::cancel(self, id);
    }

    fn retry(&self, id: Uuid) {
        Queue::retry(self, id);
    }

    fn remove(&self, id: Uuid) {
        Queue::remove(self, id);
    }

    fn clear_finished(&self) {
        Queue::clear_finished(self);
    }

    fn snapshot(&self) -> watch::Receiver<Vec<JobView>> {
        Queue::snapshot(self)
    }
}

/// Lists and deletes the channel's prints.
#[async_trait]
pub trait LibraryAdmin: Send + Sync {
    async fn entries(&self, channel_title: &str) -> Result<Vec<Entry>>;
    async fn delete(&self, entry: &Entry) -> Result<()>;
}

#[async_trait]
impl LibraryAdmin for Library {
    async fn entries(&self, channel_title: &str) -> Result<Vec<Entry>> {
        let index = self.refresh(channel_title).await?;
        Ok(index.all().cloned().collect())
    }

    async fn delete(&self, entry: &Entry) -> Result<()> {
        self.delete_entry(entry).await
    }
}

/// 4KHDHub lookups.
#[async_trait]
pub trait HubSource: Send + Sync {
    /// The title's page URL, if the site has it.
    async fn find(&self, title: &TitleDetails) -> Result<Option<String>>;
    /// The variants on a page, best first.
    async fn variants(&self, page: &str, media: MediaType) -> Result<Vec<Variant>>;
}

/// The real site, over HTTPS.
pub struct FourKHdHub {
    http: reqwest::Client,
}

impl FourKHdHub {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        Ok(Self { http })
    }
}

#[async_trait]
impl HubSource for FourKHdHub {
    async fn find(&self, title: &TitleDetails) -> Result<Option<String>> {
        Ok(hub::find(&self.http, title).await?.map(String::from))
    }

    async fn variants(&self, page: &str, media: MediaType) -> Result<Vec<Variant>> {
        let url = url::Url::parse(page).map_err(|e| Error::Other(format!("bad URL: {e}")))?;
        hub::variants(&self.http, &url, media).await
    }
}

/// No 4KHDHub (the HTTP client could not be built).
pub struct NoHub;

#[async_trait]
impl HubSource for NoHub {
    async fn find(&self, _title: &TitleDetails) -> Result<Option<String>> {
        Err(Error::Unavailable("4KHDHub is not available".into()))
    }

    async fn variants(&self, _page: &str, _media: MediaType) -> Result<Vec<Variant>> {
        Err(Error::Unavailable("4KHDHub is not available".into()))
    }
}

/// The native file picker.
#[async_trait]
pub trait FilePicker: Send + Sync {
    /// Chosen files; empty when cancelled.
    async fn pick(&self, multi: bool) -> Vec<PathBuf>;
}

/// `flox_sys::dialogs::pick_files`.
pub struct SystemPicker;

#[async_trait]
impl FilePicker for SystemPicker {
    async fn pick(&self, multi: bool) -> Vec<PathBuf> {
        flox_sys::dialogs::pick_files(multi).await
    }
}

/// Everything the ingest screens use.
pub struct Ingest {
    /// Present when Telegram and ffmpeg are available (or with fixtures).
    pub queue: Option<Arc<dyn JobQueue>>,
    /// Present when Telegram is.
    pub admin: Option<Arc<dyn LibraryAdmin>>,
    pub hub: Arc<dyn HubSource>,
    pub picker: Arc<dyn FilePicker>,
}

impl Ingest {
    /// No queue and no channel: Details has no action bar, the Library manager asks to
    /// connect Telegram.
    pub fn none() -> Self {
        Self {
            queue: None,
            admin: None,
            hub: Arc::new(NoHub),
            picker: Arc::new(SystemPicker),
        }
    }

    /// From the app's services. With fixtures (offline Telegram) the queue is an
    /// in-memory list that keeps jobs queued, so the ingest screens can be tried.
    pub fn from_context(ctx: &AppContext) -> Self {
        let offline = matches!(ctx.services.telegram, Telegram::Offline { .. });
        let queue: Option<Arc<dyn JobQueue>> = match &ctx.queue {
            Some(q) => Some(q.clone()),
            None if offline => Some(Arc::new(crate::fixtures::FixtureQueue::default())),
            None => None,
        };
        let admin = ctx.library.clone().map(|l| -> Arc<dyn LibraryAdmin> { l });
        let hub: Arc<dyn HubSource> = match FourKHdHub::new() {
            Ok(h) => Arc::new(h),
            Err(e) => {
                tracing::warn!("4KHDHub client: {e}");
                Arc::new(NoHub)
            }
        };
        Self {
            queue,
            admin,
            hub,
            picker: Arc::new(SystemPicker),
        }
    }
}

/// Queue side effects: keep the machine awake while busy, toast when drained. The
/// library refresh after a drain is done by the shell, which sees the drain in the
/// job list.
#[derive(Default)]
pub struct QueueEvents {
    awake: Mutex<Option<KeepAwake>>,
}

impl QueueHooks for QueueEvents {
    fn busy_changed(&self, busy: bool) {
        *self.awake.lock() = busy.then(KeepAwake::system);
    }

    fn drained(&self, any_failed: bool) {
        let title = if any_failed {
            "Queue finished with failures"
        } else {
            "Queue finished"
        };
        if let Err(e) = flox_sys::notify::toast(title, "") {
            tracing::warn!("toast: {e}");
        }
    }
}

// ---------------------------------------------------------------------------
// Shell state

enum HubPhase {
    /// Searching or reading a page; the text is the stamp.
    Busy(String),
    /// Not found or failed: a URL field and LOAD under the status.
    Url(String),
    Loaded(HubPicker),
}

enum Dialog {
    Paste(Target, PasteLinks),
    Files(Target, LocalFiles),
    Hub {
        target: Target,
        phase: HubPhase,
        url: String,
    },
}

struct Manager {
    load: lvm::Load,
    entries: Vec<Entry>,
    names: HashMap<(TmdbId, MediaType), String>,
    /// The prints in row order.
    flat: Vec<Entry>,
    generation: u64,
    /// The print the confirm dialog is about.
    confirm: Option<usize>,
}

impl Default for Manager {
    fn default() -> Self {
        Self {
            load: lvm::Load::Reading,
            entries: Vec::new(),
            names: HashMap::new(),
            flat: Vec::new(),
            generation: 0,
            confirm: None,
        }
    }
}

#[derive(Default)]
struct State {
    /// Details shows the action bar and selection marks.
    mode: bool,
    selection: Selection,
    /// `3 QUEUED` after an action.
    note: String,
    dialog: Option<Dialog>,
    /// Bumped per dialog; late picker or hub results for an older one are dropped.
    dialog_generation: u64,
    /// The queue list has been rendered at least once.
    queue_seen: bool,
    views: Vec<JobView>,
    rows: Vec<qvm::Row>,
    manager: Manager,
}

/// The ingest part of the shell, one field of [`Shell`].
#[derive(Default)]
pub(super) struct IngestShell {
    backend: RefCell<Option<Ingest>>,
    state: RefCell<State>,
    paste_rows: Rc<VecModel<LinkRow>>,
    file_rows: Rc<VecModel<FileRow>>,
    hub_rows: Rc<VecModel<HubRow>>,
    queue_rows: Rc<VecModel<QueueRow>>,
    library_groups: Rc<VecModel<LibraryGroup>>,
}

fn link_row(r: &ivm::LinkRow) -> LinkRow {
    LinkRow {
        episode: r.episode.as_str().into(),
        url: r.url.as_str().into(),
    }
}

fn file_row(r: &ivm::FileRow) -> FileRow {
    FileRow {
        episode: r.episode.as_str().into(),
        name: ivm::middle_elide(&r.name, 56).into(),
        tag: ivm::tag_label(r.tag).into(),
    }
}

fn queue_row(r: &qvm::Row) -> QueueRow {
    let actions: Vec<SharedString> = r.actions.iter().map(|a| a.label().into()).collect();
    QueueRow {
        label: r.label.as_str().into(),
        state: r.state.as_str().into(),
        tone: match r.tone {
            qvm::Tone::Normal => 0,
            qvm::Tone::Success => 1,
            qvm::Tone::Alert => 2,
        },
        detail: r.detail.as_str().into(),
        progress: r.progress,
        actions: ModelRc::new(VecModel::from(actions)),
    }
}

fn index_i32(i: usize) -> i32 {
    i32::try_from(i).unwrap_or(i32::MAX)
}

impl Shell {
    /// Connects the ingest globals and callbacks. Called from `bind`.
    pub(super) fn bind_ingest(self: &Rc<Self>, ui: &crate::ui::AppWindow) {
        let i = &self.ingest;
        ui.global::<PasteState>()
            .set_rows(ModelRc::from(i.paste_rows.clone()));
        ui.global::<FilesState>()
            .set_rows(ModelRc::from(i.file_rows.clone()));
        ui.global::<HubState>()
            .set_variants(ModelRc::from(i.hub_rows.clone()));
        ui.global::<QueueState>()
            .set_rows(ModelRc::from(i.queue_rows.clone()));
        ui.global::<LibraryState>()
            .set_groups(ModelRc::from(i.library_groups.clone()));

        let paste = ui.global::<PasteState>();
        let shell = self.clone();
        paste.on_episode_edited(move |row, text| {
            shell.paste_edited(row, &text, true);
        });
        let shell = self.clone();
        paste.on_url_edited(move |row, text| {
            shell.paste_edited(row, &text, false);
        });
        let shell = self.clone();
        paste.on_accepted(move || shell.queue_dialog());

        let shell = self.clone();
        ui.global::<FilesState>()
            .on_episode_edited(move |row, text| shell.files_edited(row, &text));

        let hub = ui.global::<HubState>();
        let shell = self.clone();
        hub.on_url_edited(move |text| {
            if let Some(Dialog::Hub { url, .. }) = &mut shell.ingest.state.borrow_mut().dialog {
                *url = text.to_string();
            }
        });
        let shell = self.clone();
        hub.on_url_accepted(move || shell.hub_load_typed());

        // Right click (MENU) on an episode toggles its selection in ingest mode.
        let shell = self.clone();
        ui.global::<FocusState>().on_menu(move |zone, index| {
            if let Ok(index) = usize::try_from(index) {
                if shell.screen() == Screen::Details && ZoneId(zone) == dvm::EPISODES {
                    shell.toggle_episode(index);
                }
            }
        });
    }

    /// Hands the shell its ingest services and starts following the queue.
    pub fn set_ingest(self: &Rc<Self>, ingest: Ingest) {
        let rx = ingest.queue.as_ref().map(|q| q.snapshot());
        *self.ingest.backend.borrow_mut() = Some(ingest);
        if let Some(rx) = rx {
            self.pull_queue();
            let shell = self.clone();
            self.exec.watch(rx, move |views| shell.on_queue(views));
        }
    }

    fn job_queue(&self) -> Option<Arc<dyn JobQueue>> {
        self.ingest
            .backend
            .borrow()
            .as_ref()
            .and_then(|b| b.queue.clone())
    }

    fn admin(&self) -> Option<Arc<dyn LibraryAdmin>> {
        self.ingest
            .backend
            .borrow()
            .as_ref()
            .and_then(|b| b.admin.clone())
    }

    fn hub_source(&self) -> Arc<dyn HubSource> {
        self.ingest.backend.borrow().as_ref().map_or_else(
            || -> Arc<dyn HubSource> { Arc::new(NoHub) },
            |b| b.hub.clone(),
        )
    }

    fn picker(&self) -> Arc<dyn FilePicker> {
        self.ingest.backend.borrow().as_ref().map_or_else(
            || -> Arc<dyn FilePicker> { Arc::new(SystemPicker) },
            |b| b.picker.clone(),
        )
    }

    // -- keys ---------------------------------------------------------------

    /// Space/X toggles the focused episode, A selects all, N none (Details, ingest
    /// mode, outside text fields and dialogs). `Some(true)` when handled.
    pub(super) fn ingest_key(self: &Rc<Self>, text: &str, m: Modifiers) -> Option<bool> {
        if self.screen() != Screen::Details || m.control || m.alt || m.meta {
            return None;
        }
        {
            let graphs = self.graphs.borrow();
            if graphs.details.editing() || graphs.details.in_modal() {
                return None;
            }
        }
        {
            let st = self.ingest.state.borrow();
            let tv = self
                .details
                .borrow()
                .details
                .as_ref()
                .is_some_and(|d| d.summary.media == MediaType::Tv);
            if !st.mode || !tv {
                return None;
            }
        }
        let mut chars = text.chars();
        let key = chars.next()?.to_ascii_lowercase();
        if chars.next().is_some() {
            return None;
        }
        match key {
            ' ' | 'x' => {
                let focus = self.focus()?;
                if focus.zone != dvm::EPISODES {
                    return None;
                }
                self.toggle_episode(focus.index);
            }
            'a' => {
                let all: Vec<u32> = self
                    .details
                    .borrow()
                    .episodes
                    .iter()
                    .map(|e| e.episode)
                    .collect();
                self.ingest.state.borrow_mut().selection.select_all(all);
                self.render_selection();
            }
            'n' => {
                self.ingest.state.borrow_mut().selection.clear();
                self.render_selection();
            }
            _ => return None,
        }
        Some(true)
    }

    /// BACK closes an open dialog first. True when it did.
    pub(super) fn ingest_back(self: &Rc<Self>) -> bool {
        match self.screen() {
            Screen::Details if self.ingest.state.borrow().dialog.is_some() => {
                self.close_dialog();
                true
            }
            Screen::Library if self.ingest.state.borrow().manager.confirm.is_some() => {
                self.close_confirm();
                true
            }
            _ => false,
        }
    }

    // -- details ------------------------------------------------------------

    /// Details finished loading: turn ingest mode on for titles that can be queued.
    pub(super) fn ingest_details_loaded(self: &Rc<Self>) {
        let library_only = self
            .details
            .borrow()
            .route
            .is_some_and(|(_, _, library_only)| library_only);
        let mode = self.job_queue().is_some() && !library_only;
        {
            let mut st = self.ingest.state.borrow_mut();
            st.mode = mode;
            st.selection.clear();
            st.note.clear();
        }
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<DetailsState>().set_ingest_mode(mode);
        }
        let len = if mode { ivm::BAR_LEN } else { 0 };
        self.graphs.borrow_mut().details.set_len(dvm::INGEST, len);
        self.render_bar();
    }

    /// A new season is loading: the selection belongs to the old one.
    pub(super) fn ingest_season_changed(&self) {
        self.ingest.state.borrow_mut().selection.clear();
        self.render_bar();
    }

    /// The season's episodes are listed: the marks and the bar follow.
    pub(super) fn ingest_episodes_loaded(&self) {
        self.render_selection();
    }

    /// Details closed: drop the dialog and the mode.
    pub(super) fn ingest_details_closed(&self) {
        let had_dialog = {
            let mut st = self.ingest.state.borrow_mut();
            st.mode = false;
            st.selection.clear();
            st.note.clear();
            st.dialog_generation = st.dialog_generation.wrapping_add(1);
            st.dialog.take().is_some()
        };
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<DetailsState>().set_ingest_mode(false);
            if had_dialog {
                ui.global::<PasteState>().set_open(false);
                ui.global::<FilesState>().set_open(false);
                ui.global::<HubState>().set_open(false);
            }
        }
    }

    fn target(&self) -> Option<Target> {
        let m = self.details.borrow();
        let d = m.details.as_ref()?;
        let media = d.summary.media;
        let season = match media {
            MediaType::Movie => 0,
            MediaType::Tv => m.selected?,
        };
        Some(Target {
            id: d.summary.id,
            media,
            title: d.summary.title.clone(),
            season,
            episodes: m.episodes.iter().map(|e| e.episode).collect(),
            selected: self.ingest.state.borrow().selection.sorted(),
        })
    }

    fn toggle_episode(self: &Rc<Self>, index: usize) {
        if !self.ingest.state.borrow().mode {
            return;
        }
        let Some(episode) = self.details.borrow().episodes.get(index).map(|e| e.episode) else {
            return;
        };
        self.ingest.state.borrow_mut().selection.toggle(episode);
        self.render_selection();
    }

    /// Writes the selection marks and the bar.
    fn render_selection(&self) {
        let marks: Vec<bool> = {
            let st = self.ingest.state.borrow();
            self.details
                .borrow()
                .episodes
                .iter()
                .map(|e| st.selection.contains(e.episode))
                .collect()
        };
        for (i, selected) in marks.into_iter().enumerate() {
            if let Some(mut row) = self.episodes.row_data(i) {
                if row.selected != selected {
                    row.selected = selected;
                    self.episodes.set_row_data(i, row);
                }
            }
        }
        self.render_bar();
    }

    fn render_bar(&self) {
        let (media, episodes) = {
            let m = self.details.borrow();
            (
                m.details.as_ref().map(|d| d.summary.media),
                m.episodes.len(),
            )
        };
        let Some(media) = media else {
            return;
        };
        let (selected, note) = {
            let st = self.ingest.state.borrow();
            (st.selection.len(), st.note.clone())
        };
        let bar = ivm::bar(media, selected, episodes);
        let mut parts = Vec::new();
        if !bar.selected.is_empty() {
            parts.push(bar.selected.clone());
            parts.push("SPACE OR X SELECTS · A ALL · N NONE".to_owned());
        }
        if !note.is_empty() {
            parts.push(note);
        }
        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<IngestBar>();
            state.set_vidlink_enabled(bar.vidlink_enabled);
            state.set_hub_label(bar.hub_label.into());
            state.set_hub_enabled(bar.hub_enabled);
            state.set_line(parts.join(" · ").into());
        }
    }

    /// CENTER on the action bar.
    pub(super) fn ingest_bar(self: &Rc<Self>, index: usize) {
        let Some(target) = self.target() else {
            return;
        };
        let bar = ivm::bar(target.media, target.selected.len(), target.episodes.len());
        match index {
            ivm::VIDLINK if bar.vidlink_enabled => {
                let jobs = ivm::page_jobs(&target);
                self.queue_jobs(jobs);
                self.ingest.state.borrow_mut().selection.clear();
                self.render_selection();
            }
            ivm::PASTE_LINKS => self.open_paste(target),
            ivm::LOCAL_FILES => self.pick_files(target),
            ivm::HUB if bar.hub_enabled => self.open_hub(target),
            _ => {}
        }
    }

    /// Adds jobs and notes how many under the bar.
    fn queue_jobs(self: &Rc<Self>, jobs: Vec<Job>) {
        if jobs.is_empty() {
            return;
        }
        let Some(queue) = self.job_queue() else {
            return;
        };
        let n = jobs.len();
        queue.add(jobs);
        self.ingest.state.borrow_mut().note = ivm::queued_note(n);
        self.render_bar();
        self.pull_queue();
    }

    // -- dialogs ------------------------------------------------------------

    /// Replaces the dialog layer's zones and focuses `focus` (else the first item).
    fn set_modal(&self, zones: Vec<crate::focus::Zone>, focus: Option<Focus>) {
        {
            let mut graphs = self.graphs.borrow_mut();
            let g = &mut graphs.details;
            // Popping restores the screen's focus, which the push saves again.
            g.pop_modal();
            g.push_modal(zones);
            let placed = focus.is_some_and(|f| g.set_focus(f.zone, f.index));
            if !placed && g.focus().is_none() {
                g.focus_first();
            }
        }
        self.sync_focus();
    }

    fn next_dialog(&self) -> u64 {
        let mut st = self.ingest.state.borrow_mut();
        st.dialog_generation = st.dialog_generation.wrapping_add(1);
        st.dialog_generation
    }

    fn close_dialog(self: &Rc<Self>) {
        let dialog = self.ingest.state.borrow_mut().dialog.take();
        let Some(dialog) = dialog else {
            return;
        };
        self.next_dialog();
        let clears = !matches!(dialog, Dialog::Paste(..));
        if let Some(ui) = self.ui.upgrade() {
            match dialog {
                Dialog::Paste(..) => ui.global::<PasteState>().set_open(false),
                Dialog::Files(..) => ui.global::<FilesState>().set_open(false),
                Dialog::Hub { .. } => ui.global::<HubState>().set_open(false),
            }
        }
        self.graphs.borrow_mut().details.pop_modal();
        if clears {
            // As on the Mac, closing the Files or Hub dialog clears the selection.
            self.ingest.state.borrow_mut().selection.clear();
            self.render_selection();
        }
        self.sync_focus();
    }

    /// CENTER or a click inside a dialog.
    pub(super) fn dialog_activate(self: &Rc<Self>, focus: Focus) {
        enum Kind {
            Paste(MediaType),
            Files,
            Hub,
        }
        let kind = match &self.ingest.state.borrow().dialog {
            Some(Dialog::Paste(t, _)) => Kind::Paste(t.media),
            Some(Dialog::Files(..)) => Kind::Files,
            Some(Dialog::Hub { .. }) => Kind::Hub,
            None => return,
        };
        match kind {
            Kind::Paste(media) => self.paste_activate(media, focus),
            Kind::Files => self.files_activate(focus),
            Kind::Hub => self.hub_activate(focus),
        }
    }

    /// QUEUE N in the open dialog.
    fn queue_dialog(self: &Rc<Self>) {
        let jobs = {
            let st = self.ingest.state.borrow();
            let library = self.library.borrow().clone();
            match &st.dialog {
                Some(Dialog::Paste(t, p)) => p.jobs(t),
                Some(Dialog::Files(t, f)) => f.jobs(t),
                Some(Dialog::Hub {
                    target,
                    phase: HubPhase::Loaded(p),
                    ..
                }) => p.jobs(target, library.as_ref()),
                _ => return,
            }
        };
        if jobs.is_empty() {
            return;
        }
        self.queue_jobs(jobs);
        self.close_dialog();
    }

    // Paste links

    fn open_paste(self: &Rc<Self>, target: Target) {
        self.next_dialog();
        let vm = PasteLinks::new(target.start_episode());
        let media = target.media;
        let subtitle = match media {
            MediaType::Tv => format!("{}. One file URL per episode.", target.subtitle()),
            MediaType::Movie => format!("{}. One file URL.", target.subtitle()),
        };
        self.ingest
            .paste_rows
            .set_vec(vm.rows.iter().map(link_row).collect::<Vec<_>>());
        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<PasteState>();
            state.set_is_tv(media == MediaType::Tv);
            state.set_subtitle(subtitle.into());
            state.set_can_remove(vm.can_remove());
            state.set_queue_label(ivm::queue_label(0).into());
            state.set_open(true);
        }
        let zones = vm.zones(media);
        self.ingest.state.borrow_mut().dialog = Some(Dialog::Paste(target, vm));
        let url = Focus::new(ivm::cell_zone(ivm::PASTE_ROWS, 0, ivm::COL_MAIN), 0);
        self.set_modal(zones, Some(url));
    }

    fn paste_edited(&self, row: i32, text: &str, episode: bool) {
        let Ok(row) = usize::try_from(row) else {
            return;
        };
        let count = {
            let mut st = self.ingest.state.borrow_mut();
            let Some(Dialog::Paste(t, p)) = &mut st.dialog else {
                return;
            };
            if episode {
                p.set_episode(row, text);
            } else {
                p.set_url(row, text);
            }
            p.jobs(t).len()
        };
        // Mirror the text into the model so a re-created field (the dialog shown
        // again after Search or Settings) keeps it. The edited field's own binding
        // is already replaced by what was typed, so this does not move its caret.
        if let Some(mut data) = self.ingest.paste_rows.row_data(row) {
            if episode {
                data.episode = text.into();
            } else {
                data.url = text.into();
            }
            self.ingest.paste_rows.set_row_data(row, data);
        }
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<PasteState>()
                .set_queue_label(ivm::queue_label(count).into());
        }
    }

    fn render_paste_meta(&self) {
        let (can_remove, count) = {
            let st = self.ingest.state.borrow();
            let Some(Dialog::Paste(t, p)) = &st.dialog else {
                return;
            };
            (p.can_remove(), p.jobs(t).len())
        };
        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<PasteState>();
            state.set_can_remove(can_remove);
            state.set_queue_label(ivm::queue_label(count).into());
        }
    }

    fn paste_activate(self: &Rc<Self>, media: MediaType, focus: Focus) {
        if let Some((row, ivm::COL_REMOVE)) = ivm::zone_cell(ivm::PASTE_ROWS, focus.zone) {
            let zones = {
                let mut st = self.ingest.state.borrow_mut();
                let Some(Dialog::Paste(_, p)) = &mut st.dialog else {
                    return;
                };
                if !p.remove(row) {
                    return;
                }
                (p.zones(media), p.rows.len(), p.can_remove())
            };
            let (zones, len, can_remove) = zones;
            self.ingest.paste_rows.remove(row);
            let next = row.min(len.saturating_sub(1));
            let col = if can_remove {
                ivm::COL_REMOVE
            } else {
                ivm::COL_MAIN
            };
            self.render_paste_meta();
            self.set_modal(
                zones,
                Some(Focus::new(ivm::cell_zone(ivm::PASTE_ROWS, next, col), 0)),
            );
            return;
        }
        if focus.zone != ivm::PASTE_FOOTER {
            return;
        }
        match ivm::paste_footer(media).get(focus.index) {
            Some(Footer::AddField) => {
                let added = {
                    let mut st = self.ingest.state.borrow_mut();
                    let Some(Dialog::Paste(_, p)) = &mut st.dialog else {
                        return;
                    };
                    p.add_field().map(|r| (r, p.zones(media), p.rows.len()))
                };
                let Some((row, zones, len)) = added else {
                    return;
                };
                self.ingest.paste_rows.push(link_row(&row));
                self.render_paste_meta();
                let url = ivm::cell_zone(ivm::PASTE_ROWS, len - 1, ivm::COL_MAIN);
                self.set_modal(zones, Some(Focus::new(url, 0)));
            }
            Some(Footer::Cancel) => self.close_dialog(),
            Some(Footer::Queue) => self.queue_dialog(),
            _ => {}
        }
    }

    // Local files

    /// Opens the system picker; the dialog opens with the chosen files.
    fn pick_files(self: &Rc<Self>, target: Target) {
        let generation = self.ingest.state.borrow().dialog_generation;
        let multi = target.media == MediaType::Tv;
        let picker = self.picker();
        let shell = self.clone();
        self.exec
            .run(async move { picker.pick(multi).await }, move |paths| {
                if paths.is_empty() || shell.ingest.state.borrow().dialog_generation != generation {
                    return;
                }
                shell.open_files(target, paths);
            });
    }

    fn open_files(self: &Rc<Self>, target: Target, paths: Vec<PathBuf>) {
        if self.screen() != Screen::Details {
            return;
        }
        self.next_dialog();
        let mut vm = LocalFiles::new(target.media, target.start_episode());
        vm.append(paths);
        let subtitle = match target.media {
            MediaType::Tv => format!("{}. One file per episode.", target.subtitle()),
            MediaType::Movie => format!("{}. One file.", target.subtitle()),
        };
        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<FilesState>();
            state.set_is_tv(target.media == MediaType::Tv);
            state.set_subtitle(subtitle.into());
            state.set_open(true);
        }
        let zones = vm.zones();
        self.ingest.state.borrow_mut().dialog = Some(Dialog::Files(target, vm));
        self.render_files();
        self.set_modal(zones, Some(Focus::new(ivm::FILES_FOOTER, 2)));
    }

    fn render_files(&self) {
        let (rows, count) = {
            let st = self.ingest.state.borrow();
            let Some(Dialog::Files(t, f)) = &st.dialog else {
                return;
            };
            (
                f.rows.iter().map(file_row).collect::<Vec<_>>(),
                f.jobs(t).len(),
            )
        };
        self.ingest.file_rows.set_vec(rows);
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<FilesState>()
                .set_queue_label(ivm::queue_label(count).into());
        }
    }

    fn files_edited(&self, row: i32, text: &str) {
        let Ok(row) = usize::try_from(row) else {
            return;
        };
        let count = {
            let mut st = self.ingest.state.borrow_mut();
            let Some(Dialog::Files(t, f)) = &mut st.dialog else {
                return;
            };
            f.set_episode(row, text);
            f.jobs(t).len()
        };
        // As in `paste_edited`.
        if let Some(mut data) = self.ingest.file_rows.row_data(row) {
            data.episode = text.into();
            self.ingest.file_rows.set_row_data(row, data);
        }
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<FilesState>()
                .set_queue_label(ivm::queue_label(count).into());
        }
    }

    fn files_activate(self: &Rc<Self>, focus: Focus) {
        if let Some((row, col)) = ivm::zone_cell(ivm::FILES_ROWS, focus.zone) {
            match col {
                ivm::COL_MAIN => {
                    let row_data = {
                        let mut st = self.ingest.state.borrow_mut();
                        let Some(Dialog::Files(_, f)) = &mut st.dialog else {
                            return;
                        };
                        f.cycle_tag(row);
                        f.rows.get(row).map(file_row)
                    };
                    if let Some(data) = row_data {
                        self.ingest.file_rows.set_row_data(row, data);
                    }
                }
                ivm::COL_REMOVE => {
                    let zones = {
                        let mut st = self.ingest.state.borrow_mut();
                        let Some(Dialog::Files(_, f)) = &mut st.dialog else {
                            return;
                        };
                        if !f.remove(row) {
                            return;
                        }
                        (f.zones(), f.rows.len())
                    };
                    let (zones, len) = zones;
                    self.ingest.file_rows.remove(row);
                    self.refresh_files_label();
                    let focus = if len == 0 {
                        Focus::new(ivm::FILES_FOOTER, 0)
                    } else {
                        Focus::new(
                            ivm::cell_zone(ivm::FILES_ROWS, row.min(len - 1), ivm::COL_REMOVE),
                            0,
                        )
                    };
                    self.set_modal(zones, Some(focus));
                }
                _ => {}
            }
            return;
        }
        if focus.zone != ivm::FILES_FOOTER {
            return;
        }
        match ivm::files_footer().get(focus.index) {
            Some(Footer::ChooseFiles) => self.choose_more_files(),
            Some(Footer::Cancel) => self.close_dialog(),
            Some(Footer::Queue) => self.queue_dialog(),
            _ => {}
        }
    }

    fn refresh_files_label(&self) {
        let count = {
            let st = self.ingest.state.borrow();
            let Some(Dialog::Files(t, f)) = &st.dialog else {
                return;
            };
            f.jobs(t).len()
        };
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<FilesState>()
                .set_queue_label(ivm::queue_label(count).into());
        }
    }

    /// CHOOSE FILES: adds to the open dialog.
    fn choose_more_files(self: &Rc<Self>) {
        let (generation, multi) = {
            let st = self.ingest.state.borrow();
            let Some(Dialog::Files(t, _)) = &st.dialog else {
                return;
            };
            (st.dialog_generation, t.media == MediaType::Tv)
        };
        let picker = self.picker();
        let shell = self.clone();
        self.exec
            .run(async move { picker.pick(multi).await }, move |paths| {
                if paths.is_empty() {
                    return;
                }
                let zones = {
                    let mut st = shell.ingest.state.borrow_mut();
                    if st.dialog_generation != generation {
                        return;
                    }
                    let Some(Dialog::Files(_, f)) = &mut st.dialog else {
                        return;
                    };
                    f.append(paths);
                    f.zones()
                };
                shell.render_files();
                shell.set_modal(zones, Some(Focus::new(ivm::FILES_FOOTER, 2)));
            });
    }

    // 4KHDHub

    fn open_hub(self: &Rc<Self>, target: Target) {
        let Some(details) = self.details.borrow().details.clone() else {
            return;
        };
        let generation = self.next_dialog();
        let subtitle = match target.media {
            MediaType::Tv => format!("{} · {} episodes", target.subtitle(), target.wanted().len()),
            MediaType::Movie => target.subtitle(),
        };
        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<HubState>();
            state.set_subtitle(subtitle.into());
            state.set_url("".into());
            state.set_open(true);
        }
        self.ingest.state.borrow_mut().dialog = Some(Dialog::Hub {
            target,
            phase: HubPhase::Busy(ivm::HUB_SEARCHING.to_owned()),
            url: String::new(),
        });
        self.render_hub(None);

        let source = self.hub_source();
        let shell = self.clone();
        self.exec
            .run(async move { source.find(&details).await }, move |result| {
                if shell.ingest.state.borrow().dialog_generation != generation {
                    return;
                }
                match result {
                    Ok(Some(page)) => shell.hub_load(page),
                    Ok(None) => shell.hub_phase(HubPhase::Url(ivm::HUB_NOT_FOUND.to_owned())),
                    Err(e) => {
                        tracing::warn!("4KHDHub search: {e}");
                        shell.hub_phase(HubPhase::Url(ivm::HUB_SEARCH_FAILED.to_owned()));
                    }
                }
            });
    }

    fn hub_phase(self: &Rc<Self>, next: HubPhase) {
        if let Some(Dialog::Hub { phase, .. }) = &mut self.ingest.state.borrow_mut().dialog {
            *phase = next;
        }
        self.render_hub(None);
    }

    /// LOAD or Enter in the URL field.
    fn hub_load_typed(self: &Rc<Self>) {
        let typed = match &self.ingest.state.borrow().dialog {
            Some(Dialog::Hub { url, .. }) => ivm::http_url(url),
            _ => return,
        };
        if let Some(page) = typed {
            self.hub_load(page);
        }
    }

    fn hub_load(self: &Rc<Self>, page: String) {
        let (generation, media) = {
            let st = self.ingest.state.borrow();
            let Some(Dialog::Hub { target, .. }) = &st.dialog else {
                return;
            };
            (st.dialog_generation, target.media)
        };
        self.hub_phase(HubPhase::Busy(ivm::hub_reading(&page)));
        let source = self.hub_source();
        let shell = self.clone();
        let url = page.clone();
        self.exec.run(
            async move { source.variants(&url, media).await },
            move |result| {
                let next = {
                    let st = shell.ingest.state.borrow();
                    if st.dialog_generation != generation {
                        return;
                    }
                    let Some(Dialog::Hub { target, .. }) = &st.dialog else {
                        return;
                    };
                    match result {
                        Ok(all) => {
                            let picker = HubPicker::new(target, page, all);
                            if picker.variants.is_empty() {
                                HubPhase::Url(ivm::hub_empty(target.media, target.season))
                            } else {
                                HubPhase::Loaded(picker)
                            }
                        }
                        Err(e) => {
                            tracing::warn!("4KHDHub page: {e}");
                            HubPhase::Url(ivm::HUB_READ_FAILED.to_owned())
                        }
                    }
                };
                shell.hub_phase(next);
            },
        );
    }

    /// Writes the hub dialog and its zones. `focus` overrides where focus lands.
    fn render_hub(self: &Rc<Self>, focus: Option<Focus>) {
        let library = self.library.borrow().clone();
        let (phase, status, rows, chosen, skip, host, note, count) = {
            let st = self.ingest.state.borrow();
            let Some(Dialog::Hub { target, phase, .. }) = &st.dialog else {
                return;
            };
            match phase {
                HubPhase::Busy(s) => (
                    0,
                    s.clone(),
                    Vec::new(),
                    -1,
                    true,
                    String::new(),
                    String::new(),
                    0,
                ),
                HubPhase::Url(s) => (
                    1,
                    s.clone(),
                    Vec::new(),
                    -1,
                    true,
                    String::new(),
                    String::new(),
                    0,
                ),
                HubPhase::Loaded(p) => {
                    let rows: Vec<HubRow> = p
                        .variants
                        .iter()
                        .map(|v| {
                            let r = p.row(target, library.as_ref(), v);
                            HubRow {
                                label: r.label.into(),
                                name: r.name.into(),
                                badge: r.badge.into(),
                                count: r.count.into(),
                                complete: r.complete,
                                size: r.size.into(),
                            }
                        })
                        .collect();
                    (
                        2,
                        String::new(),
                        rows,
                        p.chosen.map_or(-1, index_i32),
                        p.skip_uploaded,
                        ivm::host(&p.page),
                        p.status(target, library.as_ref()),
                        p.queued(target, library.as_ref()).len(),
                    )
                }
            }
        };
        let variants = rows.len();
        self.ingest.hub_rows.set_vec(rows);
        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<HubState>();
            state.set_phase(phase);
            state.set_status(status.into());
            state.set_chosen(chosen);
            state.set_skip_uploaded(skip);
            state.set_host(host.into());
            state.set_note(note.into());
            state.set_queue_label(ivm::queue_label(count).into());
        }
        let default_focus = match phase {
            1 => Focus::new(ivm::HUB_URL, 0),
            2 => Focus::new(ivm::HUB_VARIANTS, usize::try_from(chosen).unwrap_or(0)),
            _ => Focus::new(ivm::HUB_FOOTER, 0),
        };
        self.set_modal(
            ivm::hub_zones(phase == 1, variants),
            Some(focus.unwrap_or(default_focus)),
        );
    }

    fn hub_activate(self: &Rc<Self>, focus: Focus) {
        match focus.zone {
            ivm::HUB_LOAD => self.hub_load_typed(),
            ivm::HUB_VARIANTS | ivm::HUB_SKIP => {
                {
                    let mut st = self.ingest.state.borrow_mut();
                    let Some(Dialog::Hub {
                        phase: HubPhase::Loaded(p),
                        ..
                    }) = &mut st.dialog
                    else {
                        return;
                    };
                    if focus.zone == ivm::HUB_SKIP {
                        p.skip_uploaded = !p.skip_uploaded;
                    } else if focus.index < p.variants.len() {
                        p.chosen = Some(focus.index);
                    }
                }
                self.render_hub(Some(focus));
            }
            ivm::HUB_FOOTER => match ivm::hub_footer().get(focus.index) {
                Some(Footer::Cancel) => self.close_dialog(),
                Some(Footer::Queue) => self.queue_dialog(),
                _ => {}
            },
            _ => {}
        }
    }

    // -- queue ----------------------------------------------------------------

    /// Reads the queue's current list (the watch covers live changes; this also
    /// covers inline execution and actions just taken).
    fn pull_queue(self: &Rc<Self>) {
        if let Some(queue) = self.job_queue() {
            let views = queue.snapshot().borrow().clone();
            self.on_queue(views);
        }
    }

    fn on_queue(self: &Rc<Self>, views: Vec<JobView>) {
        let rows = qvm::rows(&views);
        let (drained, before) = {
            let mut st = self.ingest.state.borrow_mut();
            if st.queue_seen && st.views == views {
                return;
            }
            st.queue_seen = true;
            let drained = qvm::drained(&st.views, &views);
            let before: Vec<Uuid> = st.rows.iter().map(|r| r.id).collect();
            st.views = views.clone();
            st.rows = rows.clone();
            (drained, before)
        };
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<HomeState>()
                .set_queue_count(index_i32(qvm::pending(&views)));
            ui.global::<QueueState>()
                .set_footer(qvm::footer(&views).into());
        }
        let model = &self.ingest.queue_rows;
        if model.row_count() == rows.len() {
            for (i, r) in rows.iter().enumerate() {
                model.set_row_data(i, queue_row(r));
            }
        } else {
            model.set_vec(rows.iter().map(queue_row).collect::<Vec<_>>());
        }
        {
            let mut graphs = self.graphs.borrow_mut();
            let focus = graphs.queue.focus();
            let next = qvm::refocus(&before, focus, &rows);
            let mut graph = FocusGraph::with_zones(qvm::zones(&rows));
            if let Some(f) = next {
                graph.set_focus(f.zone, f.index);
            }
            graphs.queue = graph;
        }
        if self.screen() == Screen::Queue {
            self.sync_focus();
        }
        if drained {
            self.invalidate_library();
            self.render_library();
            self.maybe_load_library();
            if self.screen() == Screen::Library {
                self.refresh_manager();
            }
        }
    }

    pub(super) fn open_queue(self: &Rc<Self>) {
        self.pull_queue();
        let mut graphs = self.graphs.borrow_mut();
        if graphs.queue.focus().is_none() {
            graphs.queue.focus_first();
        }
    }

    pub(super) fn queue_activate(self: &Rc<Self>, focus: Focus) {
        let Some(queue) = self.job_queue() else {
            return;
        };
        if focus.zone == qvm::FOOTER {
            queue.clear_finished();
        } else if let Some(i) = qvm::zone_row(focus.zone) {
            let target = {
                let st = self.ingest.state.borrow();
                st.rows
                    .get(i)
                    .and_then(|r| r.actions.get(focus.index).map(|a| (r.id, *a)))
            };
            match target {
                Some((id, qvm::Action::Retry)) => queue.retry(id),
                Some((id, qvm::Action::Remove)) => queue.remove(id),
                Some((id, qvm::Action::Cancel)) => queue.cancel(id),
                None => return,
            }
        } else {
            return;
        }
        self.pull_queue();
    }

    // -- library manager ------------------------------------------------------

    pub(super) fn open_library(self: &Rc<Self>, fresh: bool) {
        if !fresh {
            return;
        }
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<LibraryState>().set_scroll_y(0.0);
        }
        {
            let mut graphs = self.graphs.borrow_mut();
            let len = self.ingest.state.borrow().manager.flat.len();
            graphs.library = FocusGraph::with_zones(lvm::zones(len));
            graphs.library.focus_first();
        }
        self.refresh_manager();
    }

    fn refresh_manager(self: &Rc<Self>) {
        let ready = self.home.borrow().status.ready();
        let admin = self.admin().filter(|_| ready);
        let Some(admin) = admin else {
            self.ingest.state.borrow_mut().manager.load = lvm::Load::NotConnected;
            self.render_manager();
            return;
        };
        let generation = {
            let mut st = self.ingest.state.borrow_mut();
            st.manager.generation = st.manager.generation.wrapping_add(1);
            st.manager.load = lvm::Load::Reading;
            st.manager.generation
        };
        self.render_manager();
        let channel = self.services.settings.get().telegram_channel;
        let catalog = self.services.catalog.clone();
        let shell = self.clone();
        self.exec.run(
            async move {
                let entries = admin.entries(&channel).await?;
                let titles: HashSet<(TmdbId, MediaType)> =
                    entries.iter().map(|e| (e.key.tmdb, e.key.media)).collect();
                let lookups = titles.into_iter().map(|(id, media)| {
                    let catalog = catalog.clone();
                    async move { ((id, media), catalog.details(media, id).await) }
                });
                let mut names = HashMap::new();
                for (key, resolved) in futures::future::join_all(lookups).await {
                    match resolved {
                        Ok(d) => {
                            names.insert(key, d.summary.title);
                        }
                        Err(e) => tracing::debug!("library title {key:?}: {e}"),
                    }
                }
                Ok::<_, Error>((entries, names))
            },
            move |result| {
                {
                    let mut st = shell.ingest.state.borrow_mut();
                    if st.manager.generation != generation {
                        return;
                    }
                    match result {
                        Ok((entries, names)) => {
                            st.manager.entries = entries;
                            st.manager.names = names;
                            st.manager.load = lvm::Load::Loaded;
                        }
                        Err(e) => {
                            tracing::warn!("library manager: {e}");
                            st.manager.load = lvm::Load::Failed(e.to_string());
                        }
                    }
                }
                shell.render_manager();
            },
        );
    }

    fn render_manager(&self) {
        let (groups, stamp, status, len) = {
            let mut st = self.ingest.state.borrow_mut();
            let m = &mut st.manager;
            let (groups, flat) = lvm::groups(&m.entries, &m.names);
            m.flat = flat;
            let len = m.flat.len();
            (
                groups,
                lvm::stamp(&m.load, len),
                lvm::status(&m.load, len),
                len,
            )
        };
        let rows: Vec<LibraryGroup> = groups
            .into_iter()
            .map(|g| {
                let entries: Vec<LibraryEntry> = g
                    .rows
                    .into_iter()
                    .map(|r| LibraryEntry {
                        index: index_i32(r.index),
                        key: r.key.into(),
                        quality: r.quality.into(),
                        size: r.size.into(),
                    })
                    .collect();
                LibraryGroup {
                    title: g.title.to_uppercase().into(),
                    entries: ModelRc::new(VecModel::from(entries)),
                }
            })
            .collect();
        self.ingest.library_groups.set_vec(rows);
        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<LibraryState>();
            state.set_stamp(stamp.into());
            state.set_status(status.into());
        }
        self.set_len(Screen::Library, lvm::ENTRIES, len);
    }

    pub(super) fn library_activate(self: &Rc<Self>, focus: Focus) {
        match (focus.zone, focus.index) {
            (lvm::REFRESH, _) => self.refresh_manager(),
            (lvm::ENTRIES, i) => self.open_confirm(i),
            (lvm::CONFIRM, lvm::CONFIRM_DELETE) => self.delete_confirmed(),
            (lvm::CONFIRM, _) => self.close_confirm(),
            _ => {}
        }
    }

    fn open_confirm(&self, index: usize) {
        let text = {
            let mut st = self.ingest.state.borrow_mut();
            let m = &mut st.manager;
            let Some(e) = m.flat.get(index) else {
                return;
            };
            let title = lvm::title_name(&m.names, e.key.tmdb, e.key.media);
            let text = lvm::confirm_text(&title, e);
            m.confirm = Some(index);
            text
        };
        if let Some(ui) = self.ui.upgrade() {
            let state = ui.global::<LibraryState>();
            state.set_confirm_text(text.into());
            state.set_confirm_open(true);
        }
        {
            let mut graphs = self.graphs.borrow_mut();
            graphs.library.push_modal(lvm::confirm_zones());
            graphs.library.set_focus(lvm::CONFIRM, lvm::CONFIRM_CANCEL);
        }
        self.sync_focus();
    }

    fn close_confirm(&self) {
        let was_open = self
            .ingest
            .state
            .borrow_mut()
            .manager
            .confirm
            .take()
            .is_some();
        if !was_open {
            return;
        }
        if let Some(ui) = self.ui.upgrade() {
            ui.global::<LibraryState>().set_confirm_open(false);
        }
        self.graphs.borrow_mut().library.pop_modal();
        self.sync_focus();
    }

    fn delete_confirmed(self: &Rc<Self>) {
        let entry = {
            let st = self.ingest.state.borrow();
            st.manager
                .confirm
                .and_then(|i| st.manager.flat.get(i).cloned())
        };
        self.close_confirm();
        let (Some(entry), Some(admin)) = (entry, self.admin()) else {
            return;
        };
        self.ingest.state.borrow_mut().manager.load = lvm::Load::Reading;
        self.render_manager();
        let shell = self.clone();
        self.exec
            .run(async move { admin.delete(&entry).await }, move |result| {
                if let Err(e) = result {
                    tracing::warn!("delete: {e}");
                    shell.ingest.state.borrow_mut().manager.load =
                        lvm::Load::Failed(format!("delete failed: {e}"));
                    shell.render_manager();
                    return;
                }
                shell.refresh_manager();
                shell.invalidate_library();
                shell.render_library();
                shell.maybe_load_library();
            });
    }
}
