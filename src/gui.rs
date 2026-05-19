//! Native Win32 GUI for `dcmfree`.
//!
//! Renders a real Common Controls v6 `ListView` of HCS layers so the user
//! can review and selectively destroy them in bulk. Selection uses native
//! multi-select (click / Ctrl+click / Shift+click — same as File Explorer),
//! plus helper buttons for `Select orphans` / `Clear`.
//!
//! Destructive work runs on a worker thread; the GUI keeps its message loop
//! alive so the Cancel button stays responsive. Progress + per-layer results
//! flow back via `nwg::Notice` + an `mpsc::channel`. When the worker
//! finishes (cancelled or not) the results are shown in a scrollable modal
//! dialog with full per-layer outcome and a summary footer.

use crate::format::{age as fmt_age, bytes as fmt_bytes};
use crate::layers::{self, LayerInfo};
use crate::{DEFAULT_LAYERS_DIR, hcs, privileges};
use chrono::{DateTime, Local};
use native_windows_derive::NwgUi;
use native_windows_gui as nwg;
use nwg::NativeUi;
use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::SystemTime;

#[derive(Default)]
struct AppState {
    layers_dir: PathBuf,
    layers: Vec<LayerInfo>,
    // Active job, if any (scan or destroy share the same notice + channel).
    receiver: Option<Receiver<WorkerMsg>>,
    cancel: Option<Arc<AtomicBool>>,
    job: Job,
    // Cumulative results for an in-flight destroy run.
    results: Vec<OutcomeRow>,
    total_targets: usize,
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
enum Job {
    #[default]
    Idle,
    Scanning,
    Destroying,
}

#[derive(Debug, Clone)]
struct OutcomeRow {
    id: String,
    size_bytes: u64,
    outcome: OutcomeKind,
}

#[derive(Debug, Clone)]
enum OutcomeKind {
    Destroyed,
    Failed(String),
    Cancelled,
}

#[derive(Debug)]
enum WorkerMsg {
    // Scan messages
    /// One layer fully stat-ed; ready to be displayed.
    ScanLayer(Box<LayerInfo>),
    /// Whole layers dir failed to enumerate (e.g. not found).
    ScanError(String),
    /// Scan complete.
    ScanFinished,

    // Destroy messages
    /// Starting work on layer at `layers_index` (id supplied for the status label).
    DestroyStarting { index: usize, id: String },
    /// Layer finished — bubble up outcome and the row index that was acted on.
    DestroyDone {
        layers_index: usize,
        row: OutcomeRow,
    },
    /// Worker finished the whole batch (or was cancelled before starting some).
    DestroyFinished,
}

#[derive(Default, NwgUi)]
pub struct App {
    state: RefCell<AppState>,

