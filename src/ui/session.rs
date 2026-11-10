//! Interactive TUI modal & write-path session controller (`EPIC-08`..`EPIC-10`).
//!
//! Coordinates modal state (`TaskForm`, `ConfirmDialog`, `DestinationPicker`,
//! `CommentsModal`, `ErrorsModal`, `StructureForm`, inline quick priority/due
//! prompts), incremental `/` [`SearchOverlay`], read-only saved filter guards,
//! transient error toasts, and atomic SQLite mutations + background sync triggers.

#![allow(dead_code)]

use std::collections::HashMap;
use std::time::{Duration, SystemTime};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;
use rusqlite::Connection;

use crate::api::model::Id;
use crate::db::queries;
use crate::error::{AppError, Result};
use crate::queue::store::CommandStore;
use crate::search::query::SearchItem;
use crate::ui::actions::dispatch::dispatch;
use crate::ui::actions::reorder::ReorderDirection;
use crate::ui::actions::{add, comment, complete, edit, errors, move_task, reorder};
use crate::ui::app::App;
use crate::ui::data::SqliteViewData;
use crate::ui::filters_view::allow_action_in_view;
use crate::ui::keymap::{Action, Keymap};
use crate::ui::model::SidebarKind;
use crate::ui::search::{
    apply_accept, apply_cancel, load_search_items_from_db, render as render_search, SearchOverlay,
    SearchUiEvent,
};
use crate::ui::status::StatusModel;
use crate::ui::structure::{
    error_to_toast, execute_confirm, prompt_for_action, submit_form, StructureConfirmAction,
    StructureForm, StructureFormEvent, StructurePrompt,
};
use crate::ui::theme::Theme;
use crate::ui::view::{self, layout};
use crate::ui::widgets::{
    CommentsEvent, CommentsModal, ConfirmDialog, ConfirmEvent, DestinationPicker, ErrorToast,
    ErrorsEvent, ErrorsModal, FormEvent, PickerEvent, TaskForm,
};

const TOAST_TTL: Duration = Duration::from_secs(4);

/// Active overlay modal inside an interactive TUI session.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum ActiveModal {
    TaskForm {
        form: TaskForm,
        is_edit: bool,
    },
    ConfirmTaskDelete {
        task_id: Id,
        dialog: ConfirmDialog,
    },
    MovePicker {
        task_id: Id,
        picker: DestinationPicker,
    },
    Comments(CommentsModal),
    Errors(ErrorsModal),
    QuickPriority {
        task_id: Id,
    },
    QuickDue {
        task_id: Id,
        buf: String,
    },
    StructureForm(StructureForm),
    StructureConfirm {
        action: StructureConfirmAction,
        preferred_parent_id: Option<Id>,
        dialog: ConfirmDialog,
    },
}

/// Interactive state bridging [`App`], active modals/search, SQLite cache, and [`StatusModel`].
#[derive(Debug, Clone, Default)]
pub struct InteractiveState {
    pub modal: Option<ActiveModal>,
    pub search: Option<SearchOverlay>,
    pub toast: Option<ErrorToast>,
}

impl InteractiveState {
    /// Renders base frame plus active search overlay, centered modal, and toast.
    pub fn render_frame(
        &self,
        frame: &mut Frame<'_>,
        app: &App,
        status: &StatusModel,
        keymap: &Keymap,
        now: SystemTime,
        theme: &Theme,
    ) {
        view::render_frame(frame, app, status, keymap, now, theme);
        let area = frame.area();
        if layout::is_too_small(area) {
            return;
        }

        if let Some(ref overlay) = self.search {
            let list_area = layout::split(area).list;
            render_search(frame, list_area, overlay, theme);
        }

        if let Some(ref modal) = self.modal {
            render_modal(frame, area, modal, theme);
        }

        if let Some(ref toast) = self.toast {
            if !toast.is_expired(now) && area.height >= 4 {
                let toast_area = Rect::new(area.x + 1, area.height - 2, area.width - 2, 1);
                frame.render_widget(Clear, toast_area);
                let p = Paragraph::new(Line::from(Span::styled(
                    format!(" {} ", toast.message),
                    theme.status_error.add_modifier(Modifier::BOLD),
                )));
                frame.render_widget(p, toast_area);
            }
        }
    }

    /// Shows a short-lived status/error toast.
    pub fn notify(&mut self, msg: impl Into<String>, now: SystemTime) {
        self.toast = Some(ErrorToast::new(msg, now, TOAST_TTL));
    }