    #[nwg_control(
        size: (1040, 660),
        title: "dcmfree - container layer cleanup",
        flags: "MAIN_WINDOW|VISIBLE"
    )]
    #[nwg_events(OnWindowClose: [App::on_close], OnInit: [App::on_init])]
    window: nwg::Window,

    #[nwg_control(parent: window)]
    status: nwg::StatusBar,

    // -- top action row ------------------------------------------------------
    #[nwg_control(parent: window, text: "Refresh", position: (10, 10), size: (110, 30))]
    #[nwg_events(OnButtonClick: [App::on_refresh])]
    btn_refresh: nwg::Button,

    #[nwg_control(parent: window, text: "Select orphans", position: (130, 10), size: (130, 30))]
    #[nwg_events(OnButtonClick: [App::on_select_orphans])]
    btn_select_orphans: nwg::Button,

    #[nwg_control(parent: window, text: "Clear selection", position: (270, 10), size: (130, 30))]
    #[nwg_events(OnButtonClick: [App::on_clear])]
    btn_clear: nwg::Button,

    // -- info strip ----------------------------------------------------------
    #[nwg_control(
        parent: window,
        text: "Layers: 0    Selected: 0 (0 B)",
        position: (10, 50),
        size: (1010, 22)
    )]
    info_label: nwg::Label,

    // -- the list ------------------------------------------------------------
    // FULL_ROW_SELECT + GRID + HEADER_DRAG_DROP are what nwg exposes; native
    // multi-select with Ctrl/Shift is the Win32 default (no LVS_SINGLESEL).
    #[nwg_control(
        parent: window,
        position: (10, 80),
        size: (1010, 490),
        list_style: nwg::ListViewStyle::Detailed,
        focus: true,
        ex_flags: nwg::ListViewExFlags::FULL_ROW_SELECT
                | nwg::ListViewExFlags::GRID
                | nwg::ListViewExFlags::HEADER_DRAG_DROP
    )]
    #[nwg_events(OnListViewItemChanged: [App::on_list_selection_changed])]
    list: nwg::ListView,

    // -- bottom row ----------------------------------------------------------
    #[nwg_control(parent: window, text: "Quit", position: (10, 580), size: (110, 36))]
    #[nwg_events(OnButtonClick: [App::on_close])]
    btn_quit: nwg::Button,

    #[nwg_control(
        parent: window,
        text: "Destroy selected",
        position: (810, 580),
        size: (210, 36),
        enabled: false
    )]
    #[nwg_events(OnButtonClick: [App::on_destroy])]
    btn_destroy: nwg::Button,

    // Worker -> GUI bridge.
    #[nwg_control]
    #[nwg_events(OnNotice: [App::on_worker_notice])]
    worker_notice: nwg::Notice,
}

impl App {
    fn on_init(&self) {
        self.state.borrow_mut().layers_dir = PathBuf::from(DEFAULT_LAYERS_DIR);
        self.setup_columns();
        self.start_scan();
    }

    fn on_close(&self) {
        // If a worker is running, ask it to stop before we tear down the
        // event loop so we don't leave a half-completed destroy behind.
        if let Some(cancel) = self.state.borrow().cancel.as_ref() {
            cancel.store(true, Ordering::Relaxed);
        }
        nwg::stop_thread_dispatch();
    }

    fn on_refresh(&self) {
        self.start_scan();
    }

    fn on_select_orphans(&self) {
        // Collect just the orphan flags so we can drop the borrow before
        // touching ListView state (which would otherwise need a second
        // borrow if `select_item` ever read shared state).
        let orphan_flags: Vec<bool> = self
            .state
            .borrow()
            .layers
            .iter()
            .map(|l| l.orphan)
            .collect();
        for (i, &is_orphan) in orphan_flags.iter().enumerate() {
            self.list.select_item(i, is_orphan);
        }
        self.update_info();
    }

    fn on_clear(&self) {
        let n = self.state.borrow().layers.len();
        for i in 0..n {
            self.list.select_item(i, false);
        }
        self.update_info();
    }

    fn on_list_selection_changed(&self) {
        self.update_info();
    }