    /// Surfaces an [`AppError`] via transient toast.
    pub fn notify_error(&mut self, err: &AppError, now: SystemTime) {
        self.toast = Some(error_to_toast(err, now));
    }
}

/// Synchronizes queue failed/pending counts from `conn` into `status`.
pub fn refresh_queue_counts(conn: &Connection, status: &mut StatusModel) {
    if let Ok(counts) = CommandStore::new(conn).count_by_state() {
        status.pending_count = usize::try_from(counts.pending).unwrap_or(usize::MAX);
        status.failed_count = usize::try_from(counts.failed).unwrap_or(usize::MAX);
    }
}

/// Processes one key event against a live SQLite connection (`conn`), updating `app`,
/// `session`, and `status`. Returns `(needs_redraw, should_trigger_sync)`.
///
/// # Errors
///
/// Propagates unrecoverable data refresh errors.
#[allow(clippy::too_many_arguments)]
pub fn handle_live_key(
    key: KeyEvent,
    conn: &mut Connection,
    today_date: &str,
    authenticated: bool,
    app: &mut App,
    session: &mut InteractiveState,
    status: &mut StatusModel,
    keymap: &Keymap,
) -> Result<(bool, bool)> {
    let now = SystemTime::now();
    if app.show_help {
        app.dismiss_help();
        return Ok((true, false));
    }

    if session.modal.is_some() {
        let trigger = handle_modal_key(key, conn, today_date, authenticated, app, session, status)?;
        refresh_queue_counts(conn, status);
        return Ok((true, trigger));
    }

    if let Some(mut overlay) = session.search.take() {
        let ev = overlay.handle_key(key, keymap);
        let data = SqliteViewData::new(conn, today_date);
        match ev {
            SearchUiEvent::Pending => {
                session.search = Some(overlay);
            }
            SearchUiEvent::Cancelled(snap) => {
                apply_cancel(app, &snap, &data)?;
            }
            SearchUiEvent::JumpToTask {
                project_id,
                task_id,
            } => {
                apply_accept(app, &project_id, &task_id, &data)?;
            }
        }
        return Ok((true, false));
    }

    let Some(action) = keymap.lookup(key) else {
        return Ok((false, false));
    };

    if !allow_action_in_view(app.active_view(), action) {
        session.notify("Filter view is read-only", now);
        return Ok((true, false));
    }

    let trigger = dispatch_normal_action(
        action,
        conn,
        today_date,
        authenticated,
        app,
        session,
        status,
    )?;
    refresh_queue_counts(conn, status);
    Ok((true, trigger))
}

/// Opens read-only/fixture-backed [`SearchOverlay`] when `/` is pressed without DB access.
#[must_use]
pub fn open_fixture_search(app: &App) -> SearchOverlay {
    let mut project_names = HashMap::new();
    for node in app.sidebar_nodes() {
        if let SidebarKind::Project { id, .. } = &node.kind {
            project_names.insert(id.as_str().to_string(), node.label.clone());
        }
    }
    let items: Vec<SearchItem> = app
        .tasks()
        .iter()
        .map(|t| SearchItem {
            id: t.id.as_str().to_string(),
            content: t.content.clone(),
            description: String::new(),
            project_id: t.project_id.as_str().to_string(),
            section_id: t.section_id.as_ref().map(|s| s.as_str().to_string()),
        })
        .collect();
    SearchOverlay::open(app, items, project_names)
}

#[allow(clippy::too_many_lines)]
fn dispatch_normal_action(
    action: Action,
    conn: &mut Connection,
    today_date: &str,
    authenticated: bool,
    app: &mut App,
    session: &mut InteractiveState,
    _status: &mut StatusModel,
) -> Result<bool> {
    let now = SystemTime::now();
    match action {
        Action::OpenSearch => {
            let (items, project_names) = load_search_items_from_db(conn)?;
            app.search_active = true;
            session.search = Some(SearchOverlay::open(app, items, project_names));
            Ok(false)
        }
        Action::ToggleHelp
        | Action::Quit
        | Action::SyncNow
        | Action::Down
        | Action::Up
        | Action::First
        | Action::Last
        | Action::Left
        | Action::Right
        | Action::ToggleSearchScope => {
            let data = SqliteViewData::new(conn, today_date);
            app.apply(action, &data)?;
            Ok(app.take_sync_triggered())
        }
        Action::AddTask | Action::AddSubtask => {
            let inbox_id = resolve_inbox_id(conn);
            let form = add::open_add_form(
                app.active_view(),
                &inbox_id,
                app.selected_task(),
                action == Action::AddSubtask,
            );
            session.modal = Some(ActiveModal::TaskForm {
                form,
                is_edit: false,
            });
            Ok(false)
        }
        Action::EditTask => {
            if let Some(detail) = app.detail() {
                session.modal = Some(ActiveModal::TaskForm {
                    form: edit::open_edit_form(detail),
                    is_edit: true,
                });
            }
            Ok(false)
        }
        Action::QuickPriority => {
            if let Some(task) = app.selected_task() {
                session.modal = Some(ActiveModal::QuickPriority {
                    task_id: task.id.clone(),
                });
            }
            Ok(false)
        }
        Action::QuickDue => {
            if let Some(task) = app.selected_task() {
                let prefill = task
                    .due
                    .as_ref()
                    .and_then(|d| d.string.clone().or_else(|| Some(d.date.clone())))
                    .unwrap_or_default();
                session.modal = Some(ActiveModal::QuickDue {
                    task_id: task.id.clone(),
                    buf: prefill,
                });
            }
            Ok(false)
        }
        Action::ToggleComplete => {
            if let Some(task) = app.selected_task().cloned() {
                match complete::toggle_complete(conn, authenticated, task.id, task.checked) {
                    Ok(_) => {
                        let data = SqliteViewData::new(conn, today_date);
                        app.refresh(&data)?;
                        return Ok(true);
                    }
                    Err(err) => session.notify_error(&err, now),
                }
            }
            Ok(false)
        }
        Action::DeleteTask => {
            if let Some(task) = app.selected_task() {
                session.modal = Some(ActiveModal::ConfirmTaskDelete {
                    task_id: task.id.clone(),
                    dialog: complete::open_delete_confirm(&task.content),
                });
            }
            Ok(false)
        }
        Action::MoveTask => {
            if let Some(task) = app.selected_task() {
                match move_task::build_move_picker(conn, &task.id) {
                    Ok(picker) => {
                        session.modal = Some(ActiveModal::MovePicker {
                            task_id: task.id.clone(),
                            picker,
                        });
                    }
                    Err(err) => session.notify_error(&err, now),
                }
            }
            Ok(false)
        }
        Action::ReorderUp | Action::ReorderDown => {
            let dir = if action == Action::ReorderUp {
                ReorderDirection::Up
            } else {
                ReorderDirection::Down
            };
            if let Some(task) = app.selected_task().cloned() {
                match reorder::reorder_task(conn, authenticated, &task.id, dir) {
                    Ok(Some(_)) => {
                        let data = SqliteViewData::new(conn, today_date);
                        let current_view = app.active_view().clone();
                        app.select_view_and_task(&current_view, Some(&task.id), &data)?;
                        return Ok(true);
                    }
                    Ok(None) => {}
                    Err(err) => session.notify_error(&err, now),
                }
            }
            Ok(false)
        }
        Action::Comments => {
            if let Some(task) = app.selected_task() {
                match comment::open_comments_modal(conn, task.id.clone(), &task.content) {
                    Ok(modal) => session.modal = Some(ActiveModal::Comments(modal)),
                    Err(err) => session.notify_error(&err, now),
                }
            }
            Ok(false)
        }
        Action::ViewErrors => {
            match errors::open_errors_modal(conn) {
                Ok(modal) => session.modal = Some(ActiveModal::Errors(modal)),
                Err(err) => session.notify_error(&err, now),
            }
            Ok(false)
        }
        Action::StructureNew | Action::StructureEdit | Action::StructureDelete => {
            match prompt_for_action(conn, app, action) {
                Ok(StructurePrompt::Ignored) => {}
                Ok(StructurePrompt::Form(form)) => {
                    session.modal = Some(ActiveModal::StructureForm(form));
                }
                Ok(StructurePrompt::Confirm {
                    action: confirm_act,
                    dialog,
                }) => {
                    let parent_id = if let Some(node) = app.selected_sidebar_node() {
                        if let SidebarKind::Project { parent_id, .. } = &node.kind {
                            parent_id.clone()
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    session.modal = Some(ActiveModal::StructureConfirm {
                        action: confirm_act,
                        preferred_parent_id: parent_id,
                        dialog,
                    });
                }
                Err(err) => session.notify_error(&err, now),
            }
            Ok(false)
        }
    }
}

#[allow(clippy::too_many_lines)]
fn handle_modal_key(
    key: KeyEvent,
    conn: &mut Connection,
    today_date: &str,
    authenticated: bool,
    app: &mut App,
    session: &mut InteractiveState,
    status: &mut StatusModel,
) -> Result<bool> {
    let now = SystemTime::now();
    let Some(modal) = session.modal.take() else {
        return Ok(false);
    };

    match modal {
        ActiveModal::TaskForm { mut form, is_edit } => match form.handle_key(key) {
            FormEvent::Continue => {
                session.modal = Some(ActiveModal::TaskForm { form, is_edit });
                Ok(false)
            }
            FormEvent::Cancel | FormEvent::Submit(None) => Ok(false),
            FormEvent::Submit(Some(act)) => match dispatch(conn, authenticated, &act) {
                Ok(_) => {
                    let data = SqliteViewData::new(conn, today_date);
                    app.refresh(&data)?;
                    Ok(true)
                }
                Err(err) => {
                    form.error = Some(err.to_string());
                    session.modal = Some(ActiveModal::TaskForm { form, is_edit });
                    Ok(false)
                }
            },
        },
        ActiveModal::ConfirmTaskDelete {
            task_id,
            mut dialog,
        } => match dialog.handle_key(key) {
            ConfirmEvent::Pending => {
                session.modal = Some(ActiveModal::ConfirmTaskDelete { task_id, dialog });
                Ok(false)
            }
            ConfirmEvent::Cancelled => Ok(false),
            ConfirmEvent::Confirmed => {
                match complete::confirm_delete(conn, authenticated, task_id) {
                    Ok(_) => {
                        let data = SqliteViewData::new(conn, today_date);
                        app.refresh(&data)?;
                        Ok(true)
                    }
                    Err(err) => {
                        session.notify_error(&err, now);
                        Ok(false)
                    }
                }
            }
        },
        ActiveModal::MovePicker {
            task_id,
            mut picker,
        } => match picker.handle_key(key) {
            PickerEvent::Pending => {
                session.modal = Some(ActiveModal::MovePicker { task_id, picker });
                Ok(false)
            }
            PickerEvent::Cancelled => Ok(false),
            PickerEvent::Selected(target) => {
                match move_task::move_task(conn, authenticated, task_id.clone(), target) {
                    Ok(_) => {
                        let data = SqliteViewData::new(conn, today_date);
                        app.refresh(&data)?;
                        Ok(true)
                    }
                    Err(err) => {
                        picker.error = Some(err.to_string());
                        session.modal = Some(ActiveModal::MovePicker { task_id, picker });
                        Ok(false)
                    }
                }
            }
        },
        ActiveModal::Comments(mut comments) => match comments.handle_key(key) {
            CommentsEvent::Pending => {
                session.modal = Some(ActiveModal::Comments(comments));
                Ok(false)
            }
            CommentsEvent::Close => Ok(false),
            CommentsEvent::AddComment { item_id, content } => {
                match comment::submit_comment(conn, authenticated, item_id.clone(), &content) {
                    Ok(_) => {
                        let refreshed =
                            comment::open_comments_modal(conn, item_id, &comments.task_title)
                                .unwrap_or(comments);
                        session.modal = Some(ActiveModal::Comments(refreshed));
                        let data = SqliteViewData::new(conn, today_date);
                        app.refresh(&data)?;
                        Ok(true)
                    }
                    Err(err) => {
                        comments.error = Some(err.to_string());
                        session.modal = Some(ActiveModal::Comments(comments));
                        Ok(false)
                    }
                }
            }
        },
        ActiveModal::Errors(mut err_modal) => match err_modal.handle_key(key) {
            ErrorsEvent::Pending => {
                session.modal = Some(ActiveModal::Errors(err_modal));
                Ok(false)
            }
            ErrorsEvent::Close => Ok(false),
            ErrorsEvent::Dismiss(uuid) => {
                match errors::dismiss_failed_command(conn, &uuid) {
                    Ok(remaining) => {
                        err_modal.items = remaining;
                        if !err_modal.items.is_empty()
                            && err_modal.selected >= err_modal.items.len()
                        {
                            err_modal.selected = err_modal.items.len() - 1;
                        }
                        refresh_queue_counts(conn, status);
                    }
                    Err(err) => session.notify_error(&err, now),
                }
                session.modal = Some(ActiveModal::Errors(err_modal));
                Ok(false)
            }
        },
        ActiveModal::QuickPriority { task_id } => match key.code {
            KeyCode::Esc => Ok(false),
            KeyCode::Char(c @ '1'..='4') => {
                let p = u8::try_from(c.to_digit(10).unwrap_or(1)).unwrap_or(1);
                match edit::quick_priority(conn, authenticated, task_id, p) {
                    Ok(_) => {
                        let data = SqliteViewData::new(conn, today_date);
                        app.refresh(&data)?;
                        Ok(true)
                    }
                    Err(err) => {
                        session.notify_error(&err, now);
                        Ok(false)
                    }
                }
            }
            _ => {
                session.modal = Some(ActiveModal::QuickPriority { task_id });
                Ok(false)
            }
        },
        ActiveModal::QuickDue { task_id, mut buf } => match key.code {
            KeyCode::Esc => Ok(false),
            KeyCode::Backspace => {
                buf.pop();
                session.modal = Some(ActiveModal::QuickDue { task_id, buf });
                Ok(false)
            }
            KeyCode::Enter => match edit::quick_due(conn, authenticated, task_id, &buf) {
                Ok(_) => {
                    let data = SqliteViewData::new(conn, today_date);
                    app.refresh(&data)?;
                    Ok(true)
                }
                Err(err) => {
                    session.notify_error(&err, now);
                    Ok(false)
                }
            },
            KeyCode::Char(c) => {
                buf.push(c);
                session.modal = Some(ActiveModal::QuickDue { task_id, buf });
                Ok(false)
            }
            _ => {
                session.modal = Some(ActiveModal::QuickDue { task_id, buf });
                Ok(false)
            }
        },
        ActiveModal::StructureForm(mut form) => match form.handle_key(key) {
            StructureFormEvent::Pending => {
                session.modal = Some(ActiveModal::StructureForm(form));
                Ok(false)
            }
            StructureFormEvent::Cancelled => Ok(false),
            StructureFormEvent::Submit(intent) => {
                if submit_form(conn, app, today_date, &mut form, &intent)? {
                    Ok(true)
                } else {
                    session.modal = Some(ActiveModal::StructureForm(form));
                    Ok(false)
                }
            }
        },
        ActiveModal::StructureConfirm {
            action,
            preferred_parent_id,
            mut dialog,
        } => match dialog.handle_key(key) {
            ConfirmEvent::Pending => {
                session.modal = Some(ActiveModal::StructureConfirm {
                    action,
                    preferred_parent_id,
                    dialog,
                });
                Ok(false)
            }
            ConfirmEvent::Cancelled => Ok(false),
            ConfirmEvent::Confirmed => {
                match execute_confirm(conn, app, today_date, &action, preferred_parent_id.as_ref())
                {
                    Ok(_) => Ok(true),
                    Err(err) => {
                        session.notify_error(&err, now);
                        Ok(false)
                    }
                }
            }
        },
    }
}

fn render_modal(frame: &mut Frame<'_>, area: Rect, modal: &ActiveModal, theme: &Theme) {
    match modal {
        ActiveModal::TaskForm { form, .. } => form.render(frame, area, theme),
        ActiveModal::ConfirmTaskDelete { dialog, .. }
        | ActiveModal::StructureConfirm { dialog, .. } => dialog.render(frame, area, theme),
        ActiveModal::MovePicker { picker, .. } => picker.render(frame, area, theme),
        ActiveModal::Comments(modal) => modal.render(frame, area, theme),
        ActiveModal::Errors(modal) => modal.render(frame, area, theme),
        ActiveModal::StructureForm(form) => form.render(frame, area, theme),
        ActiveModal::QuickPriority { .. } => {
            render_mini_prompt(
                frame,
                area,
                " Quick Priority ",
                "Press 1 (P4), 2 (P3), 3 (P2), 4 (P1) or Esc to cancel",
                theme,
            );
        }
        ActiveModal::QuickDue { buf, .. } => {
            render_mini_prompt(
                frame,
                area,
                " Quick Due Date ",
                &format!("Due: {buf}_  (Enter save, empty clears, Esc cancel)"),
                theme,
            );
        }
    }
}

fn render_mini_prompt(frame: &mut Frame<'_>, area: Rect, title: &str, body: &str, theme: &Theme) {
    if area.width < 20 || area.height < 5 {
        return;
    }
    let w = 60u16.min(area.width.saturating_sub(4));
    let h = 3u16;
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect::new(x, y, w, h);
    frame.render_widget(Clear, rect);
    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(theme.border_focused)
        .style(theme.help_overlay);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(body, theme.list_row))),
        inner,
    );
}

fn resolve_inbox_id(conn: &Connection) -> Id {
    if let Ok(projects) = queries::all_projects(conn) {
        if let Some(inbox) = projects
            .into_iter()
            .find(|p| p.name.eq_ignore_ascii_case("inbox"))
        {
            return inbox.id;
        }
    }
    Id::new("inbox")
}