    fn on_destroy(&self) {
        let selected = self.list.selected_items();
        if selected.is_empty() {
            self.message_info("Nothing selected to destroy.");
            return;
        }
        if !privileges::is_elevated() {
            self.message_warning(
                "Administrator is required to destroy HCS layers.\n\n\
                 Re-run dcmfree from an elevated prompt or right-click \
                 dcmfree.exe and choose \"Run as administrator\".",
            );
            return;
        }
        let total_size: u64 = {
            let state = self.state.borrow();
            selected
                .iter()
                .filter_map(|&i| state.layers.get(i).map(|l| l.size_bytes))
                .sum()
        };
        let confirm = format!(
            "About to destroy {} layer(s), reclaiming approximately {}.\n\n\
             This is irreversible. You can cancel mid-process; layers already \
             destroyed cannot be recovered.\n\nContinue?",
            selected.len(),
            fmt_bytes(total_size)
        );
        if !self.confirm_ok_cancel("Confirm destroy", &confirm, nwg::MessageIcons::Warning) {
            return;
        }
        if let Err(e) = privileges::enable_backup_restore() {
            self.message_error(&format!("Could not enable required privileges: {e}"));
            return;
        }

        // Build the worker payload: paths + (layers-index, id, size) so we
        // can update the right row when each item completes. `filter_map`
        // silently drops any selection index whose layer was removed
        // between selection and click.
        let payload: Vec<(usize, String, u64, PathBuf)> = {
            let state = self.state.borrow();
            selected
                .iter()
                .filter_map(|&i| {
                    state
                        .layers
                        .get(i)
                        .map(|l| (i, l.id.clone(), l.size_bytes, l.path.clone()))
                })
                .collect()
        };

        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel::<WorkerMsg>();
        let sender = self.worker_notice.sender();
        let cancel_for_thread = Arc::clone(&cancel);

        {
            let mut st = self.state.borrow_mut();
            st.receiver = Some(rx);
            st.cancel = Some(cancel);
            st.job = Job::Destroying;
            st.results = Vec::new();
            st.total_targets = payload.len();
        }

        // While the worker runs, lock the main UI down.
        self.btn_destroy.set_enabled(false);
        self.btn_refresh.set_enabled(false);
        self.btn_select_orphans.set_enabled(false);
        self.btn_clear.set_enabled(false);
        // Hijack the Quit button to mean Cancel while a job is in flight.
        self.btn_quit.set_text("Cancel");
        self.set_status("Starting destroy...");

        thread::spawn(move || {
            for (layers_index, id, size_bytes, path) in payload {
                if cancel_for_thread.load(Ordering::Relaxed) {
                    let _ = tx.send(WorkerMsg::DestroyDone {
                        layers_index,
                        row: OutcomeRow {
                            id,
                            size_bytes,
                            outcome: OutcomeKind::Cancelled,
                        },
                    });
                    continue;
                }
                let _ = tx.send(WorkerMsg::DestroyStarting {
                    index: layers_index,
                    id: id.clone(),
                });
                sender.notice();
                let outcome = match hcs::destroy_layer(&path) {
                    Ok(()) => OutcomeKind::Destroyed,
                    Err(e) => OutcomeKind::Failed(format!("{e}")),
                };
                let _ = tx.send(WorkerMsg::DestroyDone {
                    layers_index,
                    row: OutcomeRow {
                        id,
                        size_bytes,
                        outcome,
                    },
                });
                sender.notice();
            }
            let _ = tx.send(WorkerMsg::DestroyFinished);
            sender.notice();
        });
    }

    fn on_worker_notice(&self) {
        // Drain everything currently in the channel under a single borrow
        // before doing anything else, to keep the borrow-checker happy.
        let messages: Vec<WorkerMsg> =
            self.state
                .borrow()
                .receiver
                .as_ref()
                .map_or_else(Vec::new, |rx| {
                    let mut msgs = Vec::new();
                    while let Ok(m) = rx.try_recv() {
                        msgs.push(m);
                    }
                    msgs
                });

        let mut destroy_finished = false;
        let mut scan_finished = false;
        let mut scan_error: Option<String> = None;
        let mut destroyed_rows: Vec<usize> = Vec::new();
        let now_batch = SystemTime::now();
        for msg in messages {
            match msg {
                // -------- scan ----------
                WorkerMsg::ScanLayer(info) => {
                    // `now` is captured once per drained batch — typical
                    // batches arrive within microseconds of each other, and
                    // a single timestamp is fine for display.
                    self.append_layer_row(&info, now_batch);
                    self.state.borrow_mut().layers.push(*info);
                }
                WorkerMsg::ScanError(e) => {
                    scan_error = Some(e);
                }
                WorkerMsg::ScanFinished => {
                    scan_finished = true;
                }

                // -------- destroy --------
                WorkerMsg::DestroyStarting { index, id } => {
                    let (progress, total) = {
                        let st = self.state.borrow();
                        (st.results.len() + 1, st.total_targets)
                    };
                    self.set_status(&format!(
                        "Destroying {progress}/{total}: {id}  (idx {index})"
                    ));
                }
                WorkerMsg::DestroyDone { layers_index, row } => {
                    if matches!(row.outcome, OutcomeKind::Destroyed) {
                        destroyed_rows.push(layers_index);
                    }
                    self.state.borrow_mut().results.push(row);
                }
                WorkerMsg::DestroyFinished => {
                    destroy_finished = true;
                }
            }
        }

        if !destroyed_rows.is_empty() {
            // Descending so earlier removals don't shift later indices.
            destroyed_rows.sort_unstable_by(|a, b| b.cmp(a));
            for idx in destroyed_rows {
                self.list.remove_item(idx);
                let mut st = self.state.borrow_mut();
                if idx < st.layers.len() {
                    st.layers.remove(idx);
                }
            }
            self.update_info();
        }

        if scan_finished {
            self.finish_scan(scan_error);
        }
        if destroy_finished {
            self.finish_destroy_run();
        }
    }

    fn finish_scan(&self, error: Option<String>) {
        {
            let mut st = self.state.borrow_mut();
            st.receiver = None;
            st.job = Job::Idle;
        }
        self.btn_refresh.set_enabled(true);
        if let Some(e) = error {
            self.set_status(&e);
            self.message_error(&e);
            return;
        }
        let count = self.state.borrow().layers.len();
        let dir = self.state.borrow().layers_dir.display().to_string();
        self.set_status(&format!("Loaded {count} layer(s) from {dir}"));
        self.update_info();
    }

    fn finish_destroy_run(&self) {
        // Snapshot results and clear the in-flight state.
        let (results, total_targets) = {
            let mut state = self.state.borrow_mut();
            state.receiver = None;
            state.cancel = None;
            state.job = Job::Idle;
            (std::mem::take(&mut state.results), state.total_targets)
        };

        // Re-enable controls and restore the Cancel/Quit button label.
        self.btn_refresh.set_enabled(true);
        self.btn_select_orphans.set_enabled(true);
        self.btn_clear.set_enabled(true);
        self.btn_quit.set_text("Quit");
        self.update_info();

        let destroyed = results
            .iter()
            .filter(|r| matches!(r.outcome, OutcomeKind::Destroyed))
            .count();
        let failed = results
            .iter()
            .filter(|r| matches!(r.outcome, OutcomeKind::Failed(_)))
            .count();
        let cancelled = results
            .iter()
            .filter(|r| matches!(r.outcome, OutcomeKind::Cancelled))
            .count();
        let bytes_freed: u64 = results
            .iter()
            .filter(|r| matches!(r.outcome, OutcomeKind::Destroyed))
            .map(|r| r.size_bytes)
            .sum();
        self.set_status(&format!(
            "Done: {destroyed}/{total_targets} destroyed, {failed} failed, {cancelled} cancelled. Freed approximately {}.",
            fmt_bytes(bytes_freed),
        ));

        // Show a scrollable report.
        ReportDialog::show(&results, destroyed, failed, cancelled, bytes_freed);
    }

    // ---- helpers ---------------------------------------------------------

    fn setup_columns(&self) {
        let cols: &[(&str, i32)] = &[
            ("Size", 90),
            ("Files", 80),
            ("Age", 110),
            ("Created", 150),
            ("Modified", 150),
            ("Status", 80),
            ("ID", 460),
        ];
        for (i, (name, width)) in cols.iter().enumerate() {
            self.list.insert_column(nwg::InsertListViewColumn {
                index: Some(i32::try_from(i).unwrap_or(i32::MAX)),
                text: Some((*name).into()),
                width: Some(*width),
                fmt: None,
            });
        }
        self.list.set_headers_enabled(true);
    }

    /// Spawn a worker thread that enumerates the layers directory and sends
    /// one `ScanLayer` message per layer so the GUI can populate incrementally.
    fn start_scan(&self) {
        if self.state.borrow().job != Job::Idle {
            return;
        }
        let dir = self.state.borrow().layers_dir.clone();
        let (tx, rx) = channel::<WorkerMsg>();
        let sender = self.worker_notice.sender();

        {
            let mut st = self.state.borrow_mut();
            st.layers.clear();
            st.receiver = Some(rx);
            st.cancel = None;
            st.job = Job::Scanning;
        }

        self.list.clear();
        self.btn_destroy.set_enabled(false);
        self.btn_refresh.set_enabled(false);
        self.set_status(&format!("Scanning layers in {}...", dir.display()));

        thread::spawn(move || {
            let entries = match std::fs::read_dir(&dir) {
                Ok(e) => e,
                Err(e) => {
                    let _ = tx.send(WorkerMsg::ScanError(format!(
                        "Could not read {}: {e}",
                        dir.display()
                    )));
                    sender.notice();
                    let _ = tx.send(WorkerMsg::ScanFinished);
                    sender.notice();
                    return;
                }
            };

            let in_use = crate::docker::in_use_layer_dirs();
            for entry in entries.flatten() {
                let path = entry.path();
                if !path.is_dir() {
                    continue;
                }
                let id = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .unwrap_or("?")
                    .to_string();
                let mut info = layers::stat_layer(path, id);
                info.orphan = !in_use.contains(&info.path);
                let _ = tx.send(WorkerMsg::ScanLayer(Box::new(info)));
                sender.notice();
            }
            let _ = tx.send(WorkerMsg::ScanFinished);
            sender.notice();
        });
    }

    fn append_layer_row(&self, info: &LayerInfo, now: SystemTime) {
        let row_idx = self.state.borrow().layers.len();
        self.list.insert_items_row(
            Some(i32::try_from(row_idx).unwrap_or(i32::MAX)),
            &[
                fmt_bytes(info.size_bytes),
                info.file_count.to_string(),
                fmt_age(info.age(now)),
                format_timestamp(info.created_at),
                format_timestamp(info.last_modified),
                if info.orphan {
                    "orphan".into()
                } else {
                    "in use".into()
                },
                info.id.clone(),
            ],
        );
    }

    fn update_info(&self) {
        let state = self.state.borrow();
        let selected = self.list.selected_items();
        let sel_size: u64 = selected
            .iter()
            .filter_map(|&i| state.layers.get(i).map(|l| l.size_bytes))
            .sum();
        let total_size: u64 = state.layers.iter().map(|l| l.size_bytes).sum();
        let orphan_size: u64 = state
            .layers
            .iter()
            .filter(|l| l.orphan)
            .map(|l| l.size_bytes)
            .sum();
        let orphan_count = state.layers.iter().filter(|l| l.orphan).count();

        self.info_label.set_text(&format!(
            "Layers: {}    Orphans: {} ({})    On disk: {}    Selected: {} ({})",
            state.layers.len(),
            orphan_count,
            fmt_bytes(orphan_size),
            fmt_bytes(total_size),
            selected.len(),
            fmt_bytes(sel_size),
        ));

        // Don't fight an in-progress job when toggling enabled.
        let busy = state.job != Job::Idle;
        self.btn_destroy.set_enabled(!busy && !selected.is_empty());
    }

    fn set_status(&self, text: &str) {
        self.status.set_text(0, text);
    }

    fn confirm_ok_cancel(&self, title: &str, content: &str, icon: nwg::MessageIcons) -> bool {
        let params = nwg::MessageParams {
            title,
            content,
            buttons: nwg::MessageButtons::OkCancel,
            icons: icon,
        };
        nwg::modal_message(&self.window, &params) == nwg::MessageChoice::Ok
    }

    fn message_info(&self, text: &str) {
        self.msg(text, nwg::MessageIcons::Info);
    }
    fn message_warning(&self, text: &str) {
        self.msg(text, nwg::MessageIcons::Warning);
    }
    fn message_error(&self, text: &str) {
        self.msg(text, nwg::MessageIcons::Error);
    }
    fn msg(&self, text: &str, icon: nwg::MessageIcons) {
        let params = nwg::MessageParams {
            title: "dcmfree",
            content: text,
            buttons: nwg::MessageButtons::Ok,
            icons: icon,
        };
        let _ = nwg::modal_message(&self.window, &params);
    }
}

/// Scrollable report dialog shown at the end of a destroy run.
///
/// Each row of the inner `ListView` is `outcome` / `size` / `id`. A footer
/// summarises totals.
#[derive(Default, NwgUi)]
pub struct ReportDialogUi {
    #[nwg_control(
        size: (820, 540),
        title: "dcmfree - destroy report",
        flags: "WINDOW|VISIBLE"
    )]
    #[nwg_events(OnWindowClose: [ReportDialogUi::on_close])]
    window: nwg::Window,

    #[nwg_control(
        parent: window,
        text: "",
        position: (10, 10),
        size: (800, 24)
    )]
    summary: nwg::Label,

    #[nwg_control(
        parent: window,
        position: (10, 40),
        size: (800, 440),
        list_style: nwg::ListViewStyle::Detailed,
        ex_flags: nwg::ListViewExFlags::FULL_ROW_SELECT
                | nwg::ListViewExFlags::GRID
    )]
    list: nwg::ListView,

    #[nwg_control(parent: window, text: "Close", position: (720, 490), size: (90, 32))]
    #[nwg_events(OnButtonClick: [ReportDialogUi::on_close])]
    btn_close: nwg::Button,
}

impl ReportDialogUi {
    fn on_close(&self) {
        // Hide instead of stop_thread_dispatch — the outer dcmfree event
        // loop must keep running. The leaked UI struct keeps the underlying
        // window handle valid until process exit (acceptable: one-shot per
        // destroy run, OS reclaims on exit).
        self.window.set_visible(false);
    }
}

struct ReportDialog;

impl ReportDialog {
    fn show(
        results: &[OutcomeRow],
        destroyed: usize,
        failed: usize,
        cancelled: usize,
        bytes_freed: u64,
    ) {
        // Leak intentionally: the dialog must outlive this function (it's
        // user-dismissed), and we don't want to nest dispatch loops. One
        // leak per destroy run; the OS cleans up at process exit.
        let ui: &'static ReportDialogUi = Box::leak(Box::new(
            ReportDialogUi::build_ui(ReportDialogUi::default())
                .expect("failed to build report dialog UI"),
        ));

        ui.list.insert_column(nwg::InsertListViewColumn {
            index: Some(0),
            text: Some("Outcome".into()),
            width: Some(110),
            fmt: None,
        });
        ui.list.insert_column(nwg::InsertListViewColumn {
            index: Some(1),
            text: Some("Size".into()),
            width: Some(100),
            fmt: None,
        });
        ui.list.insert_column(nwg::InsertListViewColumn {
            index: Some(2),
            text: Some("ID / detail".into()),
            width: Some(560),
            fmt: None,
        });
        ui.list.set_headers_enabled(true);

        for (i, r) in results.iter().enumerate() {
            let (outcome, detail) = match &r.outcome {
                OutcomeKind::Destroyed => ("destroyed".to_string(), r.id.clone()),
                OutcomeKind::Failed(why) => ("failed".to_string(), format!("{} - {why}", r.id)),
                OutcomeKind::Cancelled => ("cancelled".to_string(), r.id.clone()),
            };
            let row = i32::try_from(i).unwrap_or(i32::MAX);
            ui.list
                .insert_items_row(Some(row), &[outcome, fmt_bytes(r.size_bytes), detail]);
        }

        ui.summary.set_text(&format!(
            "Destroyed: {destroyed}   Failed: {failed}   Cancelled: {cancelled}   Freed: {}",
            fmt_bytes(bytes_freed)
        ));
    }
}

fn format_timestamp(ts: SystemTime) -> String {
    let dt: DateTime<Local> = ts.into();
    dt.format("%Y-%m-%d %H:%M").to_string()
}

/// Public entry point — initialise nwg and run the message loop.
pub fn run() -> anyhow::Result<()> {
    nwg::init().map_err(|e| anyhow::anyhow!("nwg init failed: {e}"))?;
    let _ = nwg::Font::set_global_family("Segoe UI");
    // `build_ui` already returns a struct that owns an `Rc<App>` internally;
    // we just need to keep it alive for the lifetime of the message loop.
    let _app =
        App::build_ui(App::default()).map_err(|e| anyhow::anyhow!("UI build failed: {e}"))?;
    nwg::dispatch_thread_events();
    Ok(())
}
